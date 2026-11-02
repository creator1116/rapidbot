//! Movement against entity colliders, the world border and scaffolding.

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player, Pose};
use rapidbot_world::chunk::Chunk;
use rapidbot_world::tags::Tags;
use rapidbot_world::{Aabb, DimensionHeight, Registry, World};

fn state(name: &str, properties: &str) -> u32 {
    let r = Registry::get();
    let block = r.block_by_name(name).unwrap();
    r.states
        .iter()
        .position(|s| s.block == block.id && s.properties.contains(properties))
        .unwrap() as u32
}

fn flat_world() -> World {
    let stone = state("minecraft:stone", "");
    let mut world = World::new(DimensionHeight::OVERWORLD);
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

fn empty_world() -> World {
    let mut world = World::new(DimensionHeight::OVERWORLD);
    for cx in -1..=1 {
        for cz in -1..=1 {
            world.insert_chunk(cx, cz, Chunk::empty(24));
        }
    }
    world
}

fn player_at(x: f64, y: f64, z: f64) -> Player {
    let mut player = Player::new();
    player.set_pos(Vec3::new(x, y, z));
    player
}

fn walk_east(player: &mut Player, world: &World, tags: &Tags) {
    player.y_rot = -90.0;
    player.held_keys = Keys {
        forward: true,
        ..Keys::default()
    };
    for _ in 0..40 {
        player.tick(world, tags);
    }
}

#[test]
fn world_border_stops_a_walking_player() {
    let world = flat_world();
    let tags = Tags::default();
    let mut player = player_at(4.0, 64.0, 0.0);
    player.world_border.set_size(10.0);
    walk_east(&mut player, &world, &tags);
    // The collision shape is block-aligned: floor/ceil of +/-5.
    assert!(player.pos.x <= 4.7 + 1.0e-6, "x = {}", player.pos.x);
    assert!(player.pos.x > 4.5);
}

#[test]
fn an_entity_box_stops_a_walking_player() {
    let world = flat_world();
    let tags = Tags::default();
    let mut player = player_at(4.0, 64.0, 0.0);
    // A boat-sized box just east of the player.
    player.entity_colliders = vec![Aabb::new(4.9, 64.0, -0.5, 5.3, 65.8, 0.5)];
    walk_east(&mut player, &world, &tags);
    assert!(player.pos.x <= 4.6 + 1.0e-6, "x = {}", player.pos.x);
    assert!(player.pos.x > 4.4);
}

#[test]
fn scaffolding_is_solid_from_above() {
    let mut world = flat_world();
    // Stable scaffolding has a full 16x16 slab at y 14/16..1.
    let scaffold = state(
        "minecraft:scaffolding",
        "bottom=false,distance=0,waterlogged=false",
    );
    world.set_block_state(0, 64, 0, scaffold);
    let tags = Tags::default();
    let mut player = player_at(0.0, 65.0, 0.0);
    for _ in 0..10 {
        player.tick(&world, &tags);
    }
    assert!(player.on_ground);
    assert!((player.pos.y - 65.0).abs() < 1.0e-6, "y = {}", player.pos.y);
}

#[test]
fn collision_context_uses_fall_distance_after_effects_reset_it() {
    let mut world = empty_world();
    world.set_block_state(0, 63, 0, state("minecraft:powder_snow", ""));
    let tags = Tags::default();
    let mut player = player_at(0.5, 64.0, 0.5);
    player.delta_movement.y = -0.2;
    player.fall_distance = 3.0;
    player.effects.slow_falling = true;

    player.tick(&world, &tags);

    // Slow falling reset fallDistance before collision lookup. Powder snow
    // therefore has no 0.9-block falling shape and the player passes through.
    assert!(player.pos.y < 63.9, "y = {}", player.pos.y);
}

#[test]
fn leather_boots_climb_out_of_powder_snow_on_jump() {
    let mut world = empty_world();
    world.set_block_state(0, 64, 0, state("minecraft:powder_snow", ""));
    let tags = Tags::default();
    let mut player = player_at(0.5, 64.2, 0.5);
    player.has_leather_boots = true;

    // The first inside-block pass sets isInPowderSnow for the following tick.
    player.tick(&world, &tags);
    player.held_keys.jump = true;
    player.tick(&world, &tags);

    assert!(
        player.delta_movement.y > 0.11,
        "vy = {}",
        player.delta_movement.y
    );
}

#[test]
fn world_border_participates_in_pose_collision_checks() {
    let world = flat_world();
    let tags = Tags::default();
    let mut player = player_at(4.8, 64.0, 0.5);
    player.apply_synced_pose(Pose::Crouching);
    player.world_border.set_size(10.0);

    player.tick(&world, &tags);

    // The player was teleported partly into the border collision shape. As in
    // CollisionGetter.noCollision, no pose is considered to fit there.
    assert_eq!(player.pose, Pose::Crouching);
}
