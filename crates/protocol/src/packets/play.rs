//! Play state: the common packets plus gameplay packets, added as the client
//! learns to handle them.

use rapidbot_buf::{Decode, Encode, Identifier};

super::common::common_packets!(play);

/// `Vec3.STREAM_CODEC`: three doubles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Encode, Decode)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// `PositionMoveRotation.STREAM_CODEC`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
pub struct PositionMoveRotation {
    pub position: Vec3,
    pub delta_movement: Vec3,
    pub y_rot: f32,
    pub x_rot: f32,
}

/// `Relative` bit flags (`Relative.SET_STREAM_CODEC`, an int).
pub mod relative {
    pub const X: i32 = 1 << 0;
    pub const Y: i32 = 1 << 1;
    pub const Z: i32 = 1 << 2;
    pub const Y_ROT: i32 = 1 << 3;
    pub const X_ROT: i32 = 1 << 4;
    pub const DELTA_X: i32 = 1 << 5;
    pub const DELTA_Y: i32 = 1 << 6;
    pub const DELTA_Z: i32 = 1 << 7;
    pub const ROTATE_DELTA: i32 = 1 << 8;
}

/// `GameType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum GameType {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

/// `GameType.OPTIONAL_STREAM_CODEC`: VarInt id + 1, with 0 meaning empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionalGameType(pub Option<GameType>);

impl Encode for OptionalGameType {
    fn encode(&self, buf: &mut Vec<u8>) {
        let v = self.0.map_or(0, |g| g as i32 + 1);
        rapidbot_buf::VarInt(v).encode(buf);
    }
}

impl Decode for OptionalGameType {
    fn decode(buf: &mut &[u8]) -> Result<Self, rapidbot_buf::DecodeError> {
        let v = rapidbot_buf::VarInt::decode(buf)?.0;
        if v == 0 {
            return Ok(Self(None));
        }
        let mut id: &[u8] = &[(v - 1) as u8];
        GameType::decode(&mut id).map(|g| Self(Some(g)))
    }
}

/// `GlobalPos`: dimension plus packed `BlockPos`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct GlobalPos {
    pub dimension: Identifier,
    pub pos: i64,
}

/// `CommonPlayerSpawnInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct CommonPlayerSpawnInfo {
    /// Registry ID in `minecraft:dimension_type`.
    #[var]
    pub dimension_type: i32,
    pub dimension: Identifier,
    pub seed: i64,
    pub game_type: GameType,
    pub previous_game_type: OptionalGameType,
    pub is_debug: bool,
    pub is_flat: bool,
    pub last_death_location: Option<GlobalPos>,
    #[var]
    pub portal_cooldown: i32,
    #[var]
    pub sea_level: i32,
}

/// `Input`: movement keys as bit flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Input {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub shift: bool,
    pub sprint: bool,
}

impl Encode for Input {
    fn encode(&self, buf: &mut Vec<u8>) {
        let flags = self.forward as u8
            | (self.backward as u8) << 1
            | (self.left as u8) << 2
            | (self.right as u8) << 3
            | (self.jump as u8) << 4
            | (self.shift as u8) << 5
            | (self.sprint as u8) << 6;
        buf.push(flags);
    }
}

impl Decode for Input {
    fn decode(buf: &mut &[u8]) -> Result<Self, rapidbot_buf::DecodeError> {
        let f = u8::decode(buf)?;
        Ok(Self {
            forward: f & 1 != 0,
            backward: f & 2 != 0,
            left: f & 4 != 0,
            right: f & 8 != 0,
            jump: f & 16 != 0,
            shift: f & 32 != 0,
            sprint: f & 64 != 0,
        })
    }
}

/// `LpVec3`: a vector packed into 6 bytes (15 bits per axis) plus a scale,
/// used for entity velocities.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LpVec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl LpVec3 {
    fn pack(v: f64) -> i64 {
        // Math.round: floor(x + 0.5).
        ((v * 0.5 + 0.5) * 32766.0 + 0.5).floor() as i64
    }

    fn unpack(v: i64) -> f64 {
        ((v & 32767) as f64).min(32766.0) * 2.0 / 32766.0 - 1.0
    }
}

impl Encode for LpVec3 {
    fn encode(&self, buf: &mut Vec<u8>) {
        let sanitize = |v: f64| if v.is_nan() { 0.0 } else { v.clamp(-1.7179869183e10, 1.7179869183e10) };
        let (x, y, z) = (sanitize(self.x), sanitize(self.y), sanitize(self.z));
        let length = x.abs().max(y.abs()).max(z.abs());
        if length < 3.051944088384301e-5 {
            buf.push(0);
            return;
        }
        let scale = length.ceil() as i64;
        let partial = (scale & 3) != scale;
        let markers = if partial { scale & 3 | 4 } else { scale };
        let s = scale as f64;
        let buffer = markers | Self::pack(x / s) << 3 | Self::pack(y / s) << 18 | Self::pack(z / s) << 33;
        buf.push(buffer as u8);
        buf.push((buffer >> 8) as u8);
        ((buffer >> 16) as i32).encode(buf);
        if partial {
            rapidbot_buf::VarInt((scale >> 2) as i32).encode(buf);
        }
    }
}

impl Decode for LpVec3 {
    fn decode(buf: &mut &[u8]) -> Result<Self, rapidbot_buf::DecodeError> {
        let lowest = u8::decode(buf)? as i64;
        if lowest == 0 {
            return Ok(Self::default());
        }
        let middle = u8::decode(buf)? as i64;
        let highest = u32::decode(buf)? as i64;
        let buffer = highest << 16 | middle << 8 | lowest;
        let mut scale = lowest & 3;
        if lowest & 4 == 4 {
            scale |= (rapidbot_buf::VarInt::decode(buf)?.0 as u32 as i64) << 2;
        }
        let s = scale as f64;
        Ok(Self { x: Self::unpack(buffer >> 3) * s, y: Self::unpack(buffer >> 18) * s, z: Self::unpack(buffer >> 33) * s })
    }
}

/// `ServerboundMovePlayerPacket.packFlags`.
pub fn move_flags(on_ground: bool, horizontal_collision: bool) -> u8 {
    on_ground as u8 | (horizontal_collision as u8) << 1
}

pub mod clientbound {
    pub use super::common_clientbound::*;
    use rapidbot_buf::{Decode, Encode, Identifier, RemainingBytes};

    use super::{CommonPlayerSpawnInfo, PositionMoveRotation};
    use crate::packets::packet;

    /// `ClientboundBundleDelimiterPacket`: packets between two delimiters are
    /// handled together on the main thread.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct BundleDelimiter;
    packet!(BundleDelimiter, play, clientbound, BUNDLE_DELIMITER);

    /// `ClientboundLoginPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct Login {
        pub player_id: i32,
        pub hardcore: bool,
        pub levels: Vec<Identifier>,
        #[var]
        pub max_players: i32,
        #[var]
        pub chunk_radius: i32,
        #[var]
        pub simulation_distance: i32,
        pub reduced_debug_info: bool,
        pub show_death_screen: bool,
        pub do_limited_crafting: bool,
        pub spawn: CommonPlayerSpawnInfo,
        pub online_mode: bool,
        pub enforces_secure_chat: bool,
    }
    packet!(Login, play, clientbound, LOGIN);

    /// `ClientboundRespawnPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct Respawn {
        pub spawn: CommonPlayerSpawnInfo,
        pub data_to_keep: u8,
    }
    packet!(Respawn, play, clientbound, RESPAWN);

    /// `ClientboundPlayerPositionPacket`: a teleport the client must confirm.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct PlayerPosition {
        #[var]
        pub id: i32,
        pub change: PositionMoveRotation,
        /// [`super::relative`] flags.
        pub relatives: i32,
    }
    packet!(PlayerPosition, play, clientbound, PLAYER_POSITION);

    /// `ClientboundPlayerRotationPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct PlayerRotation {
        pub y_rot: f32,
        pub relative_y: bool,
        pub x_rot: f32,
        pub relative_x: bool,
    }
    packet!(PlayerRotation, play, clientbound, PLAYER_ROTATION);

    /// `ClientboundGameEventPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct GameEvent {
        pub event: u8,
        pub param: f32,
    }
    packet!(GameEvent, play, clientbound, GAME_EVENT);

    impl GameEvent {
        pub const CHANGE_GAME_MODE: u8 = 3;
        pub const LEVEL_CHUNKS_LOAD_START: u8 = 13;
    }

    /// `ClientboundLevelChunkWithLightPacket`. Only the position is decoded
    /// for now.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct LevelChunkWithLight {
        pub x: i32,
        pub z: i32,
        pub data: RemainingBytes,
    }
    packet!(LevelChunkWithLight, play, clientbound, LEVEL_CHUNK_WITH_LIGHT);

    /// `ClientboundForgetLevelChunkPacket`: `ChunkPos.pack`, x in the low 32
    /// bits and z in the high.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ForgetLevelChunk {
        pub pos: i64,
    }
    packet!(ForgetLevelChunk, play, clientbound, FORGET_LEVEL_CHUNK);

    impl ForgetLevelChunk {
        pub fn x(&self) -> i32 {
            self.pos as i32
        }

        pub fn z(&self) -> i32 {
            (self.pos >> 32) as i32
        }
    }

    /// `ClientboundChunkBatchStartPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ChunkBatchStart;
    packet!(ChunkBatchStart, play, clientbound, CHUNK_BATCH_START);

    /// `ClientboundChunkBatchFinishedPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ChunkBatchFinished {
        #[var]
        pub batch_size: i32,
    }
    packet!(ChunkBatchFinished, play, clientbound, CHUNK_BATCH_FINISHED);

    /// `ClientboundSetChunkCacheCenterPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct SetChunkCacheCenter {
        #[var]
        pub x: i32,
        #[var]
        pub z: i32,
    }
    packet!(SetChunkCacheCenter, play, clientbound, SET_CHUNK_CACHE_CENTER);

    /// `ClientboundStartConfigurationPacket`: go back to configuration.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct StartConfiguration;
    packet!(StartConfiguration, play, clientbound, START_CONFIGURATION);

    /// `ClientboundBlockUpdatePacket`: packed `BlockPos` and a block state ID.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct BlockUpdate {
        pub pos: i64,
        #[var]
        pub state: i32,
    }
    packet!(BlockUpdate, play, clientbound, BLOCK_UPDATE);

    /// `ClientboundBlockChangedAckPacket`: predicted block changes up to
    /// this sequence number have been processed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct BlockChangedAck {
        #[var]
        pub sequence: i32,
    }
    packet!(BlockChangedAck, play, clientbound, BLOCK_CHANGED_ACK);

    /// `ClientboundSectionBlocksUpdatePacket`: packed `SectionPos`, then per
    /// block `state << 12 | x << 8 | z << 4 | y`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct SectionBlocksUpdate {
        pub section: i64,
        pub updates: Vec<rapidbot_buf::VarLong>,
    }
    packet!(SectionBlocksUpdate, play, clientbound, SECTION_BLOCKS_UPDATE);

    /// `ClientboundUpdateAttributesPacket`.
    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct UpdateAttributes {
        #[var]
        pub entity_id: i32,
        pub attributes: Vec<AttributeSnapshot>,
    }
    packet!(UpdateAttributes, play, clientbound, UPDATE_ATTRIBUTES);

    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct AttributeSnapshot {
        /// ID in the built-in `minecraft:attribute` registry.
        #[var]
        pub attribute: i32,
        pub base: f64,
        pub modifiers: Vec<AttributeModifier>,
    }

    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct AttributeModifier {
        pub id: Identifier,
        pub amount: f64,
        /// `AttributeModifier.Operation` ordinal.
        #[var]
        pub operation: i32,
    }

    /// `ClientboundPlayerAbilitiesPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct PlayerAbilities {
        /// 1 invulnerable, 2 flying, 4 can fly, 8 instabuild.
        pub flags: u8,
        pub flying_speed: f32,
        pub walking_speed: f32,
    }
    packet!(PlayerAbilities, play, clientbound, PLAYER_ABILITIES);

    /// `ClientboundSetHealthPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct SetHealth {
        pub health: f32,
        #[var]
        pub food: i32,
        pub saturation: f32,
    }
    packet!(SetHealth, play, clientbound, SET_HEALTH);

    /// `ClientboundUpdateTagsPacket`, kept raw for `rapidbot_world::tags`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct UpdateTags {
        pub data: RemainingBytes,
    }
    packet!(UpdateTags, play, clientbound, UPDATE_TAGS);

    /// `ClientboundSetEntityMotionPacket`: velocity in `LpVec3` form.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct SetEntityMotion {
        #[var]
        pub entity_id: i32,
        pub movement: super::LpVec3,
    }
    packet!(SetEntityMotion, play, clientbound, SET_ENTITY_MOTION);

    /// `ClientboundPlayerLookAtPacket`: the server turns the player to face
    /// a point (or an entity, whose position the client looks up).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct PlayerLookAt {
        /// `EntityAnchorArgument.Anchor`: 0 feet, 1 eyes.
        pub from_anchor: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        /// Entity ID and its anchor, when looking at an entity.
        pub entity: Option<(i32, i32)>,
    }
    packet!(PlayerLookAt, play, clientbound, PLAYER_LOOK_AT);

    impl Encode for PlayerLookAt {
        fn encode(&self, buf: &mut Vec<u8>) {
            use rapidbot_buf::VarInt;
            VarInt(self.from_anchor).encode(buf);
            self.x.encode(buf);
            self.y.encode(buf);
            self.z.encode(buf);
            self.entity.is_some().encode(buf);
            if let Some((id, anchor)) = self.entity {
                VarInt(id).encode(buf);
                VarInt(anchor).encode(buf);
            }
        }
    }

    impl Decode for PlayerLookAt {
        fn decode(buf: &mut &[u8]) -> Result<Self, rapidbot_buf::DecodeError> {
            use rapidbot_buf::VarInt;
            let from_anchor = VarInt::decode(buf)?.0;
            let (x, y, z) = (f64::decode(buf)?, f64::decode(buf)?, f64::decode(buf)?);
            let entity = if bool::decode(buf)? { Some((VarInt::decode(buf)?.0, VarInt::decode(buf)?.0)) } else { None };
            Ok(Self { from_anchor, x, y, z, entity })
        }
    }

    /// `ClientboundExplodePacket`, up to the knockback; particles and sound
    /// are left undecoded.
    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct Explode {
        pub center: super::Vec3,
        pub radius: f32,
        pub block_count: i32,
        pub player_knockback: Option<super::Vec3>,
        pub rest: RemainingBytes,
    }
    packet!(Explode, play, clientbound, EXPLODE);

    /// `ClientboundSetHeldSlotPacket`: the server changes the hotbar slot.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct SetHeldSlot {
        #[var]
        pub slot: i32,
    }
    packet!(SetHeldSlot, play, clientbound, SET_HELD_SLOT);

    /// `ClientboundOpenScreenPacket`: only the container ID is decoded.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct OpenScreen {
        #[var]
        pub container_id: i32,
        pub rest: RemainingBytes,
    }
    packet!(OpenScreen, play, clientbound, OPEN_SCREEN);

    /// `ClientboundSystemChatPacket`.
    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct SystemChat {
        pub content: rapidbot_nbt::Tag,
        /// Shown above the hotbar instead of in chat.
        pub overlay: bool,
    }
    packet!(SystemChat, play, clientbound, SYSTEM_CHAT);

    /// `ClientboundHurtAnimationPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct HurtAnimation {
        #[var]
        pub entity_id: i32,
        pub yaw: f32,
    }
    packet!(HurtAnimation, play, clientbound, HURT_ANIMATION);

    /// `ClientboundDamageEventPacket`: only the damaged entity is decoded.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct DamageEvent {
        #[var]
        pub entity_id: i32,
        pub rest: RemainingBytes,
    }
    packet!(DamageEvent, play, clientbound, DAMAGE_EVENT);
}

pub mod serverbound {
    pub use super::common_serverbound::*;
    use rapidbot_buf::{Decode, DecodeError, Encode, VarInt};

    use super::{Input, Vec3};
    use crate::packets::packet;

    /// `ServerboundAcceptTeleportationPacket`: the teleport ID plus where
    /// the client ended up.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct AcceptTeleportation {
        #[var]
        pub id: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        pub y_rot: f32,
        pub x_rot: f32,
    }
    packet!(AcceptTeleportation, play, serverbound, ACCEPT_TELEPORTATION);

    /// `ServerboundMovePlayerPacket.Pos`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct MovePlayerPos {
        pub pos: Vec3,
        /// [`super::move_flags`].
        pub flags: u8,
    }
    packet!(MovePlayerPos, play, serverbound, MOVE_PLAYER_POS);

    /// `ServerboundMovePlayerPacket.PosRot`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct MovePlayerPosRot {
        pub pos: Vec3,
        pub y_rot: f32,
        pub x_rot: f32,
        pub flags: u8,
    }
    packet!(MovePlayerPosRot, play, serverbound, MOVE_PLAYER_POS_ROT);

    /// `ServerboundMovePlayerPacket.Rot`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct MovePlayerRot {
        pub y_rot: f32,
        pub x_rot: f32,
        pub flags: u8,
    }
    packet!(MovePlayerRot, play, serverbound, MOVE_PLAYER_ROT);

    /// `ServerboundMovePlayerPacket.StatusOnly`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct MovePlayerStatusOnly {
        pub flags: u8,
    }
    packet!(MovePlayerStatusOnly, play, serverbound, MOVE_PLAYER_STATUS_ONLY);

    /// `ServerboundPlayerInputPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PlayerInput {
        pub input: Input,
    }
    packet!(PlayerInput, play, serverbound, PLAYER_INPUT);

    /// `ServerboundPlayerLoadedPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PlayerLoaded;
    packet!(PlayerLoaded, play, serverbound, PLAYER_LOADED);

    /// `ServerboundClientTickEndPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ClientTickEnd;
    packet!(ClientTickEnd, play, serverbound, CLIENT_TICK_END);

    /// `ServerboundChunkBatchReceivedPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
    pub struct ChunkBatchReceived {
        pub desired_chunks_per_tick: f32,
    }
    packet!(ChunkBatchReceived, play, serverbound, CHUNK_BATCH_RECEIVED);

    /// `ServerboundPlayerCommandPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PlayerCommand {
        #[var]
        pub entity_id: i32,
        pub action: PlayerCommandAction,
        #[var]
        pub data: i32,
    }
    packet!(PlayerCommand, play, serverbound, PLAYER_COMMAND);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub enum PlayerCommandAction {
        StopSleeping,
        StartSprinting,
        StopSprinting,
        StartRidingJump,
        StopRidingJump,
        OpenInventory,
        StartFallFlying,
    }

    /// A 256-byte `MessageSignature`.
    pub type MessageSignature = Box<[u8; 256]>;

    fn read_signature(buf: &mut &[u8]) -> Result<MessageSignature, DecodeError> {
        let mut signature = Box::new([0u8; 256]);
        signature.copy_from_slice(rapidbot_buf::take(buf, 256)?);
        Ok(signature)
    }

    /// `LastSeenMessages.Update`: what a chat packet says about the signed
    /// messages its sender has seen.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LastSeenUpdate {
        /// Messages received since the last update.
        pub offset: i32,
        /// Bit i set: slot i of the 20-message window (oldest first) holds
        /// a message.
        pub acknowledged: u32,
        pub checksum: u8,
    }

    impl Encode for LastSeenUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            VarInt(self.offset).encode(out);
            // ByteBufCodecs.fixedBitSet(20): three bytes, little-endian bits.
            out.extend_from_slice(&self.acknowledged.to_le_bytes()[..3]);
            out.push(self.checksum);
        }
    }

    impl Decode for LastSeenUpdate {
        fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
            let offset = VarInt::decode(buf)?.0;
            let bits = rapidbot_buf::take(buf, 3)?;
            let acknowledged = u32::from_le_bytes([bits[0], bits[1], bits[2], 0]);
            Ok(Self { offset, acknowledged, checksum: u8::decode(buf)? })
        }
    }

    /// `ServerboundChatPacket`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Chat {
        /// At most 256 characters.
        pub message: String,
        /// `Instant` as Unix milliseconds.
        pub timestamp: i64,
        pub salt: i64,
        pub signature: Option<MessageSignature>,
        pub last_seen: LastSeenUpdate,
    }

    impl Encode for Chat {
        fn encode(&self, out: &mut Vec<u8>) {
            self.message.encode(out);
            self.timestamp.encode(out);
            self.salt.encode(out);
            self.signature.is_some().encode(out);
            if let Some(signature) = &self.signature {
                out.extend_from_slice(&signature[..]);
            }
            self.last_seen.encode(out);
        }
    }

    impl Decode for Chat {
        fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
            Ok(Self {
                message: String::decode(buf)?,
                timestamp: i64::decode(buf)?,
                salt: i64::decode(buf)?,
                signature: if bool::decode(buf)? { Some(read_signature(buf)?) } else { None },
                last_seen: LastSeenUpdate::decode(buf)?,
            })
        }
    }
    packet!(Chat, play, serverbound, CHAT);

    /// `ServerboundChatCommandPacket`: a command with nothing to sign,
    /// without its slash.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct ChatCommand {
        pub command: String,
    }
    packet!(ChatCommand, play, serverbound, CHAT_COMMAND);

    /// `ServerboundChatCommandSignedPacket`: a command with `message`
    /// arguments, each signed if the client has a chat session.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChatCommandSigned {
        pub command: String,
        pub timestamp: i64,
        pub salt: i64,
        /// `ArgumentSignatures`: argument name (at most 16 characters) and
        /// signature; at most 8.
        pub signatures: Vec<(String, MessageSignature)>,
        pub last_seen: LastSeenUpdate,
    }

    impl Encode for ChatCommandSigned {
        fn encode(&self, out: &mut Vec<u8>) {
            self.command.encode(out);
            self.timestamp.encode(out);
            self.salt.encode(out);
            VarInt(self.signatures.len() as i32).encode(out);
            for (name, signature) in &self.signatures {
                name.encode(out);
                out.extend_from_slice(&signature[..]);
            }
            self.last_seen.encode(out);
        }
    }

    impl Decode for ChatCommandSigned {
        fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
            let command = String::decode(buf)?;
            let timestamp = i64::decode(buf)?;
            let salt = i64::decode(buf)?;
            let count = VarInt::decode(buf)?.0;
            let mut signatures = Vec::new();
            for _ in 0..count {
                signatures.push((String::decode(buf)?, read_signature(buf)?));
            }
            Ok(Self { command, timestamp, salt, signatures, last_seen: LastSeenUpdate::decode(buf)? })
        }
    }
    packet!(ChatCommandSigned, play, serverbound, CHAT_COMMAND_SIGNED);

    /// `ServerboundChatAckPacket`: sent when more than 64 signed messages
    /// arrived without the client saying anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ChatAck {
        #[var]
        pub offset: i32,
    }
    packet!(ChatAck, play, serverbound, CHAT_ACK);

    /// `ServerboundPlayerActionPacket`: digging and item actions. `pos` is
    /// a packed `BlockPos`, `direction` a `Direction` ordinal; `sequence`
    /// is zero unless the action predicts a block change.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PlayerAction {
        pub action: PlayerActionKind,
        pub pos: i64,
        pub direction: u8,
        #[var]
        pub sequence: i32,
    }
    packet!(PlayerAction, play, serverbound, PLAYER_ACTION);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub enum PlayerActionKind {
        StartDestroyBlock,
        ChangeDestroyDirection,
        AbortDestroyBlock,
        StopDestroyBlock,
        DropAllItems,
        DropItem,
        ReleaseUseItem,
        SwapItemWithOffhand,
        Stab,
    }

    /// `ServerboundAttackPacket`: a left click on an entity.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct Attack {
        #[var]
        pub entity_id: i32,
    }
    packet!(Attack, play, serverbound, ATTACK);

    /// `ServerboundPunchPacket`: the arm swung at something.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct Punch;
    packet!(Punch, play, serverbound, PUNCH);

    /// `ServerboundSetCarriedItemPacket`: the selected hotbar slot, 0 to 8.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct SetCarriedItem {
        pub slot: i16,
    }
    packet!(SetCarriedItem, play, serverbound, SET_CARRIED_ITEM);

    /// `ServerboundContainerClosePacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ContainerClose {
        #[var]
        pub container_id: i32,
    }
    packet!(ContainerClose, play, serverbound, CONTAINER_CLOSE);

    /// `ServerboundChatSessionUpdatePacket` (`RemoteChatSession.Data`).
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct ChatSessionUpdate {
        /// Random per session (`LocalChatSession.create`).
        pub session_id: uuid::Uuid,
        /// `ProfilePublicKey.Data`: expiry in Unix milliseconds,
        pub expires_at: i64,
        /// the X.509 public key (at most 512 bytes),
        pub public_key: rapidbot_buf::ByteArray,
        /// and Mojang's signature over it (at most 4096 bytes).
        pub key_signature: rapidbot_buf::ByteArray,
    }
    packet!(ChatSessionUpdate, play, serverbound, CHAT_SESSION_UPDATE);

    /// `ServerboundClientCommandPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ClientCommand {
        pub action: ClientCommandAction,
    }
    packet!(ClientCommand, play, serverbound, CLIENT_COMMAND);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub enum ClientCommandAction {
        PerformRespawn,
        RequestStats,
        RequestGameruleValues,
    }

    /// `ServerboundConfigurationAcknowledgedPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ConfigurationAcknowledged;
    packet!(ConfigurationAcknowledged, play, serverbound, CONFIGURATION_ACKNOWLEDGED);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lp_vec3_roundtrip() {
        for v in [(0.0, 0.0, 0.0), (0.1, -0.0784, 0.0), (0.0, 0.42, -0.9), (3.5, -12.0, 100.25)] {
            let lp = LpVec3 { x: v.0, y: v.1, z: v.2 };
            let bytes = rapidbot_buf::to_bytes(&lp);
            let back = LpVec3::decode(&mut bytes.as_slice()).unwrap();
            let scale = v.0.abs().max(v.1.abs()).max(v.2.abs()).ceil().max(1.0);
            for (a, b) in [(back.x, v.0), (back.y, v.1), (back.z, v.2)] {
                assert!((a - b).abs() <= scale / 16000.0, "{a} vs {b}");
            }
        }
        assert_eq!(rapidbot_buf::to_bytes(&LpVec3::default()), [0]);
    }
}
