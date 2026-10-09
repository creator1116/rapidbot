//! The local player's movement, ported from `LocalPlayer`, `Player`,
//! `LivingEntity` and `Entity` (26.3).
//!
//! Covered: walking, sprinting (key and double-tap), sneaking with edge
//! back-off, jumping, step-up, block friction/speed/jump factors, bounce
//! restitution, crouch/standing pose changes, fall distance.
//!
//! Also covered: water and lava (heights, currents, swimming, jumping out),
//! ladders and vines, elytra gliding, and the jump boost / slow falling /
//! levitation / blindness effects.
//!
//! Blocks that act on whatever is inside them are covered too: cobwebs,
//! sweet berry bushes and powder snow (stuck multipliers), honey (sliding),
//! bubble columns, and slime's slowdown when stepped on.
//!
//! Not yet: creative flight, riding, powder snow's and scaffolding's
//! context-dependent collision shapes, entity collisions, world border,
//! firework boosts, dolphin's grace, suffocation push-out
//! (`moveTowardsClosestSpace`).

use rapidbot_world::collision::{block_collisions, collide as shapes_collide};
use rapidbot_world::fluid::{self, FluidKind};
use rapidbot_world::shape::PlacedShape;
use rapidbot_world::tags::Tags;
use rapidbot_world::{Aabb, Axis, Registry, World};

use crate::attributes::{self, Attributes, Operation};
use crate::math::{self, DEG_TO_RAD, Vec3};

/// Keys held down (`Input`), as the player's keyboard state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Keys {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub shift: bool,
    pub sprint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    Standing,
    Crouching,
    Swimming,
    FallFlying,
}

impl Pose {
    /// `Avatar.POSES`: (width, height) as floats.
    fn dimensions(self) -> (f32, f32) {
        match self {
            Pose::Standing => (0.6, 1.8),
            Pose::Crouching => (0.6, 1.5),
            Pose::Swimming | Pose::FallFlying => (0.6, 0.6),
        }
    }

    pub fn eye_height(self) -> f32 {
        match self {
            Pose::Standing => 1.62,
            Pose::Crouching => 1.27,
            Pose::Swimming | Pose::FallFlying => 0.4,
        }
    }
}

/// `EntityDimensions.makeBoundingBox`: half width computed in float.
fn make_bounding_box(pose: Pose, pos: Vec3) -> Aabb {
    let (width, height) = pose.dimensions();
    let w = (width / 2.0) as f64;
    let h = height as f64;
    Aabb::new(pos.x - w, pos.y, pos.z - w, pos.x + w, pos.y + h, pos.z + w)
}

/// `Abilities`, as the server last sent them.
#[derive(Debug, Clone, Copy)]
pub struct Abilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub may_fly: bool,
    pub instabuild: bool,
    pub flying_speed: f32,
    pub walking_speed: f32,
}

impl Default for Abilities {
    fn default() -> Self {
        Self { invulnerable: false, flying: false, may_fly: false, instabuild: false, flying_speed: 0.05, walking_speed: 0.1 }
    }
}

/// Mob effects that change movement, with their amplifiers (0 = level I).
/// Speed and slowness are not here: they arrive as attribute modifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Effects {
    pub jump_boost: Option<i32>,
    pub levitation: Option<i32>,
    pub slow_falling: bool,
    pub blindness: bool,
    pub weaving: bool,
    pub haste: Option<i32>,
    pub conduit_power: Option<i32>,
    pub mining_fatigue: Option<i32>,
}

impl Effects {
    /// Sets or clears an effect by registry name; returns false if the
    /// effect does not affect movement.
    pub fn set(&mut self, name: &str, amplifier: Option<i32>) -> bool {
        match name {
            "minecraft:jump_boost" => self.jump_boost = amplifier,
            "minecraft:levitation" => self.levitation = amplifier,
            "minecraft:slow_falling" => self.slow_falling = amplifier.is_some(),
            "minecraft:blindness" => self.blindness = amplifier.is_some(),
            "minecraft:weaving" => self.weaving = amplifier.is_some(),
            "minecraft:haste" => self.haste = amplifier,
            "minecraft:conduit_power" => self.conduit_power = amplifier,
            "minecraft:mining_fatigue" => self.mining_fatigue = amplifier,
            _ => return false,
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct Player {
    pub pos: Vec3,
    pub delta_movement: Vec3,
    pub y_rot: f32,
    pub x_rot: f32,
    bb: Aabb,

    pub on_ground: bool,
    pub horizontal_collision: bool,
    pub vertical_collision: bool,
    pub vertical_collision_below: bool,
    pub minor_horizontal_collision: bool,
    pub main_supporting_block: Option<(i32, i32, i32)>,
    on_ground_no_blocks: bool,
    pub fall_distance: f64,

    pub pose: Pose,
    pub crouching: bool,
    pub sprinting: bool,
    pub jumping: bool,
    no_jump_delay: i32,
    sprint_trigger_time: i32,
    pub xxa: f32,
    pub zza: f32,

    /// Keys the controller is holding; read into `key_presses` at
    /// `input.tick()`.
    pub held_keys: Keys,
    /// `input.keyPresses`: what was read on the last tick.
    pub key_presses: Keys,
    /// `input.moveVector` (x = left, y = forward).
    move_vector: (f32, f32),

    pub abilities: Abilities,
    pub spectator: bool,
    pub food_level: i32,
    /// `isDeadOrDying()`: health at zero. A dead player is immobile.
    pub dead: bool,
    pub effects: Effects,
    pub attributes: Attributes,
    /// `options.sprintWindow`, default 7.
    pub sprint_window: i32,

    /// `wasTouchingWater` (what `isInWater()` returns).
    pub in_water: bool,
    /// `wasEyeInWater`: eyes were in water at the start of this tick.
    was_eye_in_water: bool,
    eye_in_water: bool,
    /// Fluid height above the feet, per fluid (`getFluidHeight`).
    pub water_height: f64,
    pub lava_height: f64,
    /// Shared flag 4: sprint-swimming.
    pub swimming: bool,
    /// Shared flag 7: gliding with an elytra.
    pub fall_flying: bool,
    /// Whether a glider (elytra) is equipped. Inventory is not decoded yet,
    /// so the owner of the bot has to say.
    pub has_elytra: bool,
    /// Set on the tick gliding starts; the client sends
    /// `player_command(START_FALL_FLYING)` and clears it.
    pub started_fall_flying: bool,
    first_tick: bool,
    /// `stuckSpeedMultiplier`: set by a cobweb (or the like) this tick,
    /// applied to the next move.
    stuck_speed_multiplier: Vec3,
    /// `movementThisTick`: from, to and the requested delta of each move.
    movements: Vec<(Vec3, Vec3, Option<Vec3>)>,
    /// `oldPosition()`: where this tick started.
    old_pos: Vec3,
    /// Lava currents are three times stronger in the Nether
    /// (`EnvironmentAttributes.FAST_LAVA`).
    pub fast_lava: bool,
}

impl Player {
    /// A player as `handleLogin` creates it: at the origin, facing
    /// yaw -180, pitch 0, standing, not on the ground.
    pub fn new() -> Self {
        let pos = Vec3::ZERO;
        Self {
            pos,
            delta_movement: Vec3::ZERO,
            y_rot: -180.0,
            x_rot: 0.0,
            bb: make_bounding_box(Pose::Standing, pos),
            on_ground: false,
            horizontal_collision: false,
            vertical_collision: false,
            vertical_collision_below: false,
            minor_horizontal_collision: false,
            main_supporting_block: None,
            on_ground_no_blocks: false,
            fall_distance: 0.0,
            pose: Pose::Standing,
            crouching: false,
            sprinting: false,
            jumping: false,
            no_jump_delay: 0,
            sprint_trigger_time: 0,
            xxa: 0.0,
            zza: 0.0,
            held_keys: Keys::default(),
            key_presses: Keys::default(),
            move_vector: (0.0, 0.0),
            abilities: Abilities::default(),
            spectator: false,
            food_level: 20,
            dead: false,
            effects: Effects::default(),
            attributes: Attributes::default(),
            sprint_window: 7,
            in_water: false,
            was_eye_in_water: false,
            eye_in_water: false,
            water_height: 0.0,
            lava_height: 0.0,
            swimming: false,
            fall_flying: false,
            has_elytra: false,
            started_fall_flying: false,
            first_tick: true,
            stuck_speed_multiplier: Vec3::ZERO,
            movements: Vec::new(),
            old_pos: Vec3::ZERO,
            fast_lava: false,
        }
    }

    pub fn bounding_box(&self) -> Aabb {
        self.bb
    }

    /// `Entity.setPos`: moves and rebuilds the bounding box.
    pub fn set_pos(&mut self, pos: Vec3) {
        self.pos = pos;
        self.bb = make_bounding_box(self.pose, pos);
    }

    pub fn eye_position(&self) -> Vec3 {
        Vec3::new(self.pos.x, self.pos.y + self.pose.eye_height() as f64, self.pos.z)
    }

    /// `Entity.turn(xo, yo)`: what `MouseHandler.turnPlayer` calls with the
    /// sensitivity-scaled mouse movement. Yaw is not wrapped.
    pub fn turn(&mut self, xo: f64, yo: f64) {
        let x_delta = yo as f32 * 0.15;
        let y_delta = xo as f32 * 0.15;
        self.x_rot += x_delta;
        self.y_rot += y_delta;
        self.x_rot = math::clamp_f32(self.x_rot, -90.0, 90.0);
    }

    /// `Entity.lookAt(anchor, pos)` from a `player_look_at` packet.
    pub fn look_at(&mut self, from_eyes: bool, target: Vec3) {
        let from = if from_eyes { self.eye_position() } else { self.pos };
        let xd = target.x - from.x;
        let yd = target.y - from.y;
        let zd = target.z - from.z;
        let sd = (xd * xd + zd * zd).sqrt();
        let rad_to_deg = 180.0f32 as f64 / (std::f64::consts::PI as f32) as f64;
        self.x_rot = math::wrap_degrees((-(math::atan2(yd, sd) * rad_to_deg)) as f32);
        self.y_rot = math::wrap_degrees((math::atan2(zd, xd) * rad_to_deg) as f32 - 90.0);
    }

    /// `LivingEntity.setSprinting`: also toggles the speed modifier.
    pub fn set_sprinting(&mut self, sprinting: bool) {
        self.sprinting = sprinting;
        let speed = self.attributes.get_mut(attributes::MOVEMENT_SPEED).unwrap();
        speed.remove_modifier(attributes::SPRINTING_MODIFIER_ID);
        if sprinting {
            speed.add_modifier(
                attributes::SPRINTING_MODIFIER_ID,
                attributes::SPRINTING_MODIFIER_AMOUNT,
                Operation::AddMultipliedTotal,
            );
        }
    }

    fn is_shift_key_down(&self) -> bool {
        self.key_presses.shift
    }

    fn is_moving_slowly(&self) -> bool {
        // isCrouching() || isVisuallyCrawling(). LivingEntity counts a
        // leftover gliding pose (no room to stand up) as swimming.
        let visually_swimming = self.pose == Pose::Swimming || !self.fall_flying && self.pose == Pose::FallFlying;
        self.crouching || (visually_swimming && !self.in_water)
    }

    /// Our own `DATA_SHARED_FLAGS_ID` byte arriving from the server
    /// (`SynchedEntityData.assignValues`). The server echoes the flags it
    /// holds for us, and it alone ends a glide (`updateFallFlying` clears
    /// flag 7 on landing). Assigning the data bypasses `setSprinting`, so
    /// the sprint speed modifier is left as it is.
    pub fn apply_shared_flags(&mut self, flags: u8) {
        self.sprinting = flags & 1 << 3 != 0;
        self.swimming = flags & 1 << 4 != 0;
        self.fall_flying = flags & 1 << 7 != 0;
    }

    /// Our own `DATA_POSE` arriving from the server. The server echoes the
    /// pose it computed for us; `onSyncedDataUpdated` resizes the box at
    /// once (`refreshDimensions`), and `updatePlayerPose` puts ours back at
    /// the end of the next tick.
    pub fn apply_synced_pose(&mut self, pose: Pose) {
        self.pose = pose;
        self.bb = make_bounding_box(pose, self.pos);
    }

    /// `isInLava()`.
    pub fn in_lava(&self) -> bool {
        !self.first_tick && self.lava_height > 0.0
    }

    fn in_liquid(&self) -> bool {
        self.in_water || self.in_lava()
    }

    /// `isUnderWater()`.
    pub fn under_water(&self) -> bool {
        self.was_eye_in_water && self.in_water
    }

    /// `Entity.getLookAngle` / `calculateViewVector`.
    /// `isEyeInFluid(FluidTags.WATER)`, as of the last tick.
    pub fn eye_in_water(&self) -> bool {
        self.eye_in_water
    }

    /// `getLookAngle` / `getViewVector(1.0F)`.
    pub fn look_angle(&self) -> Vec3 {
        let real_x = self.x_rot * DEG_TO_RAD;
        let real_y = -self.y_rot * DEG_TO_RAD;
        let (y_cos, y_sin) = (math::cos(real_y as f64), math::sin(real_y as f64));
        let (x_cos, x_sin) = (math::cos(real_x as f64), math::sin(real_x as f64));
        Vec3::new((y_sin * x_cos) as f64, (-x_sin) as f64, (y_cos * x_cos) as f64)
    }

    /// `Entity.baseTick`: fluid state for this tick, currents, swimming.
    fn base_tick(&mut self, ctx: &Ctx<'_>) {
        self.was_eye_in_water = self.eye_in_water;
        self.update_fluid_interaction(ctx);
        // Player.updateSwimming → Entity.updateSwimming
        self.swimming = if self.abilities.flying {
            false
        } else if self.swimming {
            self.sprinting && self.in_water
        } else {
            let (bx, by, bz) = self.block_position();
            self.sprinting
                && self.under_water()
                && fluid::fluid_at(ctx.world, bx, by, bz).is_some_and(|f| f.kind == FluidKind::Water)
        };
        if self.in_lava() {
            self.fall_distance *= 0.5;
        }
        self.first_tick = false;
    }

    /// `Entity.updateFluidInteraction` with `EntityFluidInteraction.update`.
    fn update_fluid_interaction(&mut self, ctx: &Ctx<'_>) {
        self.water_height = 0.0;
        self.lava_height = 0.0;
        self.eye_in_water = false;
        // Per fluid: accumulated flow, number of cells, running height.
        let mut currents = [(Vec3::ZERO, 0u32, 0.0f64); 2];

        let bbox = self.bb.deflate(0.001, 0.001, 0.001);
        let (x0, y0, z0) = (math::floor(bbox.min_x), math::floor(bbox.min_y), math::floor(bbox.min_z));
        let (x1, y1, z1) = (bbox.max_x.ceil() as i32 - 1, bbox.max_y.ceil() as i32 - 1, bbox.max_z.ceil() as i32 - 1);
        // hasFluidAndLoaded: every chunk around the box must be loaded.
        let loaded = ((x0 - 1) >> 4..=(x1 + 1) >> 4)
            .all(|cx| ((z0 - 1) >> 4..=(z1 + 1) >> 4).all(|cz| ctx.world.has_chunk(cx, cz)));
        if loaded {
            let entity_y = self.bb.min_y;
            let (eye_x, eye_z) = (math::floor(self.pos.x), math::floor(self.pos.z));
            let eye_y = self.pos.y + self.pose.eye_height() as f64;
            for x in x0..=x1 {
                for y in y0..=y1 {
                    for z in z0..=z1 {
                        let Some(f) = fluid::fluid_at(ctx.world, x, y, z) else { continue };
                        let bottom = y as f64;
                        let top = bottom + fluid::height(ctx.world, f, x, y, z) as f64;
                        if top < bbox.min_y {
                            continue;
                        }
                        if f.kind == FluidKind::Water && x == eye_x && z == eye_z && eye_y >= bottom {
                            let camera_top = bottom + fluid::height_for_camera(ctx.world, f, x, y, z) as f64;
                            if eye_y <= camera_top {
                                self.eye_in_water = true;
                            }
                        }
                        let (height, slot) = match f.kind {
                            FluidKind::Water => (&mut self.water_height, 0),
                            FluidKind::Lava => (&mut self.lava_height, 1),
                        };
                        *height = rapidbot_world::aabb::java_max(top - entity_y, *height);
                        let tracker_height = *height;
                        let current = &mut currents[slot];
                        let flow = fluid::flow(ctx.world, ctx.tags, f, x, y, z);
                        let mut flow = Vec3::new(flow[0], flow[1], flow[2]);
                        current.2 = rapidbot_world::aabb::java_max(tracker_height, current.2);
                        if current.2 < 0.4 {
                            flow = flow.scale(current.2);
                        }
                        current.0 = current.0.add(flow);
                        current.1 += 1;
                    }
                }
            }
        }

        let in_water = self.water_height > 0.0;
        if in_water {
            self.fall_distance = 0.0;
        }
        self.in_water = in_water;
        if in_water {
            self.apply_current(currents[0], 0.014);
        }
        if self.lava_height > 0.0 {
            self.apply_current(currents[1], if self.fast_lava { 0.007 } else { 0.0023333333333333335 });
        }
    }

    /// `CurrentAccumulator.applyTo` for a player.
    fn apply_current(&mut self, (accumulated, count, _): (Vec3, u32, f64), scale: f64) {
        if count == 0 || accumulated.length_sqr() < 1.0e-5f32 as f64 {
            return;
        }
        let mut impulse = accumulated.scale(1.0 / count as f64).scale(scale);
        let old = self.delta_movement;
        if old.x.abs() < 0.003 && old.z.abs() < 0.003 && impulse.length_sqr().sqrt() < 0.0045000000000000005 {
            impulse = impulse.normalize().scale(0.0045000000000000005);
        }
        self.delta_movement = old.add(impulse);
    }

    /// `LivingEntity.onClimbable`.
    fn on_climbable(&self, ctx: &Ctx<'_>) -> bool {
        if self.spectator {
            return false;
        }
        let (x, y, z) = self.block_position();
        let state = ctx.world.block_state_or_air(x, y, z);
        let block = ctx.registry.block_of(state);
        if self.fall_flying && ctx.tags.block_is("minecraft:can_glide_through", block.id) {
            return false;
        }
        if ctx.tags.block_is("minecraft:climbable", block.id) {
            return true;
        }
        // trapdoorUsableAsLadder: an open trapdoor over a ladder facing the
        // same way.
        if block.name.ends_with("_trapdoor") && ctx.registry.property(state, "open") == Some("true") {
            let below = ctx.world.block_state_or_air(x, y - 1, z);
            return ctx.registry.block_of(below).name == "minecraft:ladder"
                && ctx.registry.property(below, "facing") == ctx.registry.property(state, "facing");
        }
        false
    }

    fn has_forward_impulse(&self) -> bool {
        self.move_vector.1 > 1.0e-5
    }

    /// `LocalPlayer.tick` → `Player.tick` → `LivingEntity.tick`, movement
    /// parts, for a player whose client has loaded.
    pub fn tick(&mut self, world: &World, tags: &Tags) {
        let ctx = Ctx { world, registry: Registry::get(), tags };

        // Player.tick
        if self.spectator {
            self.set_on_ground(&ctx, false);
        }
        self.old_pos = self.pos;
        self.base_tick(&ctx);

        // LivingEntity.tick → aiStep
        self.local_ai_step(&ctx);

        // Player.tick, after super.tick()
        let nx = math::clamp_f64(self.pos.x, -2.9999999e7, 2.9999999e7);
        let nz = math::clamp_f64(self.pos.z, -2.9999999e7, 2.9999999e7);
        if nx != self.pos.x || nz != self.pos.z {
            self.set_pos(Vec3::new(nx, self.pos.y, nz));
        }
        self.update_player_pose(&ctx);
    }

    /// `LocalPlayer.aiStep`.
    fn local_ai_step(&mut self, ctx: &Ctx<'_>) {
        if self.sprint_trigger_time > 0 {
            self.sprint_trigger_time -= 1;
        }
        let was_jumping = self.key_presses.jump;
        let was_shift_key_down = self.key_presses.shift;
        let has_forward_impulse = self.has_forward_impulse();

        // Uses last tick's keys: input.tick() comes after.
        self.crouching = !self.abilities.flying
            && !self.swimming
            && self.can_fit(ctx, Pose::Crouching)
            && (self.is_shift_key_down() || !self.can_fit(ctx, Pose::Standing));

        // KeyboardInput.tick
        self.key_presses = self.held_keys;
        let impulse = |pos: bool, neg: bool| if pos == neg { 0.0 } else if pos { 1.0 } else { -1.0f32 };
        let forward = impulse(self.key_presses.forward, self.key_presses.backward);
        let left = impulse(self.key_presses.left, self.key_presses.right);
        self.move_vector = vec2_normalized(left, forward);

        if was_shift_key_down || self.key_presses.backward {
            self.sprint_trigger_time = 0;
        }

        if self.can_start_sprinting() {
            if !has_forward_impulse {
                if self.sprint_trigger_time > 0 {
                    self.set_sprinting(true);
                } else {
                    self.sprint_trigger_time = self.sprint_window;
                }
            }
            if self.key_presses.sprint {
                self.set_sprinting(true);
            }
        }

        if self.sprinting {
            let stop = if self.swimming { self.should_stop_swim_sprinting() } else { self.should_stop_run_sprinting() };
            if stop {
                self.set_sprinting(false);
            }
        }

        // (Creative/spectator flight toggling is not modelled.)

        // A fresh press of jump in the air opens the elytra.
        self.started_fall_flying = false;
        if self.key_presses.jump && !was_jumping && !self.on_climbable(ctx) && self.try_to_start_fall_flying() {
            self.started_fall_flying = true;
        }

        if self.in_water && self.key_presses.shift && !self.abilities.flying {
            // goDownInWater
            self.delta_movement = self.delta_movement.add(Vec3::new(0.0, -0.04f32 as f64, 0.0));
        }

        self.player_ai_step(ctx);
    }

    /// `LocalPlayer.isSprintingPossible`.
    fn is_sprinting_possible(&self, allowed_in_shallow_water: bool) -> bool {
        // !isMobilityRestricted() (blindness) && hasEnoughFood && ...
        !self.effects.blindness
            && (self.food_level > 6 || self.abilities.may_fly)
            && (allowed_in_shallow_water || !(self.in_water && !self.under_water()))
    }

    fn can_start_sprinting(&self) -> bool {
        !self.sprinting
            && self.has_forward_impulse()
            && self.is_sprinting_possible(self.abilities.flying)
            && (!self.fall_flying || self.under_water())
            && (!self.is_moving_slowly() || self.under_water())
    }

    fn should_stop_run_sprinting(&self) -> bool {
        !self.is_sprinting_possible(self.abilities.flying)
            || !self.has_forward_impulse()
            || self.horizontal_collision && !self.minor_horizontal_collision
    }

    fn should_stop_swim_sprinting(&self) -> bool {
        !self.is_sprinting_possible(true)
            || !self.in_water
            || !self.has_forward_impulse() && !self.on_ground && !self.key_presses.shift
    }

    /// `Player.tryToStartFallFlying` with `canGlide`.
    fn try_to_start_fall_flying(&mut self) -> bool {
        let can_glide =
            !self.abilities.flying && !self.on_ground && self.effects.levitation.is_none() && self.has_elytra;
        if !self.fall_flying && can_glide && !self.in_liquid() {
            self.fall_flying = true;
            true
        } else {
            false
        }
    }

    /// `Player.aiStep` → `LivingEntity.aiStep`.
    fn player_ai_step(&mut self, ctx: &Ctx<'_>) {
        if self.abilities.flying {
            self.fall_distance = 0.0;
        }
        self.living_ai_step(ctx);
    }

    /// `LivingEntity.aiStep`.
    fn living_ai_step(&mut self, ctx: &Ctx<'_>) {
        if self.no_jump_delay > 0 {
            self.no_jump_delay -= 1;
        }

        let m = self.delta_movement;
        let (mut dx, mut dy, mut dz) = (m.x, m.y, m.z);
        if m.horizontal_distance_sqr() < 9.0e-6 {
            dx = 0.0;
            dz = 0.0;
        }
        if m.y.abs() < 0.003 {
            dy = 0.0;
        }
        self.delta_movement = Vec3::new(dx, dy, dz);

        self.apply_input();

        if self.jumping && !self.abilities.flying {
            let in_lava = self.in_lava();
            let fluid_height = if in_lava { self.lava_height } else { self.water_height };
            let in_water_with_height = self.in_water && fluid_height > 0.0;
            // getFluidJumpThreshold
            let threshold = if (self.pose.eye_height() as f64) < 0.4 { 0.0 } else { 0.4 };
            if !in_water_with_height || self.on_ground && !(fluid_height > threshold) {
                if !in_lava || self.on_ground && self.lava_height <= threshold {
                    if (self.on_ground || in_water_with_height && fluid_height <= threshold) && self.no_jump_delay == 0 {
                        self.jump_from_ground(ctx);
                        self.no_jump_delay = 10;
                    }
                } else {
                    // jumpInLiquid(LAVA)
                    self.delta_movement = self.delta_movement.add(Vec3::new(0.0, 0.04f32 as f64, 0.0));
                }
            } else {
                // jumpInLiquid(WATER)
                self.delta_movement = self.delta_movement.add(Vec3::new(0.0, 0.04f32 as f64, 0.0));
            }
        } else {
            self.no_jump_delay = 0;
        }

        let input = Vec3::new(self.xxa as f64, 0.0, self.zza as f64);
        if self.effects.slow_falling || self.effects.levitation.is_some() {
            self.fall_distance = 0.0;
        }
        self.travel(ctx, input);
        self.apply_effects_from_blocks(ctx);
    }

    /// `Entity.applyEffectsFromBlocks`: the block stood on, then every
    /// block the box passed through this tick.
    fn apply_effects_from_blocks(&mut self, ctx: &Ctx<'_>) {
        let mut movements = std::mem::take(&mut self.movements);
        match movements.last() {
            None => movements.push((self.old_pos, self.pos, None)),
            Some(&(_, to, _)) => {
                let d = to.add(self.pos.scale(-1.0));
                if d.length_sqr() > 9.9999994e-11f32 as f64 {
                    movements.push((to, self.pos, None));
                }
            }
        }
        // isAffectedByBlocks
        if self.spectator {
            return;
        }
        if self.on_ground {
            // getOnPosLegacy → Block.stepOn. Only slime changes movement.
            let below = self.on_pos(ctx, 0.2);
            if ctx.block_at(below).name == "minecraft:slime_block" {
                let abs_y = self.delta_movement.y.abs();
                if abs_y < 0.1 && !self.is_shift_key_down() {
                    let scale = 0.4 + abs_y * 0.2;
                    self.delta_movement = self.delta_movement.multiply(scale, 1.0, scale);
                }
            }
        }

        // checkInsideBlocks: each move is retraced one axis at a time, in
        // `Direction.axisStepOrder`.
        let mut visited: Vec<(i32, i32, i32)> = Vec::new();
        for (from, to, original) in movements {
            let delta = to.add(from.scale(-1.0));
            match original {
                Some(original) if delta.length_sqr() > 0.0 => {
                    let steps = if original.x.abs() < original.z.abs() {
                        [(0.0, delta.y, 0.0), (0.0, 0.0, delta.z), (delta.x, 0.0, 0.0)]
                    } else {
                        [(0.0, delta.y, 0.0), (delta.x, 0.0, 0.0), (0.0, 0.0, delta.z)]
                    };
                    let mut pos = from;
                    for (x, y, z) in steps {
                        if x != 0.0 || y != 0.0 || z != 0.0 {
                            let next = pos.add(Vec3::new(x, y, z));
                            self.check_inside_blocks(ctx, pos, next, &mut visited);
                            pos = next;
                        }
                    }
                }
                _ => self.check_inside_blocks(ctx, from, to, &mut visited),
            }
        }
    }

    /// `Entity.checkInsideBlocks` for one straight piece of movement. The
    /// blocks `forEachBlockIntersectedBetween` visits are those the box
    /// touches at either end or sweeps through; for a move along one axis
    /// that is everything inside the hull of the two boxes. (Vanilla's cap
    /// of 16 steps only matters at speeds a player on foot never reaches.)
    fn check_inside_blocks(&mut self, ctx: &Ctx<'_>, from: Vec3, to: Vec3, visited: &mut Vec<(i32, i32, i32)>) {
        let shrink = 1.0e-5f32 as f64;
        let at_target = make_bounding_box(self.pose, to).deflate(shrink, shrink, shrink);
        let travel = to.add(from.scale(-1.0));
        let moved_far = travel.length_sqr() > 0.9999900000002526 * 0.9999900000002526;
        let hull = if travel.length_sqr() < (1.0e-5f32 as f64) * (1.0e-5f32 as f64) {
            at_target
        } else {
            at_target.expand_towards(-travel.x, -travel.y, -travel.z)
        };
        for x in math::floor(hull.min_x)..=math::floor(hull.max_x) {
            for y in math::floor(hull.min_y)..=math::floor(hull.max_y) {
                for z in math::floor(hull.min_z)..=math::floor(hull.max_z) {
                    let state = ctx.world.block_state_or_air(x, y, z);
                    if ctx.registry.state(state).is_air {
                        continue;
                    }
                    let name = ctx.registry.block_of(state).name.as_str();
                    // Only blocks whose `entityInside` changes movement.
                    // All of them use the whole cell as their inside shape.
                    if !matches!(
                        name,
                        "minecraft:cobweb"
                            | "minecraft:sweet_berry_bush"
                            | "minecraft:powder_snow"
                            | "minecraft:honey_block"
                            | "minecraft:bubble_column"
                    ) || visited.contains(&(x, y, z))
                    {
                        continue;
                    }
                    visited.push((x, y, z));
                    let is_precise = moved_far
                        || at_target.intersects(x as f64, y as f64, z as f64, x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0);
                    self.entity_inside(ctx, name, state, (x, y, z), is_precise);
                }
            }
        }
    }

    /// `Block.entityInside` for the blocks that push the player about.
    fn entity_inside(&mut self, ctx: &Ctx<'_>, name: &str, state: u32, pos: (i32, i32, i32), is_precise: bool) {
        match name {
            // WebBlock
            "minecraft:cobweb" => {
                let multiplier =
                    if self.effects.weaving { Vec3::new(0.5, 0.25, 0.5) } else { Vec3::new(0.25, 0.05f32 as f64, 0.25) };
                self.make_stuck_in_block(multiplier);
            }
            // SweetBerryBushBlock
            "minecraft:sweet_berry_bush" => {
                self.make_stuck_in_block(Vec3::new(0.8f32 as f64, 0.75, 0.8f32 as f64));
            }
            // PowderSnowBlock: only once the feet are in it.
            "minecraft:powder_snow" => {
                if ctx.block_at(self.block_position()).name == "minecraft:powder_snow" {
                    self.make_stuck_in_block(Vec3::new(0.9f32 as f64, 1.5, 0.9f32 as f64));
                }
            }
            // HoneyBlock: sliding down its side.
            "minecraft:honey_block" => {
                let old_delta_y = self.delta_movement.y / 0.98f32 as f64 + 0.08;
                let overlap = 0.4375 + (0.6f32 / 2.0f32) as f64;
                let dx = (pos.0 as f64 + 0.5 - self.pos.x).abs();
                let dz = (pos.2 as f64 + 0.5 - self.pos.z).abs();
                let sliding = !self.on_ground
                    && !(self.pos.y > pos.1 as f64 + 0.9375 - 1.0e-7)
                    && !(old_delta_y >= -0.08)
                    && (dx + 1.0e-7 > overlap || dz + 1.0e-7 > overlap);
                if sliding {
                    let m = self.delta_movement;
                    let new_y = (-0.05 - 0.08) * 0.98f32 as f64;
                    self.delta_movement = if old_delta_y < -0.13 {
                        let factor = -0.05 / old_delta_y;
                        Vec3::new(m.x * factor, new_y, m.z * factor)
                    } else {
                        Vec3::new(m.x, new_y, m.z)
                    };
                    self.fall_distance = 0.0;
                }
            }
            // BubbleColumnBlock
            "minecraft:bubble_column" => {
                if is_precise && !self.abilities.flying {
                    let drag_down = ctx.registry.property(state, "drag") == Some("true");
                    let above = ctx.world.block_state_or_air(pos.0, pos.1 + 1, pos.2);
                    let nothing_above =
                        ctx.registry.collision_shape(above).is_none() && ctx.registry.state(above).fluid.is_empty();
                    let m = self.delta_movement;
                    let y = match (nothing_above, drag_down) {
                        (true, true) => rapidbot_world::aabb::java_max(-0.9, m.y - 0.03),
                        (true, false) => rapidbot_world::aabb::java_min(1.8, m.y + 0.1),
                        (false, true) => rapidbot_world::aabb::java_max(-0.3, m.y - 0.03),
                        (false, false) => rapidbot_world::aabb::java_min(0.7, m.y + 0.06),
                    };
                    self.delta_movement = Vec3::new(m.x, y, m.z);
                    if !nothing_above {
                        self.fall_distance = 0.0;
                    }
                }
            }
            _ => {}
        }
    }

    /// `Player.makeStuckInBlock`.
    fn make_stuck_in_block(&mut self, multiplier: Vec3) {
        if !self.abilities.flying {
            self.fall_distance = 0.0;
            self.stuck_speed_multiplier = multiplier;
        }
    }

    /// `LocalPlayer.applyInput` → `modifyInput`.
    fn apply_input(&mut self) {
        let (mut x, mut y) = self.move_vector;
        if x * x + y * y != 0.0 {
            x *= 0.98;
            y *= 0.98;
            if self.is_moving_slowly() {
                let factor = self.attributes.value(attributes::SNEAKING_SPEED) as f32;
                x *= factor;
                y *= factor;
            }
            (x, y) = modify_input_speed_for_square_movement(x, y);
        }
        self.xxa = x;
        self.zza = y;
        self.jumping = self.key_presses.jump;
        // LivingEntity.aiStep: `if (this.isImmobile())`.
        if self.dead {
            self.jumping = false;
            self.xxa = 0.0;
            self.zza = 0.0;
        }
    }

    /// `LivingEntity.jumpFromGround`.
    fn jump_from_ground(&mut self, ctx: &Ctx<'_>) {
        // getJumpPower: strength * block factor + getJumpBoostPower().
        let boost = self.effects.jump_boost.map_or(0.0, |amp| 0.1f32 * (amp as f32 + 1.0));
        let jump_power = self.attributes.value(attributes::JUMP_STRENGTH) as f32 * self.block_jump_factor(ctx) + boost;
        if jump_power > 1.0e-5 {
            let m = self.delta_movement;
            self.delta_movement = Vec3::new(m.x, rapidbot_world::aabb::java_max(jump_power as f64, m.y), m.z);
            if self.sprinting {
                let angle = self.y_rot * DEG_TO_RAD;
                self.delta_movement = self.delta_movement.add(Vec3::new(
                    -(math::sin(angle as f64) as f64) * 0.2,
                    0.0,
                    math::cos(angle as f64) as f64 * 0.2,
                ));
            }
        }
    }

    /// `Player.travel` → `LivingEntity.travel`.
    fn travel(&mut self, ctx: &Ctx<'_>, input: Vec3) {
        if self.swimming {
            let look_y = self.look_angle().y;
            let multiplier = if look_y < -0.2 { 0.085 } else { 0.06 };
            let above = (math::floor(self.pos.x), math::floor(self.pos.y + 1.0 - 0.1), math::floor(self.pos.z));
            if look_y <= 0.0 || self.jumping || fluid::fluid_at(ctx.world, above.0, above.1, above.2).is_some() {
                let m = self.delta_movement;
                self.delta_movement = m.add(Vec3::new(0.0, (look_y - m.y) * multiplier, 0.0));
            }
        }
        // shouldTravelInFluid: in a liquid and affected by fluids.
        if self.in_liquid() && !self.abilities.flying {
            self.travel_in_fluid(ctx, input);
        } else if self.fall_flying {
            self.travel_fall_flying(ctx, input);
        } else {
            self.travel_in_air(ctx, input);
        }
    }

    /// `LivingEntity.travelInFluid`.
    fn travel_in_fluid(&mut self, ctx: &Ctx<'_>, input: Vec3) {
        let is_falling = self.delta_movement.y <= 0.0;
        let old_y = self.pos.y;
        let base_gravity = self.effective_gravity();
        if self.in_water {
            // travelInWater
            let mut slow_down: f32 = if self.sprinting { 0.9 } else { 0.8 };
            let mut speed: f32 = 0.02;
            let mut water_walker = self.attributes.value(attributes::WATER_MOVEMENT_EFFICIENCY) as f32;
            if !self.on_ground {
                water_walker *= 0.5;
            }
            if water_walker > 0.0 {
                slow_down += (0.54600006f32 - slow_down) * water_walker;
                speed += (self.speed() - speed) * water_walker;
            }
            self.move_relative(speed, input);
            self.move_self(ctx, self.delta_movement);
            let mut movement = self.delta_movement;
            if self.horizontal_collision && self.on_climbable(ctx) {
                movement = Vec3::new(movement.x, 0.2, movement.z);
            }
            movement = movement.multiply(slow_down as f64, 0.8f32 as f64, slow_down as f64);
            self.delta_movement = self.fluid_falling_adjusted_movement(base_gravity, is_falling, movement);
        } else {
            // travelInLava
            self.move_relative(0.02, input);
            self.move_self(ctx, self.delta_movement);
            // isInShallowFluid(LAVA)
            let threshold = if (self.pose.eye_height() as f64) < 0.4 { 0.0 } else { 0.4 };
            if self.lava_height <= threshold {
                let m = self.delta_movement.multiply(0.5, 0.8f32 as f64, 0.5);
                self.delta_movement = self.fluid_falling_adjusted_movement(base_gravity, is_falling, m);
            } else {
                self.delta_movement = self.delta_movement.scale(0.5);
            }
            if base_gravity != 0.0 {
                self.delta_movement = self.delta_movement.add(Vec3::new(0.0, -base_gravity / 4.0, 0.0));
            }
        }
        // jumpOutOfFluid
        let m = self.delta_movement;
        if self.horizontal_collision {
            let moved = self.bb.move_by(m.x, m.y + 0.6f32 as f64 - self.pos.y + old_y, m.z);
            if ctx.no_collision(&moved) && !fluid::contains_any_liquid(ctx.world, &moved) {
                self.delta_movement = Vec3::new(m.x, 0.3f32 as f64, m.z);
            }
        }
    }

    /// `LivingEntity.getFluidFallingAdjustedMovement`.
    fn fluid_falling_adjusted_movement(&self, base_gravity: f64, is_falling: bool, movement: Vec3) -> Vec3 {
        if base_gravity != 0.0 && !self.sprinting {
            let yd = if is_falling
                && (movement.y - 0.005).abs() >= 0.003
                && (movement.y - base_gravity / 16.0).abs() < 0.003
            {
                -0.003
            } else {
                movement.y - base_gravity / 16.0
            };
            Vec3::new(movement.x, yd, movement.z)
        } else {
            movement
        }
    }

    /// `LivingEntity.travelFallFlying`.
    fn travel_fall_flying(&mut self, ctx: &Ctx<'_>, input: Vec3) {
        if self.on_climbable(ctx) {
            self.travel_in_air(ctx, input);
            // stopFallFlying()
            self.fall_flying = false;
            return;
        }
        // updateFallFlyingMovement
        let mut m = self.delta_movement;
        let look = self.look_angle();
        let lean = self.x_rot * DEG_TO_RAD;
        let look_hor = (look.x * look.x + look.z * look.z).sqrt();
        let move_hor = m.horizontal_distance_sqr().sqrt();
        let gravity = self.effective_gravity();
        let cos = (lean as f64).cos();
        let lift = cos * cos;
        m = m.add(Vec3::new(0.0, gravity * (-1.0 + lift * 0.75), 0.0));
        if m.y < 0.0 && look_hor > 0.0 {
            let convert = m.y * -0.1 * lift;
            m = m.add(Vec3::new(look.x * convert / look_hor, convert, look.z * convert / look_hor));
        }
        if lean < 0.0 && look_hor > 0.0 {
            let convert = move_hor * (-math::sin(lean as f64)) as f64 * 0.04;
            m = m.add(Vec3::new(-look.x * convert / look_hor, convert * 3.2, -look.z * convert / look_hor));
        }
        if look_hor > 0.0 {
            m = m.add(Vec3::new(
                (look.x / look_hor * move_hor - m.x) * 0.1,
                0.0,
                (look.z / look_hor * move_hor - m.z) * 0.1,
            ));
        }
        self.delta_movement = m.multiply(0.99f32 as f64, 0.98f32 as f64, 0.99f32 as f64);
        self.move_self(ctx, self.delta_movement);
    }

    /// `LivingEntity.travelInAir`.
    fn travel_in_air(&mut self, ctx: &Ctx<'_>, input: Vec3) {
        let pos_below = self.block_pos_below_that_affects_my_movement(ctx);
        let block_friction = if self.on_ground {
            let friction = ctx.block_at(pos_below).friction;
            compute_modified_friction(friction, self.attributes.value(attributes::FRICTION_MODIFIER) as f32)
        } else {
            1.0
        };

        // handleRelativeFrictionAndCalculateMovement (climbing not yet).
        let speed = self.friction_influenced_speed(block_friction);
        self.move_relative(speed, input);
        // handleOnClimbable: ladders cap speed and stop a sneaking player
        // from sliding down.
        let climbing = self.on_climbable(ctx);
        if climbing {
            self.fall_distance = 0.0;
            let d = self.delta_movement;
            let cap = 0.15f32 as f64;
            let mut yd = rapidbot_world::aabb::java_max(d.y, -cap);
            let (bx, by, bz) = self.block_position();
            let in_scaffolding =
                ctx.registry.block_of(ctx.world.block_state_or_air(bx, by, bz)).name == "minecraft:scaffolding";
            if yd < 0.0 && !in_scaffolding && self.is_shift_key_down() {
                yd = 0.0;
            }
            self.delta_movement = Vec3::new(math::clamp_f64(d.x, -cap, cap), yd, math::clamp_f64(d.z, -cap, cap));
        }
        self.move_self(ctx, self.delta_movement);
        let mut movement = self.delta_movement;
        if (self.horizontal_collision || self.jumping) && self.on_climbable(ctx) {
            movement = Vec3::new(movement.x, 0.2, movement.z);
        }

        let mut movement_y = movement.y;
        if let Some(amplifier) = self.effects.levitation {
            movement_y += (0.05 * (amplifier + 1) as f64 - movement.y) * 0.2;
        } else if ctx.world.has_chunk_at(pos_below.0, pos_below.2) {
            movement_y -= self.effective_gravity();
        } else if self.pos.y > ctx.world.height.min_y as f64 {
            movement_y = -0.1;
        } else {
            movement_y = 0.0;
        }

        let air_drag_modifier = self.attributes.value(attributes::AIR_DRAG_MODIFIER) as f32;
        let air_drag = compute_modified_friction(0.91, air_drag_modifier);
        let friction = block_friction * air_drag;
        let vertical_friction = compute_modified_friction(0.98, air_drag_modifier);
        self.delta_movement = Vec3::new(
            movement.x * friction as f64,
            movement_y * vertical_friction as f64,
            movement.z * friction as f64,
        );
    }

    /// `LivingEntity.getEffectiveGravity`: slow falling caps it while falling.
    fn effective_gravity(&self) -> f64 {
        let gravity = self.attributes.value(attributes::GRAVITY);
        if self.delta_movement.y <= 0.0 && self.effects.slow_falling {
            rapidbot_world::aabb::java_min(gravity, 0.01)
        } else {
            gravity
        }
    }

    /// `Player.getSpeed`: the attribute itself, so a sprint change applies
    /// on the same tick (unlike `LivingEntity.speed`, which lags).
    fn speed(&self) -> f32 {
        self.attributes.value(attributes::MOVEMENT_SPEED) as f32
    }

    /// `LivingEntity.getFrictionInfluencedSpeed`.
    fn friction_influenced_speed(&self, block_friction: f32) -> f32 {
        if self.on_ground {
            // `blockFriction > 0.6` compares the float widened to double, so
            // 0.6F (0.6000000238...) counts as greater.
            if block_friction as f64 > 0.6 {
                self.speed() * (0.21600002f32 / (block_friction * block_friction * block_friction))
            } else {
                self.speed()
            }
        } else if self.abilities.flying {
            if self.sprinting { self.abilities.flying_speed * 2.0 } else { self.abilities.flying_speed }
        } else if self.sprinting {
            0.025999999
        } else {
            0.02
        }
    }

    /// `Entity.moveRelative` / `getInputVector`.
    fn move_relative(&mut self, speed: f32, input: Vec3) {
        let length = input.length_sqr();
        if length < 1.0e-7 {
            return;
        }
        let movement = if length > 1.0 { input.normalize() } else { input }.scale(speed as f64);
        let angle = self.y_rot * DEG_TO_RAD;
        let sin = math::sin(angle as f64) as f64;
        let cos = math::cos(angle as f64) as f64;
        let delta = Vec3::new(movement.x * cos - movement.z * sin, movement.y, movement.z * cos + movement.x * sin);
        self.delta_movement = self.delta_movement.add(delta);
    }

    /// `Entity.move(MoverType.SELF, delta)` via `LocalPlayer.move`.
    fn move_self(&mut self, ctx: &Ctx<'_>, delta: Vec3) {
        if self.spectator {
            // noPhysics
            self.set_pos(self.pos.add(delta));
            self.horizontal_collision = false;
            self.vertical_collision = false;
            self.vertical_collision_below = false;
            self.minor_horizontal_collision = false;
            return;
        }

        let mut delta = delta;
        if self.stuck_speed_multiplier.length_sqr() > 1.0e-7 {
            let m = self.stuck_speed_multiplier;
            delta = delta.multiply(m.x, m.y, m.z);
            self.stuck_speed_multiplier = Vec3::ZERO;
            self.delta_movement = Vec3::ZERO;
        }
        let delta = self.maybe_back_off_from_edge(ctx, delta);
        let movement = self.collide(ctx, delta);
        let movement_length = movement.length_sqr();
        if movement_length > 1.0e-7 || delta.length_sqr() - movement_length < 1.0e-7 {
            if self.fall_distance != 0.0 && movement_length >= 1.0 {
                let check_distance = movement_length.sqrt().min(8.0);
                let check_to = self.pos.add(movement.normalize().scale(check_distance));
                if rapidbot_world::raycast::fall_damage_resetting(
                    ctx.world,
                    ctx.tags,
                    [self.pos.x, self.pos.y, self.pos.z],
                    [check_to.x, check_to.y, check_to.z],
                ) {
                    self.fall_distance = 0.0;
                }
            }
            let new_pos = self.pos.add(movement);
            self.movements.push((self.pos, new_pos, Some(delta)));
            self.set_pos(new_pos);
        }

        let x_collision = !math::equal(delta.x, movement.x);
        let z_collision = !math::equal(delta.z, movement.z);
        self.horizontal_collision = x_collision || z_collision;
        // The local player is authoritative, so this always runs.
        self.vertical_collision = delta.y != movement.y;
        self.vertical_collision_below = self.vertical_collision && delta.y < 0.0;
        self.set_on_ground_with_movement(ctx, self.vertical_collision_below, self.horizontal_collision, movement);

        self.minor_horizontal_collision =
            if self.horizontal_collision { self.is_horizontal_collision_minor(movement) } else { false };

        let effect_pos = self.on_pos(ctx, 0.2);
        let effect_state = ctx.world.block_state_or_air(effect_pos.0, effect_pos.1, effect_pos.2);
        self.check_fall_damage(movement.y, self.on_ground);

        let moved_vertically = delta.y.abs() > 0.0;
        if moved_vertically && self.vertical_collision || self.horizontal_collision {
            self.restitute_movement_after_collisions(ctx, effect_state, x_collision, z_collision, movement);
        }

        let factor = self.block_speed_factor(ctx) as f64;
        self.delta_movement = self.delta_movement.multiply(factor, 1.0, factor);
    }

    /// `Entity.checkFallDamage`, the fall-distance part.
    fn check_fall_damage(&mut self, ya: f64, on_ground: bool) {
        if !self.in_water && ya < 0.0 {
            self.fall_distance -= (ya as f32) as f64;
        }
        if on_ground {
            self.fall_distance = 0.0;
        }
    }

    /// `Entity.restituteMovementAfterCollisions`.
    fn restitute_movement_after_collisions(
        &mut self,
        ctx: &Ctx<'_>,
        effect_state: u32,
        x_collision: bool,
        z_collision: bool,
        movement: Vec3,
    ) {
        // Players have no entity bounciness; sneaking suppresses bounce.
        let mut restitution = 0.0f64;
        let current = self.delta_movement;
        let mut after = current;
        if x_collision {
            after = after.with(Axis::X, -current.x * restitution);
        }
        if z_collision {
            after = after.with(Axis::Z, -current.z * restitution);
        }
        if self.vertical_collision {
            if self.vertical_collision_below {
                let block = ctx.registry.block_of(effect_state);
                restitution = if !(-current.y <= self.effective_gravity())
                    && !self.is_shift_key_down()
                    && !ctx.tags.block_is("minecraft:suppresses_bounce", block.id)
                {
                    rapidbot_world::aabb::java_max(restitution, block.bounce as f64)
                } else {
                    0.0
                };
            }
            let (gravity_compensation, effective_drag) = if restitution > 0.0 {
                let portion = movement.y / current.y;
                let air_drag = compute_modified_friction(0.98, self.attributes.value(attributes::AIR_DRAG_MODIFIER) as f32);
                (portion * self.effective_gravity(), 1.0 + portion * (air_drag as f64 - 1.0))
            } else {
                (0.0, 1.0)
            };
            after = after.with(Axis::Y, (gravity_compensation - current.y) * effective_drag * restitution);
        }
        self.delta_movement = after;
    }

    /// `LocalPlayer.isHorizontalCollisionMinor`.
    fn is_horizontal_collision_minor(&self, movement: Vec3) -> bool {
        let angle = self.y_rot * DEG_TO_RAD;
        let sin = math::sin(angle as f64) as f64;
        let cos = math::cos(angle as f64) as f64;
        let global_xa = self.xxa as f64 * cos - self.zza as f64 * sin;
        let global_za = self.zza as f64 * cos + self.xxa as f64 * sin;
        let a_length_sq = global_xa * global_xa + global_za * global_za;
        let movement_length_sq = movement.x * movement.x + movement.z * movement.z;
        if a_length_sq < 1.0e-5f32 as f64 || movement_length_sq < 1.0e-5f32 as f64 {
            return false;
        }
        let dot = global_xa * movement.x + global_za * movement.z;
        let angle_between = (dot / (a_length_sq * movement_length_sq).sqrt()).acos();
        angle_between < 0.13962634f32 as f64
    }

    /// `Player.maybeBackOffFromEdge`.
    fn maybe_back_off_from_edge(&self, ctx: &Ctx<'_>, delta: Vec3) -> Vec3 {
        let max_down_step = self.attributes.value(attributes::STEP_HEIGHT) as f32;
        if self.abilities.flying || delta.y > 0.0 || !self.is_shift_key_down() || !self.is_above_ground(ctx, max_down_step)
        {
            return delta;
        }
        let max_down_step = max_down_step as f64;
        let mut dx = delta.x;
        let mut dz = delta.z;
        let step_x = java_signum(dx) * 0.05;
        let step_z = java_signum(dz) * 0.05;

        while dx != 0.0 && self.can_fall_at_least(ctx, dx, 0.0, max_down_step) {
            if dx.abs() <= 0.05 {
                dx = 0.0;
                break;
            }
            dx -= step_x;
        }
        while dz != 0.0 && self.can_fall_at_least(ctx, 0.0, dz, max_down_step) {
            if dz.abs() <= 0.05 {
                dz = 0.0;
                break;
            }
            dz -= step_z;
        }
        while dx != 0.0 && dz != 0.0 && self.can_fall_at_least(ctx, dx, dz, max_down_step) {
            if dx.abs() <= 0.05 {
                dx = 0.0;
            } else {
                dx -= step_x;
            }
            if dz.abs() <= 0.05 {
                dz = 0.0;
            } else {
                dz -= step_z;
            }
        }
        Vec3::new(dx, delta.y, dz)
    }

    fn is_above_ground(&self, ctx: &Ctx<'_>, max_down_step: f32) -> bool {
        self.on_ground
            || self.fall_distance < max_down_step as f64
                && !self.can_fall_at_least(ctx, 0.0, 0.0, max_down_step as f64 - self.fall_distance)
    }

    fn can_fall_at_least(&self, ctx: &Ctx<'_>, dx: f64, dz: f64, min_height: f64) -> bool {
        let b = self.bb;
        let area = Aabb::new(
            b.min_x + 1.0e-7 + dx,
            b.min_y - min_height - 1.0e-7,
            b.min_z + 1.0e-7 + dz,
            b.max_x - 1.0e-7 + dx,
            b.min_y,
            b.max_z - 1.0e-7 + dz,
        );
        ctx.no_collision(&area)
    }

    fn max_up_step(&self) -> f32 {
        self.attributes.value(attributes::STEP_HEIGHT) as f32
    }

    /// `Entity.collide`, with step-up.
    fn collide(&self, ctx: &Ctx<'_>, movement: Vec3) -> Vec3 {
        let aabb = self.bb;
        let movement_step = if movement.length_sqr() == 0.0 {
            movement
        } else {
            let colliders = ctx.colliders(&aabb.expand_towards(movement.x, movement.y, movement.z));
            collide_with_shapes(movement, &aabb, &colliders)
        };
        let x_collision = movement.x != movement_step.x;
        let y_collision = movement.y != movement_step.y;
        let z_collision = movement.z != movement_step.z;
        let on_ground_after_collision = y_collision && movement.y < 0.0;
        let max_up_step = self.max_up_step();

        if max_up_step > 0.0 && (on_ground_after_collision || self.on_ground) && (x_collision || z_collision) {
            let grounded = if on_ground_after_collision { aabb.move_by(0.0, movement_step.y, 0.0) } else { aabb };
            let mut step_up = grounded.expand_towards(movement.x, max_up_step as f64, movement.z);
            if !on_ground_after_collision {
                step_up = step_up.expand_towards(0.0, -1.0e-5f32 as f64, 0.0);
            }
            let colliders = ctx.colliders(&step_up);
            let step_height_to_skip = movement_step.y as f32;
            for candidate in collect_candidate_step_up_heights(&grounded, &colliders, max_up_step, step_height_to_skip) {
                let step = collide_with_shapes(Vec3::new(movement.x, candidate as f64, movement.z), &grounded, &colliders);
                if step.horizontal_distance_sqr() > movement_step.horizontal_distance_sqr() {
                    let distance_to_ground = aabb.min_y - grounded.min_y;
                    return step.subtract(0.0, distance_to_ground, 0.0);
                }
            }
        }
        movement_step
    }

    /// `Entity.setOnGroundWithMovement` → `checkSupportingBlock`.
    fn set_on_ground_with_movement(&mut self, ctx: &Ctx<'_>, on_ground: bool, horizontal: bool, movement: Vec3) {
        self.on_ground = on_ground;
        self.horizontal_collision = horizontal;
        self.check_supporting_block(ctx, on_ground, Some(movement));
    }

    fn set_on_ground(&mut self, ctx: &Ctx<'_>, on_ground: bool) {
        self.on_ground = on_ground;
        self.check_supporting_block(ctx, on_ground, None);
    }

    fn check_supporting_block(&mut self, ctx: &Ctx<'_>, on_ground: bool, movement: Option<Vec3>) {
        if on_ground {
            let b = self.bb;
            let test_area = Aabb::new(b.min_x, b.min_y - 1.0e-6, b.min_z, b.max_x, b.min_y, b.max_z);
            let mut supporting = ctx.find_supporting_block(self.pos, &test_area);
            if supporting.is_some() || self.on_ground_no_blocks {
                self.main_supporting_block = supporting;
            } else if let Some(m) = movement {
                supporting = ctx.find_supporting_block(self.pos, &test_area.move_by(-m.x, 0.0, -m.z));
                self.main_supporting_block = supporting;
            }
            self.on_ground_no_blocks = supporting.is_none();
        } else {
            self.on_ground_no_blocks = false;
            self.main_supporting_block = None;
        }
    }

    /// `Entity.getOnPos(float)`.
    pub fn on_pos(&self, ctx: &Ctx<'_>, offset: f32) -> (i32, i32, i32) {
        if let Some(support) = self.main_supporting_block {
            if !(offset > 1.0e-5) {
                return support;
            }
            let below = ctx.world.block_state_or_air(support.0, support.1, support.2);
            let block = ctx.registry.block_of(below);
            let is_fence = ctx.tags.block_is("minecraft:fences", block.id);
            let is_wall = ctx.tags.block_is("minecraft:walls", block.id);
            // `instanceof FenceGateBlock`: every fence gate is named so.
            let is_fence_gate = block.name.ends_with("_fence_gate");
            if (!(offset <= 0.5) || !is_fence) && !is_wall && !is_fence_gate {
                (support.0, math::floor(self.pos.y - offset as f64), support.2)
            } else {
                support
            }
        } else {
            (math::floor(self.pos.x), math::floor(self.pos.y - offset as f64), math::floor(self.pos.z))
        }
    }

    fn block_pos_below_that_affects_my_movement(&self, ctx: &Ctx<'_>) -> (i32, i32, i32) {
        self.on_pos(ctx, 0.500001)
    }

    fn block_position(&self) -> (i32, i32, i32) {
        (math::floor(self.pos.x), math::floor(self.pos.y), math::floor(self.pos.z))
    }

    /// `Entity.getBlockJumpFactor`.
    fn block_jump_factor(&self, ctx: &Ctx<'_>) -> f32 {
        let here = ctx.block_at(self.block_position()).jump_factor;
        let below = ctx.block_at(self.block_pos_below_that_affects_my_movement(ctx)).jump_factor;
        if here as f64 == 1.0 { below } else { here }
    }

    /// `LivingEntity.getBlockSpeedFactor` over `Entity.getBlockSpeedFactor`.
    fn block_speed_factor(&self, ctx: &Ctx<'_>) -> f32 {
        // Player.getBlockSpeedFactor: none while flying or gliding.
        if self.abilities.flying || self.fall_flying {
            return 1.0;
        }
        let here_pos = self.block_position();
        let here = ctx.block_at(here_pos);
        let base = if here.name != "minecraft:water" && here.name != "minecraft:bubble_column" {
            if here.speed_factor as f64 == 1.0 {
                ctx.block_at(self.block_pos_below_that_affects_my_movement(ctx)).speed_factor
            } else {
                here.speed_factor
            }
        } else {
            here.speed_factor
        };
        let efficiency = self.attributes.value(attributes::MOVEMENT_EFFICIENCY) as f32;
        math::lerp_f32(efficiency, base, 1.0)
    }

    /// `Player.canPlayerFitWithinBlocksAndEntitiesWhen`.
    fn can_fit(&self, ctx: &Ctx<'_>, pose: Pose) -> bool {
        ctx.no_collision(&make_bounding_box(pose, self.pos).deflate(1.0e-7, 1.0e-7, 1.0e-7))
    }

    /// `Player.updatePlayerPose`.
    fn update_player_pose(&mut self, ctx: &Ctx<'_>) {
        if !self.can_fit(ctx, Pose::Swimming) {
            return;
        }
        // getDesiredPose
        let desired = if self.swimming {
            Pose::Swimming
        } else if self.fall_flying {
            Pose::FallFlying
        } else if self.is_shift_key_down() && !self.abilities.flying {
            Pose::Crouching
        } else {
            Pose::Standing
        };
        let pose = if self.spectator || self.can_fit(ctx, desired) {
            desired
        } else if self.can_fit(ctx, Pose::Crouching) {
            Pose::Crouching
        } else {
            Pose::Swimming
        };
        if pose != self.pose {
            self.pose = pose;
            // refreshDimensions → reapplyPosition
            self.bb = make_bounding_box(pose, self.pos);
        }
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

/// World access for one tick.
pub struct Ctx<'a> {
    pub world: &'a World,
    pub registry: &'static Registry,
    pub tags: &'a Tags,
}

impl Ctx<'_> {
    fn colliders(&self, bbox: &Aabb) -> Vec<PlacedShape<'static>> {
        block_collisions(self.world, self.registry, bbox)
    }

    fn no_collision(&self, bbox: &Aabb) -> bool {
        self.colliders(bbox).is_empty()
    }

    fn block_at(&self, pos: (i32, i32, i32)) -> &'static rapidbot_world::registry::BlockInfo {
        self.registry.block_of(self.world.block_state_or_air(pos.0, pos.1, pos.2))
    }

    /// `CollisionGetter.findSupportingBlock`: the nearest colliding block
    /// (by distance to its centre), ties broken by `BlockPos.compareTo`.
    fn find_supporting_block(&self, entity_pos: Vec3, bbox: &Aabb) -> Option<(i32, i32, i32)> {
        let mut main: Option<(i32, i32, i32)> = None;
        let mut main_distance = f64::MAX;
        for shape in self.colliders(bbox) {
            let pos = (shape.offset[0] as i32, shape.offset[1] as i32, shape.offset[2] as i32);
            let dx = pos.0 as f64 + 0.5 - entity_pos.x;
            let dy = pos.1 as f64 + 0.5 - entity_pos.y;
            let dz = pos.2 as f64 + 0.5 - entity_pos.z;
            let distance = dx * dx + dy * dy + dz * dz;
            if distance < main_distance
                || distance == main_distance && main.is_none_or(|m| compare_block_pos(m, pos).is_lt())
            {
                main = Some(pos);
                main_distance = distance;
            }
        }
        main
    }
}

/// `Vec3i.compareTo`: y, then z, then x.
fn compare_block_pos(a: (i32, i32, i32), b: (i32, i32, i32)) -> std::cmp::Ordering {
    a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0))
}

/// `LivingEntity.computeModifiedFriction`.
fn compute_modified_friction(friction: f32, modifier: f32) -> f32 {
    math::clamp_f32(1.0 - (1.0 - friction) * modifier, 0.0, 1.0)
}

/// `Math.signum(double)`.
fn java_signum(v: f64) -> f64 {
    if v == 0.0 || v.is_nan() { v } else { v.signum() }
}

/// `Vec2.normalized`.
fn vec2_normalized(x: f32, y: f32) -> (f32, f32) {
    let len = math::sqrt_f32(x * x + y * y);
    if len < 1.0e-4 { (0.0, 0.0) } else { (x / len, y / len) }
}

/// `LocalPlayer.modifyInputSpeedForSquareMovement`.
fn modify_input_speed_for_square_movement(x: f32, y: f32) -> (f32, f32) {
    let length = math::sqrt_f32(x * x + y * y);
    if length <= 0.0 {
        return (x, y);
    }
    let (dx, dy) = (x * (1.0 / length), y * (1.0 / length));
    let (ax, ay) = (dx.abs(), dy.abs());
    let tan = if ay > ax { ax / ay } else { ay / ax };
    let distance_to_unit_square = math::sqrt_f32(1.0 + tan * tan);
    let modified = java_min_f32(length * distance_to_unit_square, 1.0);
    (dx * modified, dy * modified)
}

fn java_min_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() { a } else if a <= b { a } else { b }
}

/// `Entity.collideWithShapes`.
fn collide_with_shapes(movement: Vec3, bbox: &Aabb, shapes: &[PlacedShape<'_>]) -> Vec3 {
    if shapes.is_empty() {
        return movement;
    }
    let mut resolved = Vec3::ZERO;
    // Direction.axisStepOrder: Y first, then the larger horizontal axis last.
    let order = if movement.x.abs() < movement.z.abs() { [Axis::Y, Axis::Z, Axis::X] } else { [Axis::Y, Axis::X, Axis::Z] };
    for axis in order {
        let axis_movement = movement.get(axis);
        if axis_movement != 0.0 {
            let moved = bbox.move_by(resolved.x, resolved.y, resolved.z);
            let collision = shapes_collide(axis, &moved, shapes, axis_movement);
            resolved = resolved.with(axis, collision);
        }
    }
    resolved
}

/// `Entity.collectCandidateStepUpHeights`: distinct Y coordinates of the
/// colliders above the box bottom, up to the max step, ascending.
fn collect_candidate_step_up_heights(
    bbox: &Aabb,
    colliders: &[PlacedShape<'_>],
    max_step: f32,
    skip: f32,
) -> Vec<f32> {
    let mut candidates: Vec<f32> = Vec::with_capacity(4);
    for collider in colliders {
        for coord in collider.coords(Axis::Y) {
            let relative = (coord - bbox.min_y) as f32;
            if !(relative < 0.0) && relative != skip {
                if relative > max_step {
                    break;
                }
                // FloatArraySet: exact float equality.
                if !candidates.contains(&relative) {
                    candidates.push(relative);
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.total_cmp(b));
    candidates
}
