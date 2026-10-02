//! Water, lava, ladders and elytra gliding.

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player, Pose};
use rapidbot_world::chunk::Chunk;
use rapidbot_world::tags::Tags;
use rapidbot_world::{DimensionHeight, Registry, World};

fn state(name: &str, properties: &str) -> u32 {
    let r = Registry::get();
    let block = r.block_by_name(name).unwrap();
    r.states.iter().position(|s| s.block == block.id && s.properties.contains(properties)).unwrap() as u32
}

/// Stone floor with its top at y = 64 in chunks -1..=1.
fn flat_world() -> World {
    let mut world = World::new(DimensionHeight::OVERWORLD);
    let stone = state("minecraft:stone", "");
    for cx in -1..=1 {
        for cz in -1..=1 {
            world.insert_chunk(cx, cz, Chunk::empty(24));
            for x in 0..16 {
                for z in 0..16 {
                    world.set_block_state(cx * 16 + x, 63, cz * 16 + z, stone);
                }
            }
        }
    }
    world
}

/// A pool of `fluid` sources over x,z in -8..8, from y = 64 up to `top`.
fn pool(fluid: &str, top: i32) -> World {
    let mut world = flat_world();
    let source = state(fluid, "level=0");
    for x in -8..8 {
        for z in -8..8 {
            for y in 64..top {
                world.set_block_state(x, y, z, source);
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

#[test]
fn sinks_in_water_with_vanilla_velocities() {
    let world = pool("minecraft:water", 70);
    let tags = Tags::default();
    let mut p = player_at(0.5, 66.0, 0.5);
    p.tick(&world, &tags);
    assert!(p.in_water);
    // Only the cells the box overlaps count: feet at 66, box top in cell 67.
    assert_eq!(p.water_height, 2.0);
    // (0 * 0.8F) - 0.08 / 16
    assert_eq!(p.delta_movement.y, -0.005);
    p.tick(&world, &tags);
    assert_eq!(p.delta_movement.y, -0.005 * 0.800000011920929 - 0.005);
    for _ in 0..200 {
        p.tick(&world, &tags);
    }
    assert!(p.on_ground);
    assert_eq!(p.pos.y, 64.0);
    assert_eq!(p.fall_distance, 0.0);
}

#[test]
fn holding_jump_floats_at_the_surface() {
    let world = pool("minecraft:water", 67);
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    p.held_keys = Keys { jump: true, ..Keys::default() };
    for _ in 0..200 {
        p.tick(&world, &tags);
    }
    // Bobbing where the water is about 0.4 deep at the feet: the surface is
    // at 64 + 2 + 8/9.
    let depth = 64.0 + 2.0 + 8.0 / 9.0 - p.pos.y;
    assert!(depth > 0.0 && depth < 0.9, "depth {depth}");
    assert!(!p.on_ground);
}

#[test]
fn water_walking_is_slow() {
    let world = pool("minecraft:water", 65);
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, ..Keys::default() };
    for _ in 0..60 {
        p.tick(&world, &tags);
    }
    // Terminal speed 0.02 * 0.98 input per tick with 0.8 drag: v = a / (1 - 0.8).
    let v = p.delta_movement.z / 0.8f32 as f64;
    assert!((v - 0.098).abs() < 1e-3, "water speed {v}");
}

#[test]
fn sprint_swimming_under_water() {
    let world = pool("minecraft:water", 72);
    let tags = Tags::default();
    let mut p = player_at(0.5, 66.0, 0.5);
    p.x_rot = 0.0;
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, sprint: true, ..Keys::default() };
    for _ in 0..20 {
        p.tick(&world, &tags);
    }
    assert!(p.sprinting && p.swimming);
    assert_eq!(p.pose, Pose::Swimming);
    // Looking level, a swimmer holds depth instead of sinking.
    let y = p.pos.y;
    for _ in 0..20 {
        p.tick(&world, &tags);
    }
    assert!((p.pos.y - y).abs() < 0.2, "drifted {}", p.pos.y - y);
}

#[test]
fn current_pushes_the_player() {
    let mut world = flat_world();
    // A stream flowing towards +x: source at x = 0, levels rising to 7.
    for x in 0..8 {
        world.set_block_state(x, 64, 0, state("minecraft:water", &format!("level={x}")));
    }
    let tags = Tags::default();
    let mut p = player_at(3.5, 64.0, 0.5);
    for _ in 0..20 {
        p.tick(&world, &tags);
    }
    assert!(p.pos.x > 3.6, "x {}", p.pos.x);
    assert!((p.pos.z - 0.5).abs() < 1e-9);
}

#[test]
fn lava_is_thick() {
    let world = pool("minecraft:lava", 70);
    let tags = Tags::default();
    let mut p = player_at(0.5, 66.0, 0.5);
    p.tick(&world, &tags);
    p.tick(&world, &tags);
    assert!(p.in_lava() && !p.in_water);
    // Deep lava: velocity halves, then gravity / 4.
    assert_eq!(p.delta_movement.y, -0.02 * 0.5 - 0.02);
}

#[test]
fn climbs_a_ladder() {
    let mut world = flat_world();
    let mut tags = Tags::default();
    let ladder = state("minecraft:ladder", "facing=north,waterlogged=false");
    tags.insert("minecraft:block", "minecraft:climbable", Registry::get().block_of(ladder).id);
    // A stone wall at z = 3 with ladders on its north face (z = 2).
    for y in 64..72 {
        world.set_block_state(0, y, 3, state("minecraft:stone", ""));
        world.set_block_state(0, y, 2, ladder);
    }
    let mut p = player_at(0.5, 64.0, 1.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, ..Keys::default() };
    for _ in 0..30 {
        p.tick(&world, &tags);
    }
    // Pushing into the ladder climbs at 0.2 per tick before gravity and
    // drag: (0.2 - 0.08) * 0.98F.
    assert_eq!(p.delta_movement.y, (0.2 - 0.08) * 0.9800000190734863);
    assert!(p.pos.y > 66.0, "y {}", p.pos.y);

    // Letting go slides down at 0.15F; sneaking holds on.
    p.held_keys = Keys::default();
    p.tick(&world, &tags);
    p.held_keys = Keys { shift: true, ..Keys::default() };
    for _ in 0..10 {
        p.tick(&world, &tags);
    }
    let y = p.pos.y;
    for _ in 0..10 {
        p.tick(&world, &tags);
    }
    assert_eq!(p.pos.y, y);
    assert_eq!(p.fall_distance, 0.0);
}

#[test]
fn elytra_glides() {
    let world = flat_world();
    let tags = Tags::default();
    let mut p = player_at(0.5, 200.0, 0.5);
    p.has_elytra = true;
    p.x_rot = 10.0;
    for _ in 0..5 {
        p.tick(&world, &tags);
    }
    assert!(!p.fall_flying);
    p.held_keys = Keys { jump: true, ..Keys::default() };
    p.tick(&world, &tags);
    assert!(p.fall_flying && p.started_fall_flying);
    p.held_keys = Keys::default();
    p.tick(&world, &tags);
    assert!(!p.started_fall_flying);
    assert_eq!(p.pose, Pose::FallFlying);
    for _ in 0..100 {
        p.tick(&world, &tags);
    }
    // Gliding trades height for forward speed: far slower descent than a
    // fall, and well over walking speed forwards (+z at yaw 0).
    assert!(p.delta_movement.y > -0.5, "vy {}", p.delta_movement.y);
    assert!(p.delta_movement.z > 0.5, "vz {}", p.delta_movement.z);
    assert!(p.pos.y > 150.0);

    // Without an elytra the same key press does nothing.
    let mut q = player_at(0.5, 200.0, 0.5);
    q.tick(&world, &tags);
    q.held_keys = Keys { jump: true, ..Keys::default() };
    q.tick(&world, &tags);
    assert!(!q.fall_flying);
}

#[test]
fn cobweb_slows_to_a_crawl() {
    let mut world = flat_world();
    world.set_block_state(0, 70, 0, state("minecraft:cobweb", ""));
    let tags = Tags::default();
    let mut p = player_at(0.5, 73.0, 0.5);
    let mut slowest = f64::MAX;
    let mut last_y = p.pos.y;
    for _ in 0..60 {
        p.tick(&world, &tags);
        if p.pos.y < 71.0 && p.pos.y > 69.5 {
            slowest = slowest.min(last_y - p.pos.y);
        }
        last_y = p.pos.y;
    }
    // In the web each tick starts from rest: (0 - 0.08) * 0.98F, times 0.05F.
    assert!((slowest - 0.0784000015258789 * 0.05000000074505806).abs() < 1e-12, "{slowest}");
    assert_eq!(p.fall_distance, 0.0);
}

#[test]
fn slime_slows_a_walker() {
    let mut world = flat_world();
    for z in 0..12 {
        world.set_block_state(0, 63, z, state("minecraft:slime_block", ""));
    }
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    p.tick(&world, &tags);
    p.held_keys = Keys { forward: true, ..Keys::default() };
    for _ in 0..40 {
        p.tick(&world, &tags);
    }
    // Far below the 0.118 blocks/tick of a walk on stone.
    assert!(p.delta_movement.z < 0.04, "vz {}", p.delta_movement.z);
    assert!(p.pos.z > 1.0);
}

#[test]
fn bubble_column_lifts() {
    let mut world = pool("minecraft:water", 70);
    let up = state("minecraft:bubble_column", "drag=false");
    for y in 64..70 {
        world.set_block_state(0, y, 0, up);
    }
    let tags = Tags::default();
    let mut p = player_at(0.5, 64.0, 0.5);
    for _ in 0..40 {
        p.tick(&world, &tags);
    }
    // Carried up and thrown clear of the surface.
    assert!(p.pos.y > 69.0, "y {}", p.pos.y);
}
