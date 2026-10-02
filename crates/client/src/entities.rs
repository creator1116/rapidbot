//! Other entities, as the client sees them: where they are drawn (the
//! server's positions arrive every few ticks and the client glides between
//! them), how big they are, and whether the crosshair can land on them.
//!
//! Positions follow `VecDeltaCodec`: relative moves are 1/4096-block steps
//! from the entity's last base position.

use std::collections::{HashMap, VecDeque};

use rapidbot_buf::{BoundedString, ByteArray, Decode, DecodeError, VarInt};
use rapidbot_physics::math::{self, Vec3};
use rapidbot_protocol::packets::login::ProfileProperty;
use rapidbot_protocol::packets::play::{LpVec3, PositionMoveRotation};
use rapidbot_world::item::ItemStack;
use rapidbot_world::registry::{EntityKind, Interpolation};
use rapidbot_world::tags::Tags;
use rapidbot_world::Registry;
use uuid::Uuid;

/// `Pose` IDs.
pub mod pose {
    pub const STANDING: i32 = 0;
    pub const FALL_FLYING: i32 = 1;
    pub const SLEEPING: i32 = 2;
    pub const SWIMMING: i32 = 3;
    pub const SPIN_ATTACK: i32 = 4;
    pub const CROUCHING: i32 = 5;
    pub const DYING: i32 = 7;
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct PosRot {
    pos: Vec3,
    y_rot: f32,
    x_rot: f32,
}

#[derive(Debug, Clone, Copy)]
struct Step {
    at: PosRot,
    tick_offset: i32,
}

/// `PositionPath`.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionPath {
    Linear(Vec3),
    /// Positions with the ticks each takes to reach from the one before.
    Stepped(Vec<(Vec3, i32)>),
}

impl PositionPath {
    fn end(&self) -> Vec3 {
        match self {
            PositionPath::Linear(pos) => *pos,
            PositionPath::Stepped(steps) => steps.last().map(|s| s.0).unwrap_or_default(),
        }
    }

    /// `PositionPath.STREAM_CODEC`.
    fn decode(buf: &mut &[u8]) -> Result<Self, DecodeError> {
        let vec3 = |buf: &mut &[u8]| -> Result<Vec3, DecodeError> { Ok(Vec3::new(f64::decode(buf)?, f64::decode(buf)?, f64::decode(buf)?)) };
        // ByIdMap with OutOfBoundsStrategy.ZERO.
        if VarInt::decode(buf)?.0 != 1 {
            return Ok(PositionPath::Linear(vec3(buf)?));
        }
        let count = VarInt::decode(buf)?.0;
        let mut steps = Vec::new();
        for _ in 0..count {
            let pos = vec3(buf)?;
            steps.push((pos, VarInt::decode(buf)?.0));
        }
        if steps.is_empty() {
            return Err(DecodeError::Custom("empty stepped path".into()));
        }
        Ok(PositionPath::Stepped(steps))
    }
}

/// `SteppedInterpolationHandler.InterpolationData`.
#[derive(Debug, Clone)]
struct Stepped {
    target: PosRot,
    remaining_steps: VecDeque<Step>,
    last_step: PosRot,
    current_step_ticks: f32,
    remaining_ticks: f32,
    speed: f32,
}

impl Default for Stepped {
    fn default() -> Self {
        Self {
            target: PosRot::default(),
            remaining_steps: VecDeque::new(),
            last_step: PosRot::default(),
            current_step_ticks: 0.0,
            remaining_ticks: 0.0,
            speed: 1.0,
        }
    }
}

fn lerp_vec(from: Vec3, to: Vec3, a: f64) -> Vec3 {
    // Vec3.lerp: Mth.lerp(a, from, to) per axis.
    Vec3::new(from.x + a * (to.x - from.x), from.y + a * (to.y - from.y), from.z + a * (to.z - from.z))
}

/// `Mth.rotLerp(float, float, float)`.
fn rot_lerp(a: f32, from: f32, to: f32) -> f32 {
    from + a * math::wrap_degrees(to - from)
}

impl Stepped {
    fn add_step(&mut self, pos: Vec3, y_rot: f32, x_rot: f32, tick_offset: i32) {
        self.remaining_steps.push_back(Step { at: PosRot { pos, y_rot, x_rot }, tick_offset });
        self.remaining_ticks += tick_offset as f32;
    }

    /// `addSteps`.
    fn add_steps(&mut self, path: &PositionPath, y_rot: f32, x_rot: f32, interval: i32) {
        match path {
            PositionPath::Linear(pos) => self.add_step(*pos, y_rot, x_rot, interval),
            PositionPath::Stepped(steps) => {
                if y_rot == self.target.y_rot && x_rot == self.target.x_rot {
                    for (pos, ticks) in steps {
                        self.add_step(*pos, y_rot, x_rot, *ticks);
                    }
                    return;
                }
                let total: i32 = steps.iter().map(|s| s.1).sum();
                let mut offset = 0;
                for (pos, ticks) in steps {
                    offset += ticks;
                    let a = offset as f32 / total as f32;
                    let y = rot_lerp(a, self.target.y_rot, y_rot);
                    let x = math::lerp_f32(a, self.target.x_rot, x_rot);
                    self.add_step(*pos, y, x, *ticks);
                }
            }
        }
    }

    /// `getNewPositionAndRotation`.
    fn next(&mut self) -> PosRot {
        while let Some(step) = self.remaining_steps.front().copied() {
            let offset = step.tick_offset;
            if self.current_step_ticks < offset as f32 {
                let a = self.current_step_ticks / offset as f32;
                return PosRot {
                    pos: lerp_vec(self.last_step.pos, step.at.pos, a as f64),
                    y_rot: rot_lerp(a, self.last_step.y_rot, step.at.y_rot),
                    x_rot: math::lerp_f32(a, self.last_step.x_rot, step.at.x_rot),
                };
            }
            self.current_step_ticks -= offset as f32;
            self.last_step = step.at;
            self.remaining_steps.pop_front();
        }
        self.target
    }

    /// `advance` at the normal tick rate.
    fn advance(&mut self, interval: i32) {
        let mut ticks = 1.0f32;
        let target_speed = (self.remaining_ticks / interval as f32).max(1.0);
        self.speed = math::lerp_f32(1.0 / interval as f32, self.speed, target_speed);
        if ticks * self.speed < self.remaining_ticks {
            ticks *= self.speed;
        } else {
            ticks = self.remaining_ticks;
            self.speed = 1.0;
        }
        self.current_step_ticks += ticks;
        self.remaining_ticks -= ticks;
    }

    fn reset(&mut self) {
        self.remaining_steps.clear();
        self.remaining_ticks = 0.0;
        self.speed = 1.0;
    }
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub id: i32,
    pub uuid: Uuid,
    /// Registry name, e.g. `minecraft:player`.
    pub kind: &'static str,
    type_id: usize,
    /// Where the client has it this tick: the server's position reaches
    /// here a few ticks late, through the interpolation.
    pub pos: Vec3,
    pub y_rot: f32,
    pub x_rot: f32,
    /// `VecDeltaCodec` base: the last position the server sent.
    base: Vec3,
    stepped: Stepped,
    /// `LinearInterpolationHandler`: target and steps left.
    linear: (PosRot, u32),
    /// Shared flags (`DATA_SHARED_FLAGS_ID`): 1 on fire, 2 sneaking,
    /// 8 sprinting, 16 swimming, 32 invisible, 64 glowing, 128 gliding.
    pub flags: u8,
    /// A [`pose`] ID.
    pub pose: i32,
    /// Health of a living entity, once the server has sent it.
    pub health: Option<f32>,
    /// The `scale` attribute.
    pub scale: f32,
    /// `ArmorStand.DATA_CLIENT_FLAGS`.
    armor_stand_flags: u8,
    /// `AbstractArrow.IN_GROUND`.
    in_ground: bool,
}

/// `Mth.unpackDegrees`.
fn unpack_degrees(b: i8) -> f32 {
    (b as i32 * 360) as f32 / 256.0
}

impl Entity {
    pub fn is_player(&self) -> bool {
        self.kind == "minecraft:player"
    }

    /// The `minecraft:entity_type` registry ID.
    pub fn type_id(&self) -> u32 {
        self.type_id as u32
    }

    pub fn info(&self) -> &'static EntityKind {
        &Registry::get().entity_kinds[self.type_id]
    }

    pub fn is_living(&self) -> bool {
        self.info().living
    }

    /// Width and height (`Entity.getDimensions(pose)`). Babies and mobs
    /// whose size is entity data (slimes) come out at the type's size.
    pub fn dimensions(&self) -> (f32, f32) {
        let info = self.info();
        let (width, height, _) = Registry::get().entity_dimensions[self.type_id];
        let scaled = |w: f32, h: f32| if self.scale != 1.0 { (w * self.scale, h * self.scale) } else { (w, h) };
        if !info.living {
            return (width, height);
        }
        // LivingEntity.getDimensions: SLEEPING_DIMENSIONS, a fixed size.
        if self.pose == pose::SLEEPING {
            return (0.2, 0.2);
        }
        if info.is("Avatar") {
            // Avatar.POSES.
            return match self.pose {
                pose::FALL_FLYING | pose::SWIMMING | pose::SPIN_ATTACK => scaled(0.6, 0.6),
                pose::CROUCHING => scaled(0.6, 1.5),
                pose::DYING => (0.2, 0.2),
                _ => scaled(0.6, 1.8),
            };
        }
        if info.is("ArmorStand") {
            if self.armor_stand_flags & 16 != 0 {
                return (0.0, 0.0);
            }
            if self.armor_stand_flags & 1 != 0 {
                return scaled(width * 0.5, height * 0.5);
            }
        }
        scaled(width, height)
    }

    /// `EntityDimensions.makeBoundingBox`: min and max corners.
    pub fn bounding_box(&self) -> ([f64; 3], [f64; 3]) {
        let (width, height) = self.dimensions();
        let w = (width / 2.0) as f64;
        let h = height as f64;
        ([self.pos.x - w, self.pos.y, self.pos.z - w], [self.pos.x + w, self.pos.y + h, self.pos.z + w])
    }

    fn redirectable(&self, tags: &Tags) -> bool {
        tags.contains("minecraft:entity_type", "minecraft:redirectable_projectile", self.type_id as u32)
    }

    /// `Entity.isPickable`. Entities whose box is not the plain one around
    /// their position (item frames, paintings, `interaction` entities,
    /// dragon parts) are left out.
    pub fn is_pickable(&self, tags: &Tags) -> bool {
        let info = self.info();
        if info.is("ArmorStand") {
            return self.armor_stand_flags & 16 == 0;
        }
        if info.is("EnderDragon") {
            return false;
        }
        if info.living {
            return true;
        }
        if info.is("ShulkerBullet") {
            return true;
        }
        if info.is("Projectile") {
            return self.redirectable(tags) && !(info.is("AbstractArrow") && self.in_ground);
        }
        ["AbstractBoat", "AbstractMinecart", "PrimedTnt", "FallingBlockEntity", "EndCrystal"].iter().any(|c| info.is(c))
    }

    /// `Entity.getPickRadius`: pickable projectiles have a generous box.
    pub fn pick_radius(&self, tags: &Tags) -> f32 {
        if self.info().is("Projectile") && self.is_pickable(tags) { 1.0 } else { 0.0 }
    }

    /// `Entity.isAttackable`.
    pub fn is_attackable(&self, tags: &Tags) -> bool {
        let info = self.info();
        if info.is("AbstractArrow") {
            return self.redirectable(tags);
        }
        !["ExperienceOrb", "ItemEntity", "FallingBlockEntity", "FireworkRocketEntity", "EyeOfEnder"].iter().any(|c| info.is(c))
    }

    /// `Entity.hurtClient` for a player's attack: whether the client
    /// itself counts the hit as landed. True for other players and
    /// vehicles; for mobs only the server knows.
    pub fn hurt_client(&self) -> bool {
        let info = self.info();
        ["Player", "VehicleEntity", "ShulkerBullet", "EndCrystal"].iter().any(|c| info.is(c))
    }

    fn interval(&self) -> i32 {
        self.info().update_interval.min(i32::MAX as u32) as i32
    }

    fn snap_to(&mut self, pos: Vec3, y_rot: f32, x_rot: f32) {
        self.pos = pos;
        self.y_rot = y_rot;
        self.x_rot = x_rot;
        self.stepped.reset();
        self.linear.1 = 0;
    }

    fn interpolating(&self) -> bool {
        match self.info().interpolation {
            Interpolation::Snap => false,
            Interpolation::Linear(_) => self.linear.1 > 0,
            Interpolation::Stepped => !self.stepped.remaining_steps.is_empty(),
        }
    }

    /// `Entity.moveOrInterpolateTo`.
    fn move_or_interpolate_to(&mut self, path: Option<PositionPath>, rot: Option<(f32, f32)>) {
        let interpolation = self.info().interpolation;
        if interpolation == Interpolation::Snap {
            if let Some(path) = path {
                self.pos = path.end();
            }
            if let Some((y_rot, x_rot)) = rot {
                self.y_rot = y_rot;
                self.x_rot = x_rot;
            }
            return;
        }
        // AbstractInterpolationHandler.interpolateTo.
        let active = self.interpolating();
        let current = match (active, interpolation) {
            (true, Interpolation::Stepped) => self.stepped.target,
            (true, _) => self.linear.0,
            _ => PosRot { pos: self.pos, y_rot: self.y_rot, x_rot: self.x_rot },
        };
        let path = path.unwrap_or(PositionPath::Linear(current.pos));
        let (y_rot, x_rot) = rot.unwrap_or((current.y_rot, current.x_rot));
        let end = path.end();
        let steps = match interpolation {
            Interpolation::Linear(steps) => steps as i32,
            _ => self.interval(),
        };
        if steps == 0 {
            self.snap_to(end, y_rot, x_rot);
            return;
        }
        let wanted = PosRot { pos: end, y_rot, x_rot };
        if active && current == wanted {
            return;
        }
        match interpolation {
            Interpolation::Linear(steps) => self.linear = (wanted, steps),
            _ => {
                // SteppedInterpolationHandler.startInterpolating.
                if !active {
                    self.stepped.last_step = PosRot { pos: self.pos, y_rot: self.y_rot, x_rot: self.x_rot };
                    self.stepped.current_step_ticks = 1.0;
                }
                if end == self.stepped.target.pos {
                    self.stepped.add_step(end, y_rot, x_rot, steps);
                } else {
                    self.stepped.add_steps(&path, y_rot, x_rot, steps);
                }
                self.stepped.target = wanted;
            }
        }
    }

    /// `Entity.commonTick`: `getInterpolation().interpolate()`.
    fn tick(&mut self) {
        match self.info().interpolation {
            Interpolation::Snap => {}
            Interpolation::Linear(_) => {
                // LinearInterpolationHandler.doInterpolate.
                let (target, remaining) = self.linear;
                if remaining == 0 {
                    return;
                }
                let alpha = 1.0 / remaining as f64;
                self.pos = lerp_vec(self.pos, target.pos, alpha);
                self.y_rot = (self.y_rot as f64 + alpha * math::wrap_degrees(target.y_rot - self.y_rot) as f64) as f32;
                self.x_rot = (self.x_rot as f64 + alpha * (target.x_rot as f64 - self.x_rot as f64)) as f32;
                self.linear.1 = remaining - 1;
            }
            Interpolation::Stepped => {
                if self.stepped.remaining_steps.is_empty() {
                    self.stepped.reset();
                    return;
                }
                let next = self.stepped.next();
                self.pos = next.pos;
                self.y_rot = next.y_rot;
                self.x_rot = next.x_rot;
                let interval = self.interval();
                self.stepped.advance(interval);
            }
        }
    }
}

/// A tab-list entry (`PlayerInfo`).
#[derive(Debug, Clone, Default)]
pub struct PlayerInfo {
    pub name: String,
    /// `GameType` ID: 3 is spectator.
    pub game_mode: i32,
    pub listed: bool,
    pub latency: i32,
}

/// Another player close to the bot.
#[derive(Debug, Clone)]
pub struct NearbyPlayer {
    pub name: String,
    pub uuid: Uuid,
    /// The entity ID, for [`Entities::get`].
    pub id: i32,
    pub pos: Vec3,
    pub distance: f64,
}

/// `EntityHitResult`: the entity under the crosshair and where the ray
/// meets its box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityHit {
    pub id: i32,
    pub location: [f64; 3],
}

/// One value of a `ClientboundSetEntityDataPacket`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DataValue {
    Byte(u8),
    Int(i32),
    Float(f32),
    Bool(bool),
    Pose(i32),
    Other,
}

/// The data items of a `set_entity_data` packet after the entity ID, by
/// index, up to the first whose type is not decoded (particles, variants
/// that can be sent inline, profiles).
pub(crate) fn decode_entity_data(mut body: &[u8]) -> Result<Vec<(u8, DataValue)>, DecodeError> {
    let buf = &mut body;
    let mut out = Vec::new();
    loop {
        let index = u8::decode(buf)?;
        if index == 0xff {
            return Ok(out);
        }
        // Serializer IDs, in EntityDataSerializers registration order.
        let value = match VarInt::decode(buf)?.0 {
            0 => DataValue::Byte(u8::decode(buf)?),
            1 => DataValue::Int(VarInt::decode(buf)?.0),
            2 => {
                rapidbot_buf::VarLong::decode(buf)?;
                DataValue::Other
            }
            3 => DataValue::Float(f32::decode(buf)?),
            4 => {
                String::decode(buf)?;
                DataValue::Other
            }
            5 => {
                rapidbot_nbt::Tag::decode(buf)?;
                DataValue::Other
            }
            6 => {
                if bool::decode(buf)? {
                    rapidbot_nbt::Tag::decode(buf)?;
                }
                DataValue::Other
            }
            7 => {
                ItemStack::decode(buf)?;
                DataValue::Other
            }
            8 => DataValue::Bool(bool::decode(buf)?),
            // Rotations, Vector3f.
            9 | 39 => {
                for _ in 0..3 {
                    f32::decode(buf)?;
                }
                DataValue::Other
            }
            10 => {
                i64::decode(buf)?;
                DataValue::Other
            }
            11 => {
                if bool::decode(buf)? {
                    i64::decode(buf)?;
                }
                DataValue::Other
            }
            // Direction, block states, optional unsigned int, arm, dye.
            12 | 14 | 15 | 19 | 42 | 43 => {
                VarInt::decode(buf)?;
                DataValue::Other
            }
            13 => {
                if bool::decode(buf)? {
                    Uuid::decode(buf)?;
                }
                DataValue::Other
            }
            // VillagerData: type, profession, level.
            18 => {
                for _ in 0..3 {
                    VarInt::decode(buf)?;
                }
                DataValue::Other
            }
            20 => DataValue::Pose(VarInt::decode(buf)?.0),
            40 => {
                for _ in 0..4 {
                    f32::decode(buf)?;
                }
                DataValue::Other
            }
            _ => return Ok(out),
        };
        out.push((index, value));
    }
}

#[derive(Default)]
pub struct Entities {
    by_id: HashMap<i32, Entity>,
    players: HashMap<Uuid, PlayerInfo>,
}

/// `VecDeltaCodec.encode`: `Math.round(v * 4096)`.
fn steps(v: f64) -> i64 {
    (v * 4096.0 + 0.5).floor() as i64
}

fn apply_delta(base: f64, delta: i16) -> f64 {
    if delta == 0 { base } else { (steps(base) + delta as i64) as f64 / 4096.0 }
}

/// `AABB.clip`: where the segment first enters the box.
fn clip_box(min: [f64; 3], max: [f64; 3], from: [f64; 3], to: [f64; 3]) -> Option<[f64; 3]> {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut scale = 1.0;
    rapidbot_world::raycast::box_entry(min, max, from, &mut scale, None, d)?;
    Some([from[0] + scale * d[0], from[1] + scale * d[1], from[2] + scale * d[2]])
}

impl Entities {
    pub fn get(&self, id: i32) -> Option<&Entity> {
        self.by_id.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.by_id.values()
    }

    pub fn player_info(&self, uuid: &Uuid) -> Option<&PlayerInfo> {
        self.players.get(uuid)
    }

    /// Everyone in the tab list.
    pub fn tab_list(&self) -> impl Iterator<Item = (&Uuid, &PlayerInfo)> {
        self.players.iter()
    }

    /// The name of a player entity, from the tab list.
    pub fn name_of(&self, entity: &Entity) -> Option<&str> {
        self.players.get(&entity.uuid).map(|p| p.name.as_str())
    }

    /// Other players within `radius` blocks of `from`, nearest first.
    pub fn players_near(&self, from: Vec3, radius: f64, own_id: i32) -> Vec<NearbyPlayer> {
        let mut out: Vec<NearbyPlayer> = self
            .by_id
            .values()
            .filter(|e| e.is_player() && e.id != own_id)
            .filter_map(|e| {
                let (dx, dy, dz) = (e.pos.x - from.x, e.pos.y - from.y, e.pos.z - from.z);
                let distance = (dx * dx + dy * dy + dz * dz).sqrt();
                (distance <= radius).then(|| NearbyPlayer {
                    name: self.players.get(&e.uuid).map(|p| p.name.clone()).unwrap_or_default(),
                    uuid: e.uuid,
                    id: e.id,
                    pos: e.pos,
                    distance,
                })
            })
            .collect();
        out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        out
    }

    /// `EntitySelector.CAN_BE_PICKED`: not a spectator, and pickable.
    pub fn can_be_picked(&self, entity: &Entity, tags: &Tags) -> bool {
        let spectator = entity.is_player() && self.players.get(&entity.uuid).is_some_and(|p| p.game_mode == 3);
        !spectator && entity.is_pickable(tags)
    }

    /// `ProjectileUtil.getEntityHitResult` for the crosshair: the nearest
    /// pickable entity the segment passes through, nearer than
    /// `max_distance_sq`. `search` is the box entities must touch
    /// (`Level.getEntities`).
    pub fn pick(
        &self,
        own_id: i32,
        from: [f64; 3],
        to: [f64; 3],
        search: ([f64; 3], [f64; 3]),
        max_distance_sq: f64,
        tags: &Tags,
    ) -> Option<EntityHit> {
        let mut nearest = max_distance_sq;
        let mut hovered: Option<EntityHit> = None;
        // Vanilla walks its entity sections; the order only matters between
        // entities hit at exactly the same distance. IDs make ours stable.
        let mut candidates: Vec<&Entity> = self.by_id.values().filter(|e| e.id != own_id).collect();
        candidates.sort_by_key(|e| e.id);
        for entity in candidates {
            let (min, max) = entity.bounding_box();
            let touches = (0..3).all(|i| min[i] < search.1[i] && max[i] > search.0[i]);
            if !touches || !self.can_be_picked(entity, tags) {
                continue;
            }
            let radius = entity.pick_radius(tags) as f64;
            let min = [min[0] - radius, min[1] - radius, min[2] - radius];
            let max = [max[0] + radius, max[1] + radius, max[2] + radius];
            let clip = clip_box(min, max, from, to);
            let contains = (0..3).all(|i| from[i] >= min[i] && from[i] < max[i]);
            if contains {
                if nearest >= 0.0 {
                    hovered = Some(EntityHit { id: entity.id, location: clip.unwrap_or(from) });
                    nearest = 0.0;
                }
            } else if let Some(location) = clip {
                let distance: f64 = (0..3).map(|i| (location[i] - from[i]) * (location[i] - from[i])).sum();
                if distance < nearest || nearest == 0.0 {
                    hovered = Some(EntityHit { id: entity.id, location });
                    nearest = distance;
                }
            }
        }
        hovered
    }

    /// `ClientLevel.tickEntities` for everything but the local player.
    pub fn tick(&mut self) {
        for entity in self.by_id.values_mut() {
            entity.tick();
        }
    }

    /// `ClientboundAddEntityPacket`. Returns the entity if it was added.
    pub fn add_entity(&mut self, mut body: &[u8]) -> Result<Option<&Entity>, DecodeError> {
        let buf = &mut body;
        let id = VarInt::decode(buf)?.0;
        let uuid = Uuid::decode(buf)?;
        let type_id = VarInt::decode(buf)?.0 as usize;
        let pos = Vec3::new(f64::decode(buf)?, f64::decode(buf)?, f64::decode(buf)?);
        let _movement = LpVec3::decode(buf)?;
        let x_rot = unpack_degrees(i8::decode(buf)?);
        let y_rot = unpack_degrees(i8::decode(buf)?);
        let Some(kind) = Registry::get().entity_types.get(type_id) else { return Ok(None) };
        self.by_id.insert(
            id,
            Entity {
                id,
                uuid,
                kind: kind.as_str(),
                type_id,
                pos,
                y_rot,
                x_rot,
                base: pos,
                stepped: Stepped::default(),
                linear: (PosRot::default(), 0),
                flags: 0,
                pose: pose::STANDING,
                health: None,
                scale: 1.0,
                armor_stand_flags: 0,
                in_ground: false,
            },
        );
        Ok(self.by_id.get(&id))
    }

    /// `ClientboundRemoveEntitiesPacket`.
    pub fn remove_entities(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        for id in Vec::<VarInt>::decode(&mut body)? {
            self.by_id.remove(&id.0);
        }
        Ok(())
    }

    /// `ClientboundMoveEntityPacket.Pos` / `PosRot`: entity ID, properties
    /// (on-ground bit and step count), the deltas (`VecDelta.read`), and
    /// for `PosRot` two rotation bytes.
    pub fn move_entity(&mut self, mut body: &[u8], has_rotation: bool) -> Result<(), DecodeError> {
        let buf = &mut body;
        let id = VarInt::decode(buf)?.0;
        let properties = VarInt::decode(buf)?.0;
        let step_count = (properties as u32 >> 1) as usize;
        let Some(entity) = self.by_id.get_mut(&id) else { return Ok(()) };
        let delta = |buf: &mut &[u8], base: Vec3| -> Result<Vec3, DecodeError> {
            let (xa, ya, za) = (i16::decode(buf)?, i16::decode(buf)?, i16::decode(buf)?);
            Ok(Vec3::new(apply_delta(base.x, xa), apply_delta(base.y, ya), apply_delta(base.z, za)))
        };
        // VecDelta.decode.
        let path = if step_count == 0 {
            PositionPath::Linear(delta(buf, entity.base)?)
        } else {
            let mut base = entity.base;
            let mut steps = Vec::with_capacity(step_count.min(64));
            for _ in 0..step_count {
                let ticks = VarInt::decode(buf)?.0;
                base = delta(buf, base)?;
                steps.push((base, ticks));
            }
            PositionPath::Stepped(steps)
        };
        let rot = if has_rotation { Some((unpack_degrees(i8::decode(buf)?), unpack_degrees(i8::decode(buf)?))) } else { None };
        entity.base = path.end();
        entity.move_or_interpolate_to(Some(path), rot);
        Ok(())
    }

    /// `ClientboundMoveEntityPacket.Rot`.
    pub fn rotate_entity(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let buf = &mut body;
        let id = VarInt::decode(buf)?.0;
        let _on_ground = bool::decode(buf)?;
        let rot = (unpack_degrees(i8::decode(buf)?), unpack_degrees(i8::decode(buf)?));
        if let Some(entity) = self.by_id.get_mut(&id) {
            entity.move_or_interpolate_to(None, Some(rot));
        }
        Ok(())
    }

    /// `ClientboundEntityPositionSyncPacket` (`handleEntityPositionSync`).
    pub fn position_sync(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let buf = &mut body;
        let id = VarInt::decode(buf)?.0;
        let path = PositionPath::decode(buf)?;
        let (y_rot, x_rot) = (f32::decode(buf)?, f32::decode(buf)?);
        let Some(entity) = self.by_id.get_mut(&id) else { return Ok(()) };
        let pos = path.end();
        entity.base = pos;
        let (dx, dy, dz) = (entity.pos.x - pos.x, entity.pos.y - pos.y, entity.pos.z - pos.z);
        if dx * dx + dy * dy + dz * dz > 4096.0 {
            entity.snap_to(pos, y_rot, x_rot);
        } else {
            entity.move_or_interpolate_to(Some(path), Some((y_rot, x_rot)));
        }
        Ok(())
    }

    /// `ClientboundTeleportEntityPacket` (`handleTeleportEntity`).
    pub fn teleport_entity(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        let buf = &mut body;
        let id = VarInt::decode(buf)?.0;
        let change = PositionMoveRotation::decode(buf)?;
        let relatives = i32::decode(buf)?;
        let Some(entity) = self.by_id.get_mut(&id) else { return Ok(()) };
        // PositionMoveRotation.calculateAbsolute against where it is now.
        let rel = |bit: i32, current: f64, v: f64| if relatives & bit != 0 { current + v } else { v };
        let pos = Vec3::new(
            rel(1, entity.pos.x, change.position.x),
            rel(2, entity.pos.y, change.position.y),
            rel(4, entity.pos.z, change.position.z),
        );
        let y_rot = if relatives & 8 != 0 { entity.y_rot + change.y_rot } else { change.y_rot };
        let x_rot = if relatives & 16 != 0 { entity.x_rot + change.x_rot } else { change.x_rot };
        let x_rot = x_rot.clamp(-90.0, 90.0);
        let (dx, dy, dz) = (entity.pos.x - pos.x, entity.pos.y - pos.y, entity.pos.z - pos.z);
        if dx * dx + dy * dy + dz * dz > 4096.0 {
            entity.snap_to(pos, y_rot, x_rot);
        } else {
            entity.move_or_interpolate_to(Some(PositionPath::Linear(pos)), Some((y_rot, x_rot)));
        }
        Ok(())
    }

    /// `ClientboundSetEntityDataPacket` for another entity: `body` starts
    /// after the entity ID.
    pub(crate) fn set_entity_data(&mut self, id: i32, body: &[u8]) -> Result<(), DecodeError> {
        let Some(entity) = self.by_id.get_mut(&id) else { return Ok(()) };
        let info = entity.info();
        for (index, value) in decode_entity_data(body)? {
            match (index, value) {
                (0, DataValue::Byte(flags)) => entity.flags = flags,
                (6, DataValue::Pose(pose)) => entity.pose = pose,
                (9, DataValue::Float(health)) if info.living => entity.health = Some(health),
                (15, DataValue::Byte(flags)) if info.is("ArmorStand") => entity.armor_stand_flags = flags,
                (10, DataValue::Bool(in_ground)) if info.is("AbstractArrow") => entity.in_ground = in_ground,
                _ => {}
            }
        }
        Ok(())
    }

    /// The `scale` attribute of another entity, from `update_attributes`.
    pub(crate) fn set_scale(&mut self, id: i32, scale: f64) {
        if let Some(entity) = self.by_id.get_mut(&id) {
            entity.scale = scale as f32;
        }
    }

    /// `ClientboundPlayerInfoUpdatePacket`: an action bit set, then per
    /// player a UUID and the data of each action in order. Returns the
    /// names of newly added players.
    pub fn player_info_update(&mut self, mut body: &[u8]) -> Result<Vec<String>, DecodeError> {
        let buf = &mut body;
        let actions = u8::decode(buf)?;
        let count = VarInt::decode(buf)?.0;
        let mut added = Vec::new();
        for _ in 0..count {
            let uuid = Uuid::decode(buf)?;
            if actions & 1 != 0 {
                let name = BoundedString::<16>::decode(buf)?.0;
                let _properties = Vec::<ProfileProperty>::decode(buf)?;
                added.push(name.clone());
                self.players.entry(uuid).or_default().name = name;
            }
            let info = self.players.entry(uuid).or_default();
            if actions & 2 != 0 && bool::decode(buf)? {
                // RemoteChatSession.Data: session ID, expiry, key, signature.
                Uuid::decode(buf)?;
                i64::decode(buf)?;
                ByteArray::decode(buf)?;
                ByteArray::decode(buf)?;
            }
            if actions & 4 != 0 {
                info.game_mode = VarInt::decode(buf)?.0;
            }
            if actions & 8 != 0 {
                info.listed = bool::decode(buf)?;
            }
            if actions & 16 != 0 {
                info.latency = VarInt::decode(buf)?.0;
            }
            if actions & 32 != 0 && bool::decode(buf)? {
                rapidbot_nbt::Tag::decode(buf)?;
            }
            if actions & 64 != 0 {
                VarInt::decode(buf)?;
            }
            if actions & 128 != 0 {
                bool::decode(buf)?;
            }
        }
        Ok(added)
    }

    /// `ClientboundPlayerInfoRemovePacket`.
    pub fn player_info_remove(&mut self, mut body: &[u8]) -> Result<(), DecodeError> {
        for uuid in Vec::<Uuid>::decode(&mut body)? {
            self.players.remove(&uuid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_buf::Encode;

    fn type_id(name: &str) -> i32 {
        Registry::get().entity_types.iter().position(|n| n == name).unwrap() as i32
    }

    fn add(e: &mut Entities, id: i32, kind: &str, pos: [f64; 3]) {
        let mut add = Vec::new();
        VarInt(id).encode(&mut add);
        Uuid::from_u128(id as u128).encode(&mut add);
        VarInt(type_id(kind)).encode(&mut add);
        for v in pos {
            v.encode(&mut add);
        }
        // No movement, three rotation bytes, data.
        add.extend_from_slice(&[0, 0, 0, 0, 0]);
        e.add_entity(&add).unwrap().unwrap();
    }

    fn mv(e: &mut Entities, id: i32, delta: [i16; 3]) {
        let mut body = Vec::new();
        VarInt(id).encode(&mut body);
        VarInt(1).encode(&mut body);
        for v in delta {
            v.encode(&mut body);
        }
        e.move_entity(&body, false).unwrap();
    }

    #[test]
    fn relative_moves_use_4096ths() {
        assert_eq!(apply_delta(10.0, 4096), 11.0);
        assert_eq!(apply_delta(10.0, -2048), 9.5);
        assert_eq!(apply_delta(10.3, 0), 10.3);
    }

    #[test]
    fn tracks_players_nearby() {
        let mut e = Entities::default();
        let uuid = Uuid::from_u128(42);

        let mut info = vec![1u8 | 4, 1];
        uuid.encode(&mut info);
        "Admin".encode(&mut info);
        info.push(0); // no properties
        info.push(3); // spectator
        assert_eq!(e.player_info_update(&info).unwrap(), ["Admin"]);

        add(&mut e, 42, "minecraft:player", [5.0, 64.0, 0.0]);
        assert!(e.get(42).unwrap().is_player());

        let near = e.players_near(Vec3::new(0.0, 64.0, 0.0), 10.0, 1);
        assert_eq!(near.len(), 1);
        assert_eq!((near[0].name.as_str(), near[0].id), ("Admin", 42));
        assert_eq!(near[0].distance, 5.0);
        assert!(e.players_near(Vec3::new(0.0, 64.0, 0.0), 3.0, 1).is_empty());
        assert!(e.players_near(Vec3::new(0.0, 64.0, 0.0), 10.0, 42).is_empty());
        // A spectator cannot be picked.
        assert!(!e.can_be_picked(e.get(42).unwrap(), &Tags::default()));
    }

    /// A player (update interval 2) moved two blocks: the client gets there
    /// over the following ticks, not at once, and ends exactly on target.
    #[test]
    fn living_entities_glide_to_the_servers_position() {
        let mut e = Entities::default();
        add(&mut e, 7, "minecraft:player", [5.0, 64.0, 0.0]);
        mv(&mut e, 7, [-8192, 0, 0]);
        assert_eq!(e.get(7).unwrap().pos.x, 5.0);
        // startInterpolating sets currentStepTicks to 1 of the step's 2.
        e.tick();
        assert_eq!(e.get(7).unwrap().pos.x, 4.0);
        e.tick();
        assert_eq!(e.get(7).unwrap().pos.x, 3.0);
        e.tick();
        assert_eq!(e.get(7).unwrap().pos.x, 3.0);
        // The next delta is relative to the server's position.
        mv(&mut e, 7, [4096, 0, 0]);
        e.tick();
        e.tick();
        assert_eq!(e.get(7).unwrap().pos.x, 4.0);
    }

    /// Boats use the three-step linear handler; items snap.
    #[test]
    fn linear_and_snapping_entities() {
        let mut e = Entities::default();
        add(&mut e, 1, "minecraft:oak_boat", [0.0, 64.0, 0.0]);
        add(&mut e, 2, "minecraft:item", [0.0, 64.0, 0.0]);
        mv(&mut e, 1, [3 * 4096, 0, 0]);
        mv(&mut e, 2, [3 * 4096, 0, 0]);
        assert_eq!(e.get(2).unwrap().pos.x, 3.0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            e.tick();
            seen.push(e.get(1).unwrap().pos.x);
        }
        assert_eq!(seen, [1.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn entity_data_sets_pose_and_size() {
        let mut e = Entities::default();
        add(&mut e, 7, "minecraft:player", [0.0, 64.0, 0.0]);
        assert_eq!(e.get(7).unwrap().dimensions(), (0.6, 1.8));
        // Flags (byte), a custom name (optional component: absent), pose
        // crouching, health.
        let data = [0u8, 0, 0x02, 2, 6, 0, 6, 20, 5, 9, 3, 0x41, 0x20, 0, 0, 0xff];
        e.set_entity_data(7, &data).unwrap();
        let player = e.get(7).unwrap();
        assert_eq!((player.flags, player.pose, player.health), (2, pose::CROUCHING, Some(10.0)));
        assert_eq!(player.dimensions(), (0.6, 1.5));
        let (min, max) = player.bounding_box();
        assert_eq!((min[0], max[1]), (-(0.3f32 as f64), 64.0 + 1.5));
        e.set_scale(7, 2.0);
        assert_eq!(e.get(7).unwrap().dimensions(), (1.2, 3.0));
    }

    #[test]
    fn crosshair_pick() {
        let mut e = Entities::default();
        let tags = Tags::default();
        add(&mut e, 7, "minecraft:zombie", [3.0, 64.0, 0.0]);
        add(&mut e, 8, "minecraft:zombie", [5.0, 64.0, 0.0]);
        add(&mut e, 9, "minecraft:item", [1.5, 65.0, 0.0]);
        add(&mut e, 10, "minecraft:arrow", [2.0, 65.0, 0.0]);
        let search = ([-10.0; 3], [100.0; 3]);
        let from = [0.0, 65.0, 0.0];
        // Items and (untagged) arrows are passed through; the nearer zombie
        // is hit on its near face.
        let hit = e.pick(1, from, [6.0, 65.0, 0.0], search, 36.0, &tags).unwrap();
        assert_eq!(hit.id, 7);
        assert_eq!(hit.location, [3.0 - 0.3f32 as f64, 65.0, 0.0]);
        // Not beyond the distance limit, and never the player itself.
        assert!(e.pick(1, from, [6.0, 65.0, 0.0], search, 4.0, &tags).is_none());
        assert_eq!(e.pick(7, from, [6.0, 65.0, 0.0], search, 36.0, &tags).unwrap().id, 8);
        // From inside a box: that entity, at the eye. (Vanilla lets a later
        // entity further along the ray replace it, so keep the ray short.)
        let inside = e.pick(1, [3.0, 65.0, 0.0], [3.2, 65.0, 0.0], search, 36.0, &tags).unwrap();
        assert_eq!((inside.id, inside.location), (7, [3.0, 65.0, 0.0]));
        // What the client counts as a landed hit.
        assert!(!e.get(7).unwrap().hurt_client());
        assert!(e.get(7).unwrap().is_attackable(&tags));
        assert!(!e.get(9).unwrap().is_attackable(&tags));
    }
}
