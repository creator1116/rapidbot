//! Play state (`ClientPacketListener`), main-thread half.

use rand::Rng;
use rapidbot_buf::{ByteArray, Decode, VarInt};
use rapidbot_physics::attributes::Operation;
use rapidbot_physics::math::Vec3;
use rapidbot_protocol::ids::play::clientbound as ids;
use rapidbot_protocol::packets::play::{GameType, clientbound as cb, serverbound as sb};
use rapidbot_protocol::{Packet, RawPacket, State};
use rapidbot_world::chunk::Chunk;
use rapidbot_world::tags::Tags;
use rapidbot_world::{DimensionHeight, Registry, World};
use tracing::{debug, info, warn};

use super::{Game, LocalPlayer};
use crate::net::PacketSender;
use crate::{ClientError, SessionData, clock};

/// `LevelLoadTracker.CLIENT_WAIT_TIMEOUT_MS`.
const CLIENT_WAIT_TIMEOUT_MS: i64 = 30_000;

/// `LevelLoadTracker.ClientState`.
#[derive(Debug, Clone, Copy)]
enum LoadState {
    WaitingForServer {
        timeout_after: i64,
    },
    /// Waiting for the render section around the camera to be compiled.
    /// That needs the camera's chunk and its neighbours, then a frame or two
    /// on the render thread; `ready_in_frames` models the latter.
    WaitingForPlayerChunk {
        timeout_after: i64,
        ready_in_frames: Option<u32>,
        section_ready: bool,
    },
    Ready {
        ready_at: i64,
    },
}

pub(super) struct PlayState {
    pub player: LocalPlayer,
    pub game_type: GameType,
    pub world: World,
    pub entities: crate::entities::Entities,
    /// The server's tags (the client uses these, not its built-in ones).
    pub tags: Tags,
    /// Size of `minecraft:worldgen/biome`, which fixes the width of global
    /// biome palettes in chunk data.
    biome_count: usize,
    load: Option<LoadState>,
    pub client_loaded: bool,
    /// `deathTime`: ticks since dying. At 20 the player entity is removed
    /// and stops ticking; the death screen's buttons unlock at 20 too.
    pub death_time: u32,
    /// Ticks until the "Respawn" click.
    pub respawn_in: Option<u32>,
    dimension: String,
    /// `keyPairFuture`: set on an online-mode login when the account has
    /// chat keys, sent (as `setKeyPair` does) on the next listener tick.
    pending_chat_session: Option<sb::ChatSessionUpdate>,
    /// Signed-chat bookkeeping (`lastSeenMessages`, `messageSignatureCache`,
    /// `signedMessageEncoder`).
    chat: crate::chat::ChatState,
    /// The server's command tree, for telling which commands get signed.
    commands: crate::commands::CommandTree,
    pub inventory: crate::inventory::Inventory,
    /// `MultiPlayerGameMode.carriedIndex`: the selected slot the server
    /// knows about.
    carried_index: u8,
    pub game_mode: crate::interact::GameMode,
}

impl PlayState {
    fn new(
        login: &cb::Login,
        chat_keys: Option<&rapidbot_auth::ChatKeys>,
        session: &SessionData,
    ) -> Self {
        // handleLogin: `if (packet.onlineMode()) this.prepareKeyPair();`
        let mut chat = crate::chat::ChatState::new();
        let pending_chat_session = chat_keys.filter(|_| login.online_mode).and_then(|keys| {
            let profile = session.profile.as_ref()?.id;
            let session_id = chat
                .start_session(profile, &keys.private_key_pem)
                .or_else(|| {
                    warn!("could not read the chat signing key; chat will be unsigned");
                    None
                })?;
            Some(sb::ChatSessionUpdate {
                session_id,
                expires_at: keys.expires_at,
                public_key: ByteArray(keys.public_key_der.clone()),
                key_signature: ByteArray(keys.signature_v2.clone()),
            })
        });
        let mut tags = Tags::default();
        for payload in &session.tags {
            if let Err(e) = tags.apply(payload) {
                warn!("bad tags payload: {e}");
            }
        }
        let height = dimension_height(session, login.spawn.dimension_type);
        let mut player = LocalPlayer::new(login.player_id);
        player.physics.spectator = login.spawn.game_type == GameType::Spectator;
        player.physics.fast_lava = fast_lava(session, login.spawn.dimension_type);
        let mut state = Self {
            player,
            game_type: login.spawn.game_type,
            world: World::new(height),
            entities: Default::default(),
            tags,
            biome_count: registry_len(session, "minecraft:worldgen/biome"),
            load: None,
            client_loaded: false,
            death_time: 0,
            respawn_in: None,
            dimension: login.spawn.dimension.to_string(),
            pending_chat_session,
            chat,
            commands: Default::default(),
            inventory: Default::default(),
            carried_index: 0,
            game_mode: crate::interact::GameMode::new(),
        };
        state.start_waiting_for_new_level();
        state
    }

    /// `ChatScreen.handleChatInput` for an already normalized line:
    /// `sendCommand` for a slash command, `sendChat` otherwise.
    pub fn send_chat_line(&mut self, line: &str, net: &PacketSender) {
        use crate::chat::Outgoing;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let outgoing = match line.strip_prefix('/') {
            Some(command) => {
                let signable = self.commands.signable_arguments(command);
                self.chat.command(command, &signable, now)
            }
            None => self.chat.chat(line, now),
        };
        debug!("chat sent: {line}");
        match outgoing {
            Outgoing::Chat(packet) => net.send(&packet),
            Outgoing::Command(packet) => net.send(&packet),
            Outgoing::SignedCommand(packet) => net.send(&packet),
        }
    }

    /// `startWaitingForNewLevel` → `LevelLoadTracker.startClientLoad`.
    fn start_waiting_for_new_level(&mut self) {
        self.load = Some(LoadState::WaitingForServer {
            timeout_after: clock::millis() + CLIENT_WAIT_TIMEOUT_MS,
        });
    }

    /// `ClientPacketListener.tick`: chat session, then level loading.
    /// Returns true on the tick the client becomes loaded.
    /// `Minecraft.pick` and the attack button's share of `handleKeybinds`.
    pub fn attack_button(
        &mut self,
        net: &PacketSender,
        clicks: u32,
        down: bool,
        hit: &crate::interact::Pick,
    ) {
        let Self {
            player,
            world,
            inventory,
            entities,
            tags,
            game_mode,
            carried_index,
            game_type,
            ..
        } = self;
        let mut hands = crate::interact::Hands {
            player: &mut player.physics,
            world,
            inventory,
            entities,
            tags,
            net,
            restricted: matches!(game_type, GameType::Spectator | GameType::Adventure),
            survival: *game_type != GameType::Creative,
        };
        let selected = inventory.selected;
        game_mode.attack_button(&mut hands, clicks, down, hit, |net| {
            // ensureHasSentCarriedItem()
            if selected != *carried_index {
                *carried_index = selected;
                net.send(&sb::SetCarriedItem {
                    slot: selected as i16,
                });
            }
        });
    }

    pub fn listener_tick(&mut self, net: &PacketSender) -> bool {
        // gameMode.tick() starts with ensureHasSentCarriedItem().
        if self.inventory.selected != self.carried_index {
            self.carried_index = self.inventory.selected;
            net.send(&sb::SetCarriedItem {
                slot: self.carried_index as i16,
            });
        }
        if let Some(session) = self.pending_chat_session.take() {
            debug!("sending chat session");
            net.send(&session);
        }
        let Some(load) = self.load else { return false };
        let now = clock::millis();
        // tickClientLoad()
        let load = match load {
            LoadState::WaitingForPlayerChunk {
                timeout_after,
                section_ready,
                ..
            } => {
                if now > timeout_after {
                    warn!("timed out waiting for the player's chunk, loading anyway");
                    LoadState::Ready { ready_at: now }
                } else if section_ready {
                    LoadState::Ready { ready_at: now }
                } else {
                    load
                }
            }
            other => other,
        };
        self.load = Some(load);
        // isLevelReady(): close delay is 0 for a normal join.
        if let LoadState::Ready { ready_at } = load {
            if now >= ready_at {
                self.load = None;
                if !self.client_loaded {
                    net.send(&sb::PlayerLoaded);
                    self.client_loaded = true;
                    info!("player loaded");
                    return true;
                }
            }
        }
        false
    }

    /// Render-thread work that happens once per frame.
    pub fn on_frame(&mut self) {
        let Some(LoadState::WaitingForPlayerChunk {
            timeout_after,
            ready_in_frames,
            section_ready: false,
        }) = self.load
        else {
            return;
        };
        let ready_in_frames = match ready_in_frames {
            Some(0) => {
                self.load = Some(LoadState::WaitingForPlayerChunk {
                    timeout_after,
                    ready_in_frames: None,
                    section_ready: true,
                });
                return;
            }
            Some(n) => Some(n - 1),
            // The section can compile once the camera's chunk and all eight
            // neighbours are present.
            None if self.camera_neighbourhood_loaded() => Some(rand::thread_rng().gen_range(1..=3)),
            None => None,
        };
        self.load = Some(LoadState::WaitingForPlayerChunk {
            timeout_after,
            ready_in_frames,
            section_ready: false,
        });
    }

    fn camera_neighbourhood_loaded(&self) -> bool {
        let cx = crate::math::floor(self.player.physics.pos.x) >> 4;
        let cz = crate::math::floor(self.player.physics.pos.z) >> 4;
        (-1..=1).all(|dx| (-1..=1).all(|dz| self.world.has_chunk(cx + dx, cz + dz)))
    }
}

impl Game {
    pub(super) fn handle_play(&mut self, packet: RawPacket) -> Result<(), ClientError> {
        match packet.id {
            cb::Login::ID => {
                let login = packet.decode::<cb::Login>()?;
                info!(
                    entity_id = login.player_id,
                    dimension = %login.spawn.dimension,
                    game_type = ?login.spawn.game_type,
                    "joined level"
                );
                let play = PlayState::new(&login, self.config.account.chat_keys(), &self.session);
                self.key_intent = Default::default();
                self.link.emit(crate::BotEvent::Spawned {
                    entity_id: login.player_id,
                    dimension: login.spawn.dimension.to_string(),
                });
                self.aim = None;
                debug!(
                    min_y = play.world.height.min_y,
                    height = play.world.height.height,
                    "dimension"
                );
                self.play = Some(play);
            }
            cb::Ping::ID => {
                let id = packet.decode::<cb::Ping>()?.id;
                self.net.send(&sb::Pong { id });
            }
            cb::PlayerPosition::ID => {
                let pos = packet.decode::<cb::PlayerPosition>()?;
                let play = play_mut(&mut self.play)?;
                play.player.apply_teleport(&pos.change, pos.relatives);
                play.game_mode.on_teleport(&self.net);
                let p = &play.player.physics;
                debug!(
                    x = p.pos.x,
                    y = p.pos.y,
                    z = p.pos.z,
                    id = pos.id,
                    relatives = pos.relatives,
                    vy = p.delta_movement.y,
                    on_ground = p.on_ground,
                    "teleported"
                );
                self.net.send(&sb::AcceptTeleportation {
                    id: pos.id,
                    x: p.pos.x,
                    y: p.pos.y,
                    z: p.pos.z,
                    y_rot: p.y_rot,
                    x_rot: p.x_rot,
                });
            }
            cb::PlayerRotation::ID => {
                let rot = packet.decode::<cb::PlayerRotation>()?;
                let play = play_mut(&mut self.play)?;
                let p = &mut play.player.physics;
                p.y_rot = if rot.relative_y {
                    p.y_rot + rot.y_rot
                } else {
                    rot.y_rot
                };
                p.x_rot = crate::math::clamp_f32(
                    if rot.relative_x {
                        p.x_rot + rot.x_rot
                    } else {
                        rot.x_rot
                    },
                    -90.0,
                    90.0,
                );
                self.net.send(&sb::MovePlayerRot {
                    y_rot: p.y_rot,
                    x_rot: p.x_rot,
                    flags: 0,
                });
            }
            cb::GameEvent::ID => {
                let event = packet.decode::<cb::GameEvent>()?;
                let play = play_mut(&mut self.play)?;
                match event.event {
                    cb::GameEvent::LEVEL_CHUNKS_LOAD_START => {
                        if let Some(LoadState::WaitingForServer { timeout_after }) = play.load {
                            play.load = Some(LoadState::WaitingForPlayerChunk {
                                timeout_after,
                                ready_in_frames: None,
                                section_ready: false,
                            });
                        }
                    }
                    cb::GameEvent::CHANGE_GAME_MODE => {
                        let mut id: &[u8] = &[event.param as u8];
                        play.game_type = GameType::decode(&mut id)?;
                        play.player.physics.spectator = play.game_type == GameType::Spectator;
                    }
                    _ => {}
                }
            }
            cb::LevelChunkWithLight::ID => {
                let packet = packet.decode::<cb::LevelChunkWithLight>()?;
                let play = play_mut(&mut self.play)?;
                let chunk = Chunk::read_packet(
                    &packet.data.0,
                    Registry::get().state_count(),
                    play.biome_count,
                )?;
                play.world.insert_chunk(packet.x, packet.z, chunk);
            }
            cb::ForgetLevelChunk::ID => {
                let forget = packet.decode::<cb::ForgetLevelChunk>()?;
                play_mut(&mut self.play)?
                    .world
                    .remove_chunk(forget.x(), forget.z());
            }
            cb::BlockUpdate::ID => {
                let update = packet.decode::<cb::BlockUpdate>()?;
                let (x, y, z) = rapidbot_world::unpack_block_pos(update.pos);
                let play = play_mut(&mut self.play)?;
                // setServerVerifiedBlockState: a predicted block waits for its ack.
                let ours = play.game_mode.server_block((x, y, z), update.state as u32);
                if !ours {
                    play.world.set_block_state(x, y, z, update.state as u32);
                }
            }
            cb::BlockChangedAck::ID => {
                let sequence = packet.decode::<cb::BlockChangedAck>()?.sequence;
                let play = play_mut(&mut self.play)?;
                play.game_mode.block_changed_ack(
                    sequence,
                    &mut play.world,
                    &mut play.player.physics,
                );
            }
            cb::SectionBlocksUpdate::ID => {
                let update = packet.decode::<cb::SectionBlocksUpdate>()?;
                let (sx, sy, sz) = rapidbot_world::unpack_section_pos(update.section);
                let play = play_mut(&mut self.play)?;
                for change in update.updates {
                    let v = change.0;
                    let state = (v >> 12) as u32;
                    let rel = (v & 0xfff) as i32;
                    let (x, z, y) = (rel >> 8 & 15, rel >> 4 & 15, rel & 15);
                    let pos = (sx * 16 + x, sy * 16 + y, sz * 16 + z);
                    if play.game_mode.server_block(pos, state) {
                        // Our own predicted change coming back.
                        continue;
                    }
                    play.world.set_block_state(pos.0, pos.1, pos.2, state);
                }
            }
            cb::UpdateAttributes::ID => {
                let update = packet.decode::<cb::UpdateAttributes>()?;
                let play = play_mut(&mut self.play)?;
                if update.entity_id != play.player.id {
                    // Of other entities only the size matters to us.
                    for snapshot in update.attributes {
                        let Some(info) = Registry::get().attribute(snapshot.attribute as u32)
                        else {
                            continue;
                        };
                        if info.name != "minecraft:scale" {
                            continue;
                        }
                        let mut instance = rapidbot_physics::attributes::Instance::new(
                            snapshot.base,
                            info.min,
                            info.max,
                        );
                        for m in &snapshot.modifiers {
                            if let Some(op) = Operation::from_id(m.operation) {
                                instance.add_modifier(&m.id.normalized(), m.amount, op);
                            }
                        }
                        play.entities.set_scale(update.entity_id, instance.value());
                    }
                } else {
                    for snapshot in update.attributes {
                        let Some(info) = Registry::get().attribute(snapshot.attribute as u32)
                        else {
                            continue;
                        };
                        let modifiers: Vec<(String, f64, Operation)> = snapshot
                            .modifiers
                            .into_iter()
                            .filter_map(|m| {
                                Some((
                                    m.id.normalized().into_owned(),
                                    m.amount,
                                    Operation::from_id(m.operation)?,
                                ))
                            })
                            .collect();
                        play.player.physics.attributes.apply_snapshot(
                            &info.name,
                            snapshot.base,
                            &modifiers,
                        );
                    }
                }
            }
            cb::PlayerAbilities::ID => {
                let a = packet.decode::<cb::PlayerAbilities>()?;
                let play = play_mut(&mut self.play)?;
                let abilities = &mut play.player.physics.abilities;
                abilities.invulnerable = a.flags & 1 != 0;
                abilities.flying = a.flags & 2 != 0;
                abilities.may_fly = a.flags & 4 != 0;
                abilities.instabuild = a.flags & 8 != 0;
                abilities.flying_speed = a.flying_speed;
                abilities.walking_speed = a.walking_speed;
            }
            cb::SetHealth::ID => {
                let health = packet.decode::<cb::SetHealth>()?;
                let play = play_mut(&mut self.play)?;
                play.player.physics.food_level = health.food;
                let dead = health.health <= 0.0;
                if dead && !play.player.physics.dead {
                    info!("died");
                    play.death_time = 0;
                    if self.config.auto_respawn {
                        // The button unlocks after 20 ticks; then a person
                        // takes a moment to click it.
                        let delay = rapidbot_human::reaction_time(&mut self.noise, 1.4, 0.5, 0.0);
                        play.respawn_in = Some(20 + (delay * 20.0) as u32);
                    }
                    self.key_intent = Default::default();
                    self.aim = None;
                    self.link.emit(crate::BotEvent::Died);
                }
                play.player.physics.dead = dead;
            }
            cb::SetEntityMotion::ID => {
                let motion = packet.decode::<cb::SetEntityMotion>()?;
                let play = play_mut(&mut self.play)?;
                if motion.entity_id == play.player.id {
                    // Entity.lerpMotion: the velocity replaces ours.
                    let v = Vec3::new(motion.movement.x, motion.movement.y, motion.movement.z);
                    debug!(x = v.x, y = v.y, z = v.z, "velocity set");
                    play.player.physics.delta_movement = v;
                }
            }
            cb::Explode::ID => {
                let explode = packet.decode::<cb::Explode>()?;
                let play = play_mut(&mut self.play)?;
                if let Some(k) = explode.player_knockback {
                    let v = Vec3::new(k.x, k.y, k.z);
                    play.player.physics.delta_movement = play.player.physics.delta_movement.add(v);
                }
            }
            cb::PlayerLookAt::ID => {
                let look = packet.decode::<cb::PlayerLookAt>()?;
                let play = play_mut(&mut self.play)?;
                let p = &mut play.player.physics;
                p.look_at(look.from_anchor == 1, Vec3::new(look.x, look.y, look.z));
            }
            cb::SetHeldSlot::ID => {
                packet.decode::<cb::SetHeldSlot>()?;
                play_mut(&mut self.play)?
                    .inventory
                    .set_held_slot(&packet.body)?;
                self.slot_press = None;
            }
            cb::OpenScreen::ID => {
                play_mut(&mut self.play)?
                    .inventory
                    .open_screen(&packet.body)?;
            }
            ids::CONTAINER_CLOSE => {
                play_mut(&mut self.play)?.inventory.close_container();
                self.close_press = None;
            }
            ids::CONTAINER_SET_CONTENT => {
                if let Err(e) = play_mut(&mut self.play)?
                    .inventory
                    .set_content(&packet.body)
                {
                    warn!("could not read container contents: {e}");
                }
            }
            ids::SET_PLAYER_INVENTORY => {
                if let Err(e) = play_mut(&mut self.play)?
                    .inventory
                    .set_player_inventory(&packet.body)
                {
                    warn!("could not read an inventory slot: {e}");
                }
            }
            ids::SET_CURSOR_ITEM => {
                if let Err(e) = play_mut(&mut self.play)?.inventory.set_cursor(&packet.body) {
                    warn!("could not read the cursor item: {e}");
                }
            }
            cb::SystemChat::ID => {
                let chat = packet.decode::<cb::SystemChat>()?;
                let text = crate::text::nbt_to_plain(&chat.content);
                debug!(overlay = chat.overlay, "system chat: {text}");
                self.link.emit(crate::BotEvent::SystemMessage {
                    text,
                    overlay: chat.overlay,
                });
            }
            ids::ADD_ENTITY => {
                play_mut(&mut self.play)?
                    .entities
                    .add_entity(&packet.body)?;
            }
            ids::REMOVE_ENTITIES => play_mut(&mut self.play)?
                .entities
                .remove_entities(&packet.body)?,
            ids::MOVE_ENTITY_POS | ids::MOVE_ENTITY_POS_ROT => {
                play_mut(&mut self.play)?
                    .entities
                    .move_entity(&packet.body, packet.id == ids::MOVE_ENTITY_POS_ROT)?
            }
            ids::MOVE_ENTITY_ROT => play_mut(&mut self.play)?
                .entities
                .rotate_entity(&packet.body)?,
            ids::ENTITY_POSITION_SYNC => play_mut(&mut self.play)?
                .entities
                .position_sync(&packet.body)?,
            ids::TELEPORT_ENTITY => play_mut(&mut self.play)?
                .entities
                .teleport_entity(&packet.body)?,
            ids::PLAYER_INFO_UPDATE => {
                play_mut(&mut self.play)?
                    .entities
                    .player_info_update(&packet.body)?;
            }
            ids::PLAYER_INFO_REMOVE => play_mut(&mut self.play)?
                .entities
                .player_info_remove(&packet.body)?,
            ids::UPDATE_MOB_EFFECT | ids::REMOVE_MOB_EFFECT => {
                let mut body = packet.body.as_slice();
                let entity = VarInt::decode(&mut body)?.0;
                let effect = VarInt::decode(&mut body)?.0;
                let play = play_mut(&mut self.play)?;
                if entity == play.player.id {
                    if let Some(name) = Registry::get().mob_effects.get(effect as usize) {
                        let amplifier = if packet.id == ids::UPDATE_MOB_EFFECT {
                            Some(VarInt::decode(&mut body)?.0)
                        } else {
                            None
                        };
                        play.player.physics.effects.set(name, amplifier);
                    }
                }
            }
            ids::SET_ENTITY_DATA => {
                let mut body = packet.body.as_slice();
                let entity = VarInt::decode(&mut body)?.0;
                let play = play_mut(&mut self.play)?;
                if entity == play.player.id {
                    apply_own_entity_data(&mut play.player.physics, body)?;
                } else if let Err(e) = play.entities.set_entity_data(entity, body) {
                    warn!("could not read entity data: {e}");
                }
            }
            ids::CONTAINER_SET_SLOT => {
                let mut body = packet.body.as_slice();
                let _container = VarInt::decode(&mut body)?.0;
                let _state = VarInt::decode(&mut body)?;
                let _slot = i16::decode(&mut body)?;
                let play = play_mut(&mut self.play)?;
                if let Err(e) = play.inventory.set_slot(&packet.body) {
                    warn!("could not read an item stack: {e}");
                }
            }
            ids::PLAYER_CHAT => {
                let play = play_mut(&mut self.play)?;
                let entities = &play.entities;
                // A broken chat chain disconnects, as vanilla does.
                let message = play
                    .chat
                    .player_chat(&packet.body, |sender| {
                        entities.player_info(sender).is_some()
                    })
                    .map_err(|e| ClientError::Protocol(e.to_string()))?;
                if let Some(offset) = message.ack {
                    self.net.send(&sb::ChatAck { offset });
                }
                let from = entities
                    .player_info(&message.sender)
                    .map(|i| i.name.clone());
                debug!(?from, "chat: {}", message.text);
                self.link.emit(crate::BotEvent::Chat {
                    sender: message.sender,
                    name: from,
                    text: message.text,
                });
            }
            ids::DELETE_CHAT => {
                play_mut(&mut self.play)?
                    .chat
                    .delete_chat(&packet.body)
                    .map_err(|e| ClientError::Protocol(e.to_string()))?;
            }
            ids::COMMANDS => match crate::commands::CommandTree::decode(&packet.body) {
                Ok(tree) => play_mut(&mut self.play)?.commands = tree,
                Err(e) => warn!("could not read the command tree: {e}"),
            },
            ids::DISGUISED_CHAT | ids::SET_TITLE_TEXT => {
                let mut body = packet.body.as_slice();
                let text = crate::text::nbt_to_plain(&rapidbot_nbt::Tag::decode(&mut body)?);
                debug!("message: {text}");
            }
            cb::UpdateTags::ID => {
                let payload = packet.decode::<cb::UpdateTags>()?.data.0;
                play_mut(&mut self.play)?.tags.apply(&payload)?;
            }
            cb::Respawn::ID => {
                let respawn = packet.decode::<cb::Respawn>()?;
                let height = dimension_height(&self.session, respawn.spawn.dimension_type);
                let play = play_mut(&mut self.play)?;
                let dimension = respawn.spawn.dimension.to_string();
                info!(%dimension, keep = respawn.data_to_keep, "respawn");
                if dimension != play.dimension {
                    play.game_mode = crate::interact::GameMode::new();
                    // A new ClientLevel: chunks and entities start over.
                    play.world = World::new(height);
                    play.entities = Default::default();
                    play.dimension = dimension.clone();
                }
                play.player = LocalPlayer::respawned(&play.player, respawn.data_to_keep);
                // A new LocalPlayer: empty inventory, slot 0, no screen.
                // The server sends the contents again.
                play.inventory = Default::default();
                self.slot_press = None;
                self.close_press = None;
                play.game_type = respawn.spawn.game_type;
                play.player.physics.spectator = play.game_type == GameType::Spectator;
                play.player.physics.fast_lava =
                    fast_lava(&self.session, respawn.spawn.dimension_type);
                play.client_loaded = false;
                play.death_time = 0;
                play.respawn_in = None;
                play.start_waiting_for_new_level();
                self.aim = None;
                self.key_intent = Default::default();
                self.link.emit(crate::BotEvent::Spawned {
                    entity_id: play.player.id,
                    dimension,
                });
            }
            cb::CustomPayload::ID => {
                let payload = packet.decode::<cb::CustomPayload>()?;
                let brand = String::decode(&mut payload.data.0.as_slice())?;
                self.session.server_brand = Some(brand);
            }
            cb::CookieRequest::ID => {
                let key = packet.decode::<cb::CookieRequest>()?.key;
                let payload = self.session.cookies.get(&key).cloned().map(ByteArray);
                self.net.send(&sb::CookieResponse { key, payload });
            }
            cb::StoreCookie::ID => {
                let cookie = packet.decode::<cb::StoreCookie>()?;
                self.session.cookies.insert(cookie.key, cookie.payload.0);
            }
            cb::ResourcePackPush::ID => {
                let push = packet.decode::<cb::ResourcePackPush>()?;
                self.resource_pack_push(push.id, &push.url, &push.hash.0, push.required)?;
            }
            ids::RESOURCE_PACK_POP => {
                let id = Option::<uuid::Uuid>::decode(&mut packet.body.as_slice())?;
                if let Some(packs) = &mut self.packs {
                    packs.pop(id);
                }
            }
            cb::Transfer::ID => {
                let t = packet.decode::<cb::Transfer>()?;
                return Err(ClientError::Transfer {
                    host: t.host,
                    port: t.port,
                });
            }
            cb::StartConfiguration::ID => {
                // handleConfigurationStart: drop the level, switch to
                // configuration and acknowledge.
                info!("server started reconfiguration");
                self.play = None;
                self.net.send(&sb::ConfigurationAcknowledged);
            }
            other => {
                if State::Play
                    .packet_name(rapidbot_protocol::Direction::Clientbound, other)
                    .is_none()
                {
                    warn!(id = other, "unknown play packet");
                }
            }
        }
        Ok(())
    }
}

/// `SynchedEntityData.assignValues` for the parts of our own player that
/// movement depends on: the shared flags (ID 0) and the pose (ID 6). Values
/// come in ascending ID order; everything before the pose is a simple type.
fn apply_own_entity_data(
    player: &mut rapidbot_physics::Player,
    body: &[u8],
) -> Result<(), ClientError> {
    use crate::entities::DataValue;
    use rapidbot_physics::Pose;
    for (index, value) in crate::entities::decode_entity_data(body)? {
        match (index, value) {
            (0, DataValue::Byte(flags)) => {
                let was_gliding = player.fall_flying;
                player.apply_shared_flags(flags);
                if was_gliding != player.fall_flying {
                    debug!(gliding = !was_gliding, "server changed fall flying");
                }
            }
            (6, DataValue::Pose(pose)) => {
                let pose = match pose {
                    0 => Some(Pose::Standing),
                    1 => Some(Pose::FallFlying),
                    3 => Some(Pose::Swimming),
                    5 => Some(Pose::Crouching),
                    _ => None,
                };
                if let Some(pose) = pose {
                    player.apply_synced_pose(pose);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn play_mut(play: &mut Option<PlayState>) -> Result<&mut PlayState, ClientError> {
    play.as_mut()
        .ok_or_else(|| ClientError::Protocol("play packet before login".into()))
}

fn registry_len(session: &SessionData, name: &str) -> usize {
    session
        .registries
        .iter()
        .filter(|(registry, _)| registry.normalized() == name)
        .map(|(_, entries)| entries.len())
        .sum()
}

/// `EnvironmentAttributes.FAST_LAVA` of a dimension type: from the synced
/// attributes when the server sent the entry, else true for the Nether.
fn fast_lava(session: &SessionData, id: i32) -> bool {
    let entry = session
        .registries
        .iter()
        .filter(|(registry, _)| registry.normalized() == "minecraft:dimension_type")
        .flat_map(|(_, entries)| entries.iter())
        .nth(id as usize);
    let Some(entry) = entry else { return false };
    if let Some(rapidbot_nbt::Tag::Compound(c)) = &entry.data {
        return match c.get("attributes") {
            Some(rapidbot_nbt::Tag::Compound(attributes)) => attributes
                .get("minecraft:gameplay/fast_lava")
                .and_then(rapidbot_nbt::Tag::as_i64)
                .is_some_and(|v| v != 0),
            _ => false,
        };
    }
    entry.id.normalized() == "minecraft:the_nether"
}

/// Height range of the dimension type with registry ID `id`: from the
/// synced NBT when the server sent it, else vanilla's values for the name.
fn dimension_height(session: &SessionData, id: i32) -> DimensionHeight {
    let entry = session
        .registries
        .iter()
        .filter(|(registry, _)| registry.normalized() == "minecraft:dimension_type")
        .flat_map(|(_, entries)| entries.iter())
        .nth(id as usize);
    let Some(entry) = entry else {
        warn!(id, "unknown dimension type, assuming overworld height");
        return DimensionHeight::OVERWORLD;
    };
    if let Some(rapidbot_nbt::Tag::Compound(c)) = &entry.data {
        let get = |k: &str| {
            c.get(k)
                .and_then(rapidbot_nbt::Tag::as_i64)
                .map(|v| v as i32)
        };
        if let (Some(min_y), Some(height)) = (get("min_y"), get("height")) {
            return DimensionHeight { min_y, height };
        }
    }
    DimensionHeight::vanilla(&entry.id.normalized()).unwrap_or_else(|| {
        warn!(dimension = %entry.id, "unknown dimension type, assuming overworld height");
        DimensionHeight::OVERWORLD
    })
}
