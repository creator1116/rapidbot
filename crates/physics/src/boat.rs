//! `world/entity/vehicle/boat/AbstractBoat`: a boat the local player
//! controls, simulated on the client.
//!
//! When the local player is the controlling passenger the client is
//! authoritative (`isLocalInstanceAuthoritative`): it runs the water status,
//! the float, the paddle control and the move every tick, reports the paddle
//! states with `ServerboundPaddleBoatPacket`, and sends its position with
//! `ServerboundMoveVehiclePacket` from the ride loop. When it is not
//! authoritative the delta and delta rotation zero out and the position
//! follows the server through the interpolation handler.

use crate::math::Vec3;
use crate::player::collide_with_shapes;
use rapidbot_world::collision::{EntityCollisionContext, block_collisions_with};
use rapidbot_world::registry::Registry;
use rapidbot_world::{Aabb, World};

/// `AbstractBoat`'s size: 1.375 wide and 0.5625 tall.
const HALF_WIDTH: f64 = 1.375 / 2.0;
const HEIGHT: f64 = 0.5625;

/// `AbstractBoat.PADDLE_SPEED`: the paddle position's per-tick step.
const PADDLE_SPEED: f32 = std::f32::consts::PI / 8.0;

/// The paddle input, as `setInput` receives it from the ride loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoatInput {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// `AbstractBoat.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    InAir,
}

/// A boat simulated by this client.
#[derive(Debug, Clone)]
pub struct Boat {
    pub pos: Vec3,
    pub delta_movement: Vec3,
    pub y_rot: f32,
    delta_rotation: f32,
    pub bb: Aabb,
    pub input: BoatInput,
    paddle_left: bool,
    paddle_right: bool,
    paddle_positions: [f32; 2],
    land_friction: f32,
    status: Status,
    old_status: Status,
    water_level: f64,
    last_yd: f64,
    pub on_ground: bool,
    pub horizontal_collision: bool,
}

impl Boat {
    /// A boat at `pos`, as the vehicle spawn and the riding handoff place it.
    pub fn new(pos: Vec3) -> Self {
        Self {
            pos,
            delta_movement: Vec3::ZERO,
            y_rot: 0.0,
            delta_rotation: 0.0,
            bb: Self::box_at(pos),
            input: BoatInput::default(),
            paddle_left: false,
            paddle_right: false,
            paddle_positions: [0.0; 2],
            land_friction: 0.0,
            status: Status::InAir,
            old_status: Status::InAir,
            water_level: 0.0,
            last_yd: 0.0,
            on_ground: false,
            horizontal_collision: false,
        }
    }

    fn box_at(pos: Vec3) -> Aabb {
        Aabb::new(
            pos.x - HALF_WIDTH,
            pos.y,
            pos.z - HALF_WIDTH,
            pos.x + HALF_WIDTH,
            pos.y + HEIGHT,
            pos.z + HALF_WIDTH,
        )
    }

    fn set_pos(&mut self, pos: Vec3) {
        self.pos = pos;
        self.bb = Self::box_at(pos);
    }

    /// The paddle states the tick ended with: what the ride loop reports
    /// with `ServerboundPaddleBoatPacket`.
    pub fn paddle_state(&self, side: usize) -> bool {
        if side == 0 {
            self.paddle_left
        } else {
            self.paddle_right
        }
    }

    /// The paddle positions, for `getRowingTime` consumers.
    pub fn rowing_time(&self, side: usize) -> f32 {
        self.paddle_positions[side]
    }

    /// `AbstractBoat.tick` on the authoritative client. Returns the paddle
    /// states to report.
    pub fn tick(
        &mut self,
        world: &World,
        registry: &Registry,
        entity_colliders: &[Aabb],
    ) -> (bool, bool) {
        self.old_status = self.status;
        self.status = self.get_status(world);

        self.float_boat(world);
        self.control_boat();
        let paddles = (self.paddle_left, self.paddle_right);
        self.move_with_collisions(world, registry, entity_colliders);

        for i in 0..2 {
            if self.paddle_state(i) {
                self.paddle_positions[i] += PADDLE_SPEED;
            } else {
                self.paddle_positions[i] = 0.0;
            }
        }
        paddles
    }

    /// `AbstractBoat.getStatus`: underwater, in water, on land by ground
    /// friction, else in the air.
    fn get_status(&mut self, world: &World) -> Status {
        if let Some(status) = self.is_underwater(world) {
            self.water_level = self.bb.max_y;
            return status;
        }
        if self.check_in_water(world) {
            return Status::InWater;
        }
        let friction = self.ground_friction(world);
        if friction > 0.0 {
            self.land_friction = friction;
            Status::OnLand
        } else {
            Status::InAir
        }
    }

    /// The block columns of the boat's box between two heights, iterating
    /// `floor(min) ..< ceil(max)` on every axis the way the vanilla scans do.
    fn box_columns(bb: &Aabb, y0: f64, y1: f64) -> impl Iterator<Item = (i32, i32, i32)> {
        let (min_x, max_x) = (bb.min_x, bb.max_x);
        let (min_z, max_z) = (bb.min_z, bb.max_z);
        (min_x.floor() as i32..max_x.ceil() as i32).flat_map(move |x| {
            (y0.floor() as i32..y1.ceil() as i32).flat_map(move |y| {
                (min_z.floor() as i32..max_z.ceil() as i32).map(move |z| (x, y, z))
            })
        })
    }

    /// `AbstractBoat.checkInWater`: the water level of the water under the
    /// boat's bottom slab, and whether any of it reaches the bottom.
    fn check_in_water(&mut self, world: &World) -> bool {
        let registry = Registry::get();
        let mut in_water = false;
        self.water_level = -f64::MAX;
        let bb = self.bb;
        for (x, y, z) in Self::box_columns(&bb, bb.min_y, bb.min_y + 0.001) {
            if let Some((amount, _)) = water_at(world, registry, x, y, z) {
                let height = y as f64 + amount as f64 / 9.0;
                self.water_level = self.water_level.max(height);
                in_water |= bb.min_y < height;
            }
        }
        in_water
    }

    /// `AbstractBoat.isUnderwater`: water above the boat's top edge. A
    /// falling or non-source block counts as flowing water.
    fn is_underwater(&self, world: &World) -> Option<Status> {
        let registry = Registry::get();
        let max_y = self.bb.max_y + 0.001;
        let mut under_water = false;
        for (x, y, z) in Self::box_columns(&self.bb, self.bb.max_y, max_y) {
            if let Some((amount, falling)) = water_at(world, registry, x, y, z)
                && max_y < y as f64 + amount as f64 / 9.0
            {
                if amount < 8 || falling {
                    return Some(Status::UnderFlowingWater);
                }
                under_water = true;
            }
        }
        under_water.then_some(Status::UnderWater)
    }

    /// `AbstractBoat.getWaterLevelAbove`: the surface of the topmost water
    /// layer the boat's box reaches, or the ceiling of the scan.
    fn get_water_level_above(&self, world: &World) -> f64 {
        let registry = Registry::get();
        let min_y = self.bb.max_y.floor() as i32;
        let max_y = (self.bb.max_y - self.last_yd).ceil() as i32;
        let mut y = min_y;
        while y < max_y {
            let mut block_height = 0.0f64;
            for (x, yy, z) in Self::box_columns(&self.bb, y as f64, (y + 1) as f64) {
                let _ = yy;
                if let Some((amount, _)) = water_at(world, registry, x, y, z) {
                    block_height = block_height.max(amount as f64 / 9.0);
                }
                if block_height >= 1.0 {
                    break;
                }
            }
            if block_height < 1.0 {
                return y as f64 + block_height;
            }
            y += 1;
        }
        (max_y + 1) as f64
    }

    /// `AbstractBoat.getGroundFriction`: the average block friction of the
    /// blocks whose collision shape reaches into a hair-thin box under the
    /// boat, skipping lily pads as vanilla does.
    fn ground_friction(&self, world: &World) -> f32 {
        let registry = Registry::get();
        let bb = self.bb;
        let strip = Aabb::new(
            bb.min_x,
            bb.min_y - 0.001,
            bb.min_z,
            bb.max_x,
            bb.min_y,
            bb.max_z,
        );
        let x0 = strip.min_x.floor() as i32 - 1;
        let x1 = strip.max_x.ceil() as i32 + 1;
        let y0 = strip.min_y.floor() as i32 - 1;
        let y1 = strip.max_y.ceil() as i32 + 1;
        let z0 = strip.min_z.floor() as i32 - 1;
        let z1 = strip.max_z.ceil() as i32 + 1;
        let mut friction = 0.0f32;
        let mut count = 0;
        for x in x0..x1 {
            for z in z0..z1 {
                let edges =
                    (i32::from(x != x0 && x != x1 - 1)) + (i32::from(z != z0 && z != z1 - 1));
                if edges == 2 {
                    continue;
                }
                for y in y0..y1 {
                    if edges <= 0 || (y != y0 && y != y1 - 1) {
                        let state = world.block_state_or_air(x, y, z);
                        let info = registry.state(state);
                        if info.is_air || info.fluid == "minecraft:water" {
                            continue;
                        }
                        let name = &registry.blocks[info.block as usize].name;
                        if name == "minecraft:lily_pad" {
                            continue;
                        }
                        if registry.collision_shape(state).is_some_and(|shape| {
                            let bounds = shape.bounds();
                            let placed = Aabb::new(
                                bounds.min_x + x as f64,
                                bounds.min_y + y as f64,
                                bounds.min_z + z as f64,
                                bounds.max_x + x as f64,
                                bounds.max_y + y as f64,
                                bounds.max_z + z as f64,
                            );
                            placed.intersects_aabb(&strip)
                        }) {
                            friction += registry.blocks[info.block as usize].friction;
                            count += 1;
                        }
                    }
                }
            }
        }
        if count == 0 {
            0.0
        } else {
            friction / count as f32
        }
    }

    /// `AbstractBoat.floatBoat`: the buoyancy, gravity and friction of the
    /// current status, plus the drop onto the surface from the air.
    fn float_boat(&mut self, world: &World) {
        let gravity = 0.04f64;
        let mut vspeed = -gravity;
        let mut buoyancy = 0.0f64;
        if self.old_status == Status::InAir
            && self.status != Status::InAir
            && self.status != Status::OnLand
        {
            self.water_level = self.pos.y + HEIGHT;
            let target_y = self.get_water_level_above(world) - HEIGHT + 0.101;
            // The vanilla guards the drop with `noCollision` of the box moved
            // to the target height; the water-side box is free where the
            // surface scan found water, so the check is skipped here.
            self.set_pos(Vec3::new(self.pos.x, target_y, self.pos.z));
            self.delta_movement.y = 0.0;
            self.last_yd = 0.0;
            self.status = Status::InWater;
        } else {
            let inv_friction = match self.status {
                Status::InWater => {
                    buoyancy = (self.water_level - self.pos.y) / HEIGHT;
                    0.9
                }
                Status::UnderFlowingWater => {
                    vspeed = -7.0e-4;
                    0.9
                }
                Status::UnderWater => {
                    buoyancy = 0.01;
                    0.45
                }
                Status::InAir => 0.9,
                Status::OnLand => {
                    // `getControllingPassenger() instanceof Player`: the
                    // local player is always the controlling passenger here.
                    self.land_friction /= 2.0;
                    self.land_friction
                }
            };
            let delta = self.delta_movement;
            self.delta_movement = Vec3::new(
                delta.x * inv_friction as f64,
                delta.y + vspeed,
                delta.z * inv_friction as f64,
            );
            self.delta_rotation *= inv_friction;
            if buoyancy > 0.0 {
                let delta = self.delta_movement;
                self.delta_movement = Vec3::new(
                    delta.x,
                    (delta.y + buoyancy * (gravity / 0.65)) * 0.75,
                    delta.z,
                );
            }
        }
    }

    /// `AbstractBoat.controlBoat`: the paddle input into the yaw and the
    /// acceleration.
    fn control_boat(&mut self) {
        // `isVehicle()`: there is a passenger; the ride loop only ticks the
        // boat the local player controls.
        let mut acceleration = 0.0f32;
        if self.input.left {
            self.delta_rotation -= 1.0;
        }
        if self.input.right {
            self.delta_rotation += 1.0;
        }
        if self.input.right != self.input.left && !self.input.up && !self.input.down {
            acceleration += 0.005;
        }
        self.y_rot += self.delta_rotation;
        if self.input.up {
            acceleration += 0.04;
        }
        if self.input.down {
            acceleration -= 0.005;
        }
        let delta = self.delta_movement;
        self.delta_movement = Vec3::new(
            delta.x
                + (self.y_rot * -std::f32::consts::PI / 180.0).sin() as f64 * acceleration as f64,
            delta.y,
            delta.z
                + (self.y_rot * std::f32::consts::PI / 180.0).cos() as f64 * acceleration as f64,
        );
        self.paddle_left = self.input.right && !self.input.left || self.input.up;
        self.paddle_right = self.input.left && !self.input.right || self.input.up;
    }

    /// `Entity.move(MoverType.SELF, deltaMovement)`: the axis-wise clip
    /// against the world's blocks and the entities. The boat's `maxUpStep`
    /// is 0 (`Entity.maxUpStep` returns 0; only living entities carry the
    /// step-height attribute), so it bumps ledges instead of stepping, and
    /// the block speed factor is 1.0 for a non-living entity.
    fn move_with_collisions(
        &mut self,
        world: &World,
        registry: &Registry,
        entity_colliders: &[Aabb],
    ) {
        let delta = self.delta_movement;
        let expanded = self.bb.expand_towards(delta.x, delta.y, delta.z);
        let shapes = block_collisions_with(
            world,
            registry,
            &expanded,
            EntityCollisionContext::default(),
        );
        let movement = collide_with_shapes(delta, &self.bb, &shapes, entity_colliders, None);
        self.horizontal_collision = movement.x != delta.x || movement.z != delta.z;
        self.on_ground = movement.y != delta.y && delta.y < 0.0;
        let new_pos = self.pos.plus(movement);
        self.last_yd = movement.y;
        self.set_pos(new_pos);
        // `Entity.move` leaves `deltaMovement` at the caller's value; only
        // the block speed factor is applied, and it is 1.0 here.
    }
}

/// The water of a block position, as `(amount, falling)` for `minecraft:water`.
fn water_at(world: &World, registry: &Registry, x: i32, y: i32, z: i32) -> Option<(u8, bool)> {
    let state = world.block_state_or_air(x, y, z);
    let info = registry.state(state);
    (info.fluid == "minecraft:water").then_some((info.fluid_amount, info.fluid_falling))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_world::DimensionHeight;
    use rapidbot_world::chunk::Chunk;
    use rapidbot_world::registry::StateId;

    fn water_states() -> Vec<StateId> {
        let registry = Registry::get();
        let block = registry.block_by_name("minecraft:water").unwrap().id;
        registry
            .states
            .iter()
            .enumerate()
            .filter_map(|(id, state)| (state.block == block).then_some(id as StateId))
            .collect()
    }

    fn source_state() -> StateId {
        // The water source: `fluid_amount` 8, the vanilla's `getAmount()`.
        let registry = Registry::get();
        water_states()
            .into_iter()
            .find(|id| registry.state(*id).fluid_amount == 8)
            .unwrap()
    }

    /// A still pool of source water filling y 60..64 in a 9x9 patch.
    fn pool_world() -> World {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        let water = source_state();
        for x in -4..5 {
            for z in -4..5 {
                for y in 60..64 {
                    world.set_block_state(x, y, z, water);
                }
            }
        }
        world
    }

    #[test]
    fn a_boat_dropped_on_water_settles_at_the_surface() {
        let world = pool_world();
        let registry = Registry::get();
        let mut boat = Boat::new(Vec3::new(0.5, 70.0, 0.5));
        for _ in 0..600 {
            boat.tick(&world, registry, &[]);
        }
        // The water's top row is y 63 at height 8/9, so the surface is at
        // 63.889; the boat's bottom settles into the buoyancy band below it.
        assert!(
            boat.pos.y > 62.5 && boat.pos.y < 64.2,
            "boat settled at {:?}",
            boat.pos
        );
        assert!(
            boat.delta_movement.y.abs() < 0.05,
            "vertical motion {:?}",
            boat.delta_movement
        );
    }

    #[test]
    fn paddling_forward_rows_both_paddles_and_accelerates() {
        let world = pool_world();
        let registry = Registry::get();
        let mut boat = Boat::new(Vec3::new(0.5, 63.0, 0.5));
        boat.input.up = true;
        let before = boat.delta_movement;
        let (left, right) = boat.tick(&world, registry, &[]);
        assert!(left && right, "the up input rows both paddles");
        assert!(
            boat.delta_movement != before,
            "the up input accelerates: {:?} -> {:?}",
            before,
            boat.delta_movement
        );
    }

    #[test]
    fn turning_input_spins_the_boat_and_rows_one_side() {
        let world = pool_world();
        let registry = Registry::get();
        let mut boat = Boat::new(Vec3::new(0.5, 63.0, 0.5));
        boat.input.left = true;
        let (left, right) = boat.tick(&world, registry, &[]);
        // `controlBoat`: left input decrements deltaRotation and rows the
        // right paddle only.
        assert!(right && !left, "paddles after a left input");
        assert!(boat.delta_rotation < 0.0);
        assert!(boat.y_rot < 0.0);
    }

    #[test]
    fn a_boat_on_land_reports_the_land_status_and_slows() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        let stone = Registry::get().block_by_name("minecraft:stone").unwrap().id;
        let stone_state = Registry::get()
            .states
            .iter()
            .enumerate()
            .find(|(_, s)| s.block == stone)
            .map(|(id, _)| id as StateId)
            .unwrap();
        for x in -4..5 {
            for z in -4..5 {
                world.set_block_state(x, 63, z, stone_state);
            }
        }
        let registry = Registry::get();
        let mut boat = Boat::new(Vec3::new(0.5, 64.0, 0.5));
        boat.delta_movement = Vec3::new(0.4, 0.0, 0.0);
        boat.tick(&world, registry, &[]);
        // The first tick lands it: the ground friction takes over, and the
        // delta decays toward zero instead of gliding.
        for _ in 0..120 {
            boat.tick(&world, registry, &[]);
        }
        assert!(
            boat.delta_movement.x.abs() < 0.05,
            "boat slid to {:?} after 120 ticks",
            boat.delta_movement
        );
    }
}
