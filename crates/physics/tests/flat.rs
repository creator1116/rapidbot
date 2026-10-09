//! Movement on flat ground against well-known vanilla values.

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player};
use rapidbot_world::chunk::Chunk;
use rapidbot_world::tags::Tags;
use rapidbot_world::{DimensionHeight, Registry, World};

fn state(name: &str) -> u32 {
    let r = Registry::get();
    let block = r.block_by_name(name).unwrap();
    r.states.iter().position(|s| s.block == block.id).unwrap() as u32
}

/// Stone floor with its top at y = 64, in chunks -2..=2.
fn flat_world(floor: &str) -> World {
    let mut world = World::new(DimensionHeight::OVERWORLD);
    let floor = state(floor);
    for cx in -2..=2 {
        for cz in -2..=2 {
            world.insert_chunk(cx, cz, Chunk::empty(24));
            for x in 0..16 {
                for z in 0..16 {
                    world.set_block_state(cx * 16 + x, 63, cz * 16 + z, floor);
                }
            }
        }
    }
    world
}

fn player_at(x: f64, y: f64, z: f64) -> Player {
    let mut p = Player::new();
    p.y_rot = 0.0;
    p.set_pos(Vec3::new(x, y, z));
    p
}

/// A player placed exactly on the floor with no velocity is not on the
/// ground until gravity has pushed it into the floor once (vanilla too:
/// `move` with a zero delta collides with nothing).
fn settle(p: &mut Player, world: &World, tags: &Tags) {
    p.tick(world, tags);
    assert!(!p.on_ground);
    p.tick(world, tags);
    assert!(p.on_ground);
}

#[test]
fn lands_and_stays_on_the_ground() {
    let world = flat_world("minecraft:stone");
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    settle(&mut p, &world, &tags);
    assert_eq!(p.pos.y, 64.0);
    assert_eq!(p.main_supporting_block, Some((0, 63, 0)));
    for _ in 0..40 {
        p.tick(&world, &tags);
    }
    assert_eq!(p.pos.y, 64.0);
    assert!(p.on_ground);
    // Gravity is applied every tick and cancelled by the collision.
    assert_eq!(p.delta_movement.y, -0.0784000015258789);
}

#[test]
fn falls_with_vanilla_velocities() {
    let world = flat_world("minecraft:stone");
    let tags = Tags::default();
    let mut p = player_at(0.5, 70.0, 0.5);
    p.tick(&world, &tags);
    // First tick: v = (0 - 0.08) * 0.98.
    assert_eq!(p.delta_movement.y, -0.0784000015258789);
    p.tick(&world, &tags);
    assert_eq!(p.delta_movement.y, (-0.0784000015258789 - 0.08) * 0.9800000190734863);
    for _ in 0..40 {
        p.tick(&world, &tags);
    }
    assert!(p.on_ground);
    assert_eq!(p.pos.y, 64.0);
}

fn player_falling_through(world: &World, tags: &Tags, velocity: f64) -> Player {
    let mut p = player_at(0.5, 70.0, 0.5);
    p.fall_distance = 5.0;
    p.delta_movement = Vec3::new(0.0, velocity, 0.0);
    p.tick(world, tags);
    p
}

#[test]
fn fast_fall_through_water_resets_accumulated_fall_distance() {
    let mut world = flat_world("minecraft:stone");
    world.set_block_state(0, 68, 0, state("minecraft:water"));
    let tags = Tags::default();
    let p = player_falling_through(&world, &tags, -2.0);
    assert!(p.fall_distance < 3.0, "fall distance {}", p.fall_distance);
}

#[test]
fn fast_fall_through_resetting_block_resets_accumulated_fall_distance() {
    let mut world = flat_world("minecraft:stone");
    let hay = state("minecraft:hay_block");
    world.set_block_state(0, 68, 0, hay);
    let mut tags = Tags::default();
    let block = Registry::get().block_of(hay).id;
    tags.insert("minecraft:block", "minecraft:fall_damage_resetting", block);
    let p = player_falling_through(&world, &tags, -2.0);
    assert!(p.fall_distance < 3.0, "fall distance {}", p.fall_distance);
}

#[test]
fn slow_fall_does_not_reset_accumulated_fall_distance() {
    let mut world = flat_world("minecraft:stone");
    world.set_block_state(0, 69, 0, state("minecraft:water"));
    let tags = Tags::default();
    let p = player_falling_through(&world, &tags, -0.5);
    assert!(p.fall_distance > 5.0, "fall distance {}", p.fall_distance);
}

fn run_forward(sprint: bool, ticks: usize) -> Player {
    let world = flat_world("minecraft:stone");
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, sprint, ..Keys::default() };
    for _ in 0..ticks {
        p.tick(&world, &tags);
    }
    p
}

#[test]
fn walking_reaches_vanilla_speed() {
    let p = run_forward(false, 60);
    // yaw 0 faces +z. Terminal walking speed on normal blocks.
    let v = p.delta_movement.z / 0.546;
    assert!((p.delta_movement.x).abs() < 1e-12);
    assert!((v - 0.21585).abs() < 1e-3, "walking speed {v}");
    assert!(!p.sprinting);
}

#[test]
fn sprinting_reaches_vanilla_speed() {
    let p = run_forward(true, 60);
    assert!(p.sprinting);
    let v = p.delta_movement.z / 0.546;
    assert!((v - 0.2806).abs() < 1e-3, "sprinting speed {v}");
}

#[test]
fn jump_arc() {
    let world = flat_world("minecraft:stone");
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    settle(&mut p, &world, &tags);
    p.held_keys = Keys { jump: true, ..Keys::default() };
    p.tick(&world, &tags);
    // Jump power 0.42F (as a double) moved first, then gravity and drag.
    assert_eq!(p.pos.y, 64.0 + 0.41999998688697815);
    p.held_keys = Keys::default();
    let mut max_y = p.pos.y;
    for _ in 0..20 {
        p.tick(&world, &tags);
        max_y = max_y.max(p.pos.y);
    }
    assert!((max_y - 64.0 - 1.2522).abs() < 1e-3, "apex {}", max_y - 64.0);
    assert!(p.on_ground);
    assert_eq!(p.pos.y, 64.0);
}

#[test]
fn steps_up_a_slab_but_not_a_full_block() {
    let mut world = flat_world("minecraft:stone");
    let tags = Tags::default();
    let slab = Registry::get()
        .states
        .iter()
        .position(|s| {
            Registry::get().blocks[s.block as usize].name == "minecraft:stone_slab" && s.properties.contains("type=bottom")
                // The first bottom slab state is waterlogged, which is water.
                && s.properties.contains("waterlogged=false")
        })
        .unwrap() as u32;
    for x in -3..=3 {
        for z in 3..8 {
            world.set_block_state(x, 64, z, slab);
        }
        world.set_block_state(x, 64, 8, state("minecraft:stone"));
        world.set_block_state(x, 65, 8, state("minecraft:stone"));
    }
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, ..Keys::default() };
    for _ in 0..40 {
        p.tick(&world, &tags);
    }
    // Climbed onto the slab (y 64.5) and stopped at the wall at z = 8.
    assert_eq!(p.pos.y, 64.5);
    // The half width is 0.6F / 2.0F in float, so the box stops at
    // 8 - 0.30000001192092896, not 7.7.
    assert_eq!(p.pos.z, 8.0 - (0.3f32 as f64));
    assert!(p.horizontal_collision);
}

#[test]
fn sneaking_stops_at_the_edge() {
    let mut world = flat_world("minecraft:stone");
    let tags = Tags::default();
    // Remove the floor from z = 4 onwards.
    for x in -32..32 {
        for z in 4..32 {
            world.set_block_state(x, 63, z, 0);
        }
    }
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, shift: true, ..Keys::default() };
    for _ in 0..200 {
        p.tick(&world, &tags);
    }
    assert!(p.on_ground);
    assert_eq!(p.pos.y, 64.0);
    // The box (half width 0.3) may overhang the edge but not leave it.
    assert!(p.pos.z < 4.3 && p.pos.z > 4.0, "z {}", p.pos.z);
}

#[test]
fn ice_is_slippery() {
    let world = flat_world("minecraft:ice");
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, ..Keys::default() };
    for _ in 0..30 {
        p.tick(&world, &tags);
    }
    p.held_keys = Keys::default();
    let start = p.pos.z;
    for _ in 0..20 {
        p.tick(&world, &tags);
    }
    // On stone a player stops within a few ticks; on ice they slide on.
    assert!(p.pos.z - start > 1.0, "slid {}", p.pos.z - start);
}

