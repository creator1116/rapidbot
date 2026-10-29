//! Movement against entity colliders, the world border and scaffolding.

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player};
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
