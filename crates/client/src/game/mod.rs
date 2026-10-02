//! The main thread, modelled on `Minecraft.runTick`.
//!
//! Each frame:
//! 1. `DeltaTracker.advanceGameTime` works out how many ticks are due.
//! 2. `PacketProcessor.processQueuedPackets` handles every packet the network
//!    side queued since the last frame.
//! 3. Up to 10 client ticks run.
//! 4. The frame "renders": we sleep out the rest of the frame at the
//!    frame rate the vanilla limiter would allow.

mod configuration;
mod frame;
mod player;
mod play;

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use rapidbot_human::{Aim, Keyboard, MouseModel};
use rapidbot_physics::Keys;
use rapidbot_protocol::State;

use crate::bot::{BotEvent, BotLink};
use crate::checks::CheckMonitor;

use crate::controller::{Controller, TickContext};
use crate::net::{Inbound, PacketSender};
use crate::{ClientConfig, ClientError, SessionData, clock};

pub use frame::DisplaySettings;
pub use player::LocalPlayer;

pub(crate) struct Game {
    config: ClientConfig,
    net: PacketSender,
    inbound: Receiver<Inbound>,
    session: SessionData,
    timer: frame::DeltaTimer,
    frames: frame::FrameLimiter,
    seen_code_of_conduct: bool,
    /// `Minecraft.level` / `ClientPacketListener` state; `None` outside play.
    play: Option<play::PlayState>,
    controller: Box<dyn Controller>,
    /// Ticks since the client loaded, for the controller.
    loaded_ticks: u64,
    /// The hand on the mouse, and where the controller wants to look.
    mouse: MouseModel,
    aim: Option<Aim>,
    last_mouse_frame: Instant,
    last_stall: Instant,
    checks: CheckMonitor,
    /// The fingers on the keys, and what the controller wants held.
    keyboard: Keyboard,
    key_intent: Keys,
    link: BotLink,
    noise: rapidbot_human::Noise,
    /// Chat lines waiting to be typed, and the one being typed.
    chat_queue: std::collections::VecDeque<String>,
    typing: Option<Typing>,
    typist: rapidbot_human::Typist,
    /// Server resource packs: prompt, downloads, status reports.
    /// When the "Acknowledge" button of the code of conduct gets clicked.
    conduct_click: Option<Instant>,
    /// A hotbar key on its way down: the slot, and ticks until it lands.
    slot_press: Option<(u8, u32)>,
    /// The attack button: wanted, physically down, and presses since the
    /// last tick (`KeyMapping.clickCount`).
    attack_intent: bool,
    attack_down: bool,
    attack_clicks: u32,
    /// Ticks until the finger lets go of a `click_attack`.
    click_release: Option<u32>,
    /// Ticks until Escape closes the open container screen.
    close_press: Option<u32>,
    packs: Option<crate::packs::PackManager>,
    /// A pack the server requires is waiting on the prompt.
    pack_required: bool,
}

/// A chat line on its way out.
struct Typing {
    line: String,
    /// Seconds until the chat box opens; then seconds until Enter.
    remaining: f64,
    open: bool,
}

impl Game {
    pub fn new(
        config: ClientConfig,
        net: PacketSender,
        inbound: Receiver<Inbound>,
        session: SessionData,
        controller: Box<dyn Controller>,
        link: BotLink,
    ) -> Self {
        let now = clock::millis();
        let frames = frame::FrameLimiter::new(config.display, now);
        let seed = config.human_seed.unwrap_or_else(|| {
            // FNV-1a of the account name: a stable "person" per account.
            config.account.name().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
        });
        let checks = CheckMonitor::new(config.account.name());
        let mouse = match &config.mouse_profile {
            Some(profile) => MouseModel::with_profile(seed, profile.clone()),
            None => MouseModel::new(seed),
        };
        Self {
            config,
            net,
            inbound,
            session,
            timer: frame::DeltaTimer::new(20.0, now),
            frames,
            seen_code_of_conduct: false,
            play: None,
            controller,
            loaded_ticks: 0,
            mouse,
            aim: None,
            last_mouse_frame: Instant::now(),
            last_stall: Instant::now(),
            checks,
            keyboard: Keyboard::new(seed ^ 0x5bd1_e995),
            key_intent: Keys::default(),
            link,
            noise: rapidbot_human::Noise::new(seed ^ 0x2545_f491),
            chat_queue: Default::default(),
            typing: None,
            typist: rapidbot_human::Typist::new(seed ^ 0x9e37_79b9),
            conduct_click: None,
            slot_press: None,
            attack_intent: false,
            attack_down: false,
            attack_clicks: 0,
            click_release: None,
            close_press: None,
            packs: None,
            pack_required: false,
        }
    }

    /// Runs until the connection ends. Returns the disconnect reason.
    pub fn run(mut self) -> ClientError {
        loop {
            if self.link.stopped() {
                return ClientError::Stopped;
            }
            let frame_start = std::time::Instant::now();
            let ticks = self.timer.advance_game_time(clock::millis());

            if let Err(e) = self.process_queued_packets() {
                return e;
            }
            if self.conduct_click.take_if(|at| Instant::now() >= *at).is_some() {
                self.net.send(&rapidbot_protocol::packets::configuration::serverbound::AcceptCodeOfConduct);
            }
            if let Some(packs) = &mut self.packs {
                let turn = packs.poll(self.pack_required);
                if let Err(e) = self.send_pack_turn(turn) {
                    return e;
                }
            }
            let handled_in = frame_start.elapsed();
            for _ in 0..ticks.min(10) {
                self.tick();
            }
            // Lag spikes change what an anticheat's timer check sees, so
            // they are worth knowing about when reading its flags.
            if ticks > 2 || handled_in.as_millis() > 50 || frame_start.elapsed().as_millis() > 100 {
                tracing::debug!(ticks, packets_ms = handled_in.as_millis() as u64, frame_ms = frame_start.elapsed().as_millis() as u64, "slow frame");
            }
            self.input_frame();
            if let Some(play) = &mut self.play {
                play.on_frame();
            }

            // Screens: the connect/reconfigure screens show whenever there is
            // no level, which the limiter treats as a menu.
            let in_menu = self.play.is_none();
            self.frames.wait_for_next_frame(frame_start, in_menu);
            // Test hook: RAPIDBOT_STALL_MS=400 freezes the client for that
            // long every ten seconds, to see how a server takes lag spikes.
            if let Some(ms) = stall_ms() {
                if self.last_stall.elapsed().as_secs() >= 10 {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    self.last_stall = Instant::now();
                }
            }
        }
    }

    /// `MouseHandler.handleAccumulatedMovement`: once per frame, after the
    /// ticks, the frame's mouse movement turns the player. The mouse is
    /// only grabbed in game with no screen open.
    ///
    /// Key states change here too: GLFW delivers key events while the frame
    /// polls for input, and the next tick reads whatever is down.
    fn input_frame(&mut self) {
        let dt = self.last_mouse_frame.elapsed().as_secs_f64().min(0.25);
        self.last_mouse_frame = Instant::now();
        // With a screen open (the death screen) the mouse is not grabbed
        // and movement keys do nothing.
        if !self.play.as_ref().is_some_and(|p| p.client_loaded && !p.player.physics.dead) {
            return;
        }
        // The pack prompt is a screen: keys released, mouse free.
        if self.packs.as_ref().is_some_and(|p| p.screen_open()) {
            if let Some(play) = &mut self.play {
                play.player.physics.held_keys = Keys::default();
            }
            return;
        }
        // So is a container the server opened.
        if let Some(play) = self.play.as_mut().filter(|p| p.inventory.container.is_some()) {
            play.player.physics.held_keys = Keys::default();
            self.attack_down = false;
            return;
        }
        if self.chat_frame(dt) {
            // KeyMapping.releaseAll() when the screen opened.
            self.attack_down = false;
            return;
        }
        let Some(play) = self.play.as_mut() else { return };
        let settings = self.config.mouse;
        let player = &mut play.player.physics;

        let k = self.key_intent;
        self.keyboard.set_intent([k.forward, k.backward, k.left, k.right, k.jump, k.shift, k.sprint, self.attack_intent, false]);
        let down = self.keyboard.frame(dt, self.mouse.fatigue.level());
        let held = Keys {
            forward: down[0],
            backward: down[1],
            left: down[2],
            right: down[3],
            jump: down[4],
            shift: down[5],
            sprint: down[6],
        };
        if held != player.held_keys {
            self.frames.on_input_received(clock::millis());
            player.held_keys = held;
        }
        if down[7] != self.attack_down {
            self.frames.on_input_received(clock::millis());
            self.attack_down = down[7];
            if down[7] {
                self.attack_clicks += 1;
            }
        }

        self.mouse.set_target(self.aim);
        let (dx, dy) = self.mouse.frame(dt, player.y_rot, player.x_rot, settings.degrees_per_count());
        if (dx, dy) != (0, 0) {
            self.frames.on_input_received(clock::millis());
            settings.turn_player(player, dx, dy);
        }
    }

    /// The chat box: opening it, typing, Enter. Key events arrive while
    /// the frame polls for input, so a line goes out between ticks, as
    /// `ChatScreen.handleChatInput` does. Returns true while the box is
    /// open: the mouse is not grabbed then, and opening a screen let go of
    /// every key (`KeyMapping.releaseAll`).
    fn chat_frame(&mut self, dt: f64) -> bool {
        if self.typing.is_none() {
            let Some(line) = self.chat_queue.pop_front() else { return false };
            let line = crate::chat::normalize(&line);
            if line.is_empty() {
                return false;
            }
            self.typing = Some(Typing { remaining: self.typist.open_delay(), line, open: false });
        }
        let Some(typing) = &mut self.typing else { return false };
        typing.remaining -= dt;
        if typing.remaining > 0.0 {
            if !typing.open {
                return false;
            }
        } else if !typing.open {
            typing.open = true;
            typing.remaining = self.typist.time_to_type(&typing.line);
            self.frames.on_input_received(clock::millis());
        } else {
            let line = self.typing.take().map(|t| t.line).unwrap_or_default();
            self.frames.on_input_received(clock::millis());
            if let Some(play) = &mut self.play {
                play.send_chat_line(&line, &self.net);
            }
            // The box closes; the keys held before are picked up again by
            // the next frames.
            return false;
        }
        if let Some(play) = &mut self.play {
            play.player.physics.held_keys = Keys::default();
        }
        true
    }

    /// `handleResourcePackPush`, in configuration or play.
    fn resource_pack_push(&mut self, id: uuid::Uuid, url: &str, hash: &str, required: bool) -> Result<(), ClientError> {
        if self.packs.is_none() {
            let profile = self.session.profile.as_ref().map(|p| p.id).unwrap_or_default();
            let seed = self.noise.uniform(0.0, 1.0e12) as u64;
            self.packs = Some(crate::packs::PackManager::new(
                self.config.resource_packs,
                self.config.account.name().to_owned(),
                profile,
                self.config.pack_cache.clone(),
                seed,
            ));
        }
        self.pack_required |= required;
        let turn = self.packs.as_mut().map(|p| p.push(id, url, hash, required)).unwrap_or_default();
        self.send_pack_turn(turn)
    }

    fn send_pack_turn(&mut self, turn: crate::packs::PackTurn) -> Result<(), ClientError> {
        use rapidbot_protocol::packets::{configuration, play};
        for (id, action) in turn.responses {
            tracing::debug!(%id, ?action, "resource pack status");
            if self.play.is_some() {
                self.net.send(&play::serverbound::ResourcePack { id, action });
            } else {
                self.net.send(&configuration::serverbound::ResourcePack { id, action });
            }
        }
        if turn.disconnect {
            // "Disconnect" on the prompt for a required pack.
            return Err(ClientError::ResourcePackRequired);
        }
        Ok(())
    }

    fn process_queued_packets(&mut self) -> Result<(), ClientError> {
        loop {
            match self.inbound.try_recv() {
                Ok(Inbound::Packet { state, packet }) => self.handle(state, packet)?,
                Ok(Inbound::Bundle(packets)) => {
                    for packet in packets {
                        self.handle(State::Play, packet)?;
                    }
                }
                Ok(Inbound::Disconnected(reason)) => return Err(ClientError::Disconnected(reason)),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(ClientError::Disconnected("network task ended".into()));
                }
            }
        }
    }

    fn handle(&mut self, state: State, packet: rapidbot_protocol::RawPacket) -> Result<(), ClientError> {
        match state {
            State::Configuration => self.handle_configuration(packet),
            State::Play => self.handle_play(packet),
            other => Err(ClientError::Protocol(format!("packet in unexpected state {other:?}"))),
        }
    }

    /// `Minecraft.tick`, the parts that matter to the server.
    fn tick(&mut self) {
        let Some(play) = &mut self.play else {
            // No level: the configuration listener's tick only flushes
            // deferred packets, which we never defer.
            return;
        };
        // gameMode.tick() → connection.tick() → ClientPacketListener.tick()
        if play.listener_tick(&self.net) {
            self.link.emit(BotEvent::Loaded);
        }

        let dead = play.player.physics.dead;
        if dead {
            // Death screen: no keybinds. The entity ticks for 20 more ticks
            // (LocalPlayer.tickDeath), then is removed.
            if play.client_loaded && play.death_time < 20 {
                play.player.physics.held_keys = Keys::default();
                play.player.physics.tick(&play.world, &play.tags);
                play.player.send_changes(&self.net);
            }
            play.death_time += 1;
            if let Some(n) = play.respawn_in {
                if n == 0 {
                    play.respawn_in = None;
                    self.net.send(&rapidbot_protocol::packets::play::serverbound::ClientCommand {
                        action: rapidbot_protocol::packets::play::serverbound::ClientCommandAction::PerformRespawn,
                    });
                } else {
                    play.respawn_in = Some(n - 1);
                }
            }
        } else if play.client_loaded {
            // handleKeybinds(): the controller stands in for the keyboard
            // and mouse. Any change counts as input for the frame limiter.
            let pos = play.player.physics.pos;
            let players = play.entities.players_near(pos, 32.0, play.player.id);
            let close: Vec<_> = players.iter().filter(|p| p.distance <= crate::checks::NEARBY_RADIUS).cloned().collect();
            self.checks.tick(pos);
            self.checks.players_nearby(&close);
            let events = self.checks.take_events();
            for event in &events {
                self.link.emit(BotEvent::Check(event.clone()));
            }
            if let Ok(mut asked) = self.link.chat.lock() {
                self.chat_queue.extend(asked.drain(..));
            }
            let before = (play.player.physics.y_rot, play.player.physics.x_rot);
            // pick(1.0F): what the crosshair is on, before the keybinds.
            let pick = crate::interact::pick(&play.player.physics, &play.world, &play.entities, play.player.id, &play.tags);
            let chat_open = self.typing.as_ref().is_some_and(|t| t.open);
            let screen_open = chat_open || play.inventory.container.is_some();
            // The hotbar key lands (keyHotbarSlots in handleKeybinds, which
            // does not run with a screen open).
            if let Some((slot, ticks)) = self.slot_press {
                if ticks > 0 {
                    self.slot_press = Some((slot, ticks - 1));
                } else if !screen_open {
                    self.slot_press = None;
                    play.inventory.selected = slot;
                }
            }
            // Escape on a container screen: LocalPlayer.closeContainer().
            if let Some(ticks) = self.close_press {
                if ticks > 0 {
                    self.close_press = Some(ticks - 1);
                } else {
                    self.close_press = None;
                    if let Some(container) = &play.inventory.container {
                        self.net.send(&rapidbot_protocol::packets::play::serverbound::ContainerClose { container_id: container.id });
                        play.inventory.close_container();
                    }
                }
            }
            // The glide check in aiStep reads the equipment slots.
            play.player.physics.has_elytra = play.inventory.can_glide();
            let mut actions = crate::controller::Actions::default();
            let attack_strength = play.game_mode.attack_strength(&play.player.physics, 0.5);
            let mut ctx = TickContext {
                player: &mut play.player.physics,
                world: &play.world,
                tick: self.loaded_ticks,
                events: &events,
                suspicion: self.checks.suspicion(),
                players: &players,
                chat_open,
                inventory: &play.inventory,
                entities: &play.entities,
                crosshair: pick.block,
                target: pick.entity,
                attack_strength,
                own_id: play.player.id,
                tags: &play.tags,
                breaking: play.game_mode.breaking(),
                actions: &mut actions,
                aim: &mut self.aim,
                keys: &mut self.key_intent,
                chat: &mut self.chat_queue,
            };
            self.controller.tick(&mut ctx);
            self.loaded_ticks += 1;
            if let Some(slot) = actions.select_slot {
                let pending = self.slot_press.map(|(s, _)| s);
                if pending != Some(slot) && (pending.is_some() || slot != play.inventory.selected) {
                    // Finding the number key: quicker than a mouse movement.
                    let seconds = self.noise.lognormal(0.22, 0.35).clamp(0.08, 1.2);
                    self.slot_press = Some((slot, (seconds * 20.0).round() as u32));
                }
            }
            if let Some(down) = actions.attack {
                self.attack_intent = down;
                self.click_release = None;
            } else if let Some(ticks) = self.click_release {
                if ticks == 0 {
                    self.click_release = None;
                    self.attack_intent = false;
                } else {
                    self.click_release = Some(ticks - 1);
                }
            } else if actions.click && !self.attack_intent {
                // How long a finger stays on the button for a click.
                let seconds = self.noise.lognormal(0.085, 0.3).clamp(0.04, 0.3);
                self.attack_intent = true;
                self.click_release = Some((seconds * 20.0).ceil() as u32);
            }
            // The attack button: startAttack per click, then continueAttack.
            if screen_open {
                // `missTime = 10000`; clicks go to the screen.
                play.game_mode.screen_open();
                self.attack_clicks = 0;
            } else {
                let clicks = std::mem::take(&mut self.attack_clicks);
                play.attack_button(&self.net, clicks, self.attack_down, &pick);
            }
            if actions.close_container && self.close_press.is_none() && play.inventory.container.is_some() {
                let seconds = self.noise.lognormal(0.7, 0.4).clamp(0.3, 4.0);
                self.close_press = Some((seconds * 20.0).round() as u32);
            }
            let after = (play.player.physics.y_rot, play.player.physics.x_rot);
            if before != after {
                self.frames.on_input_received(clock::millis());
            }

            // level.tickEntities() → LocalPlayer.tick()
            play.player.physics.tick(&play.world, &play.tags);
            // The end of Player.tick: the attack charge.
            play.game_mode.player_tick(play.inventory.held());
            // player.sendChanges()
            play.player.send_changes(&self.net);
        }
        // level.tickEntities(): the others glide towards where the server
        // last put them.
        play.entities.tick();
        // level.tick(), particles, ...
        self.net.send(&rapidbot_protocol::packets::play::serverbound::ClientTickEnd);
        tracing::trace!(target: "rapidbot_client::tick", "tick");
    }
}

fn stall_ms() -> Option<u64> {
    static STALL: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *STALL.get_or_init(|| std::env::var("RAPIDBOT_STALL_MS").ok().and_then(|v| v.parse().ok()))
}
