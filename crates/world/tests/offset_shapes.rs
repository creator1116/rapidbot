//! Blocks whose shapes move with their position.
//!
//! `BlockBehaviour.OffsetType` shifts a block inside its cell by a hash of
//! where it is. Flowers, bamboo and dripstone move their crosshair shape that
//! way (and bamboo, dripstone and sulfur spikes their collision box); grass
//! and ferns have an offset type that only changes how they are *drawn*.
//! These tests go through the public API, the way a downstream crate would.

use rapidbot_world::chunk::Chunk;
use rapidbot_world::collision::block_collisions;
use rapidbot_world::raycast;
use rapidbot_world::{Aabb, Axis, DimensionHeight, Registry, StateId, World};

fn state(name: &str) -> StateId {
    Registry::get().block_by_name(name).unwrap().default_state
}

fn world_with_block(x: i32, y: i32, z: i32, name: &str) -> World {
    let mut world = World::new(DimensionHeight::OVERWORLD);
    world.insert_chunk(x >> 4, z >> 4, Chunk::empty(24));
    world.set_block_state(x, y, z, state(name));
    world
}

/// A rose at two positions whose offsets differ. A ray along +x through the
/// flower must hit it at the shape's near edge *plus the offset for that
/// position*: nothing else moves a coordinate.
#[test]
fn a_flower_outline_follows_its_offset() {
    let registry = Registry::get();
    let flower = state("minecraft:dandelion");
    let shape = registry.outline_shape(flower).unwrap();
    let middle = |axis| {
        let coords = shape.coords(axis);
        (coords[0] + coords[coords.len() - 1]) / 2.0
    };
    let min_x = shape.coords(Axis::X)[0];
    let centre_y = 64.0 + middle(Axis::Y);

    let mut checked = 0;
    for (x, z) in [(4, 0), (5, 0), (4, 1)] {
        let shift = registry.shape_offset(flower, x, 64, z);
        if shift[0].abs() < 1.0e-6 {
            continue;
        }
        let world = world_with_block(x, 64, z, "minecraft:dandelion");
        // Through the middle of the shape where the offset really puts it.
        let z_ray = z as f64 + middle(Axis::Z) + shift[2];
        let hit = raycast::clip(
            &world,
            [x as f64 - 1.0, centre_y, z_ray],
            [x as f64 + 2.0, centre_y, z_ray],
        );
        assert_eq!(hit.pos, (x, 64, z), "the ray hit the flower");
        assert!(!hit.miss);
        let expected = x as f64 + min_x + shift[0];
        assert!(
            (hit.location[0] - expected).abs() < 1.0e-9,
            "at ({x}, 64, {z}) the near edge should be at {expected}, hit at {}",
            hit.location[0]
        );
        checked += 1;
    }
    assert!(checked >= 2, "the test positions need differing offsets");
}

/// Grass is drawn shifted but its outline does not move, so the ray hits the
/// shape where it stands.
#[test]
fn grass_is_hit_where_it_stands() {
    let registry = Registry::get();
    let grass = state("minecraft:short_grass");
    assert_eq!(
        registry.shape_offset(grass, 10, 64, 20),
        [0.0; 3],
        "an offset that only moves how it is drawn"
    );
    let shape = registry.outline_shape(grass).unwrap();
    let ys = shape.coords(Axis::Y);
    let centre_y = 64.0 + (ys[0] + ys[ys.len() - 1]) / 2.0;

    let world = world_with_block(10, 64, 20, "minecraft:short_grass");
    let hit = raycast::clip(&world, [9.0, centre_y, 20.5], [12.0, centre_y, 20.5]);
    assert_eq!(hit.pos, (10, 64, 20));
    let expected = 10.0 + shape.coords(Axis::X)[0];
    assert!(
        (hit.location[0] - expected).abs() < 1.0e-9,
        "unshifted near edge {expected}, hit at {}",
        hit.location[0]
    );
}

/// Bamboo's *collision* box moves with the offset too, which is what the
/// physics layer walks into.
#[test]
fn a_bamboo_collision_box_follows_its_offset() {
    let registry = Registry::get();
    let bamboo = state("minecraft:bamboo");
    let mut checked = 0;
    for (x, z) in [(4, 0), (5, 0), (7, 3)] {
        let shift = registry.shape_offset(bamboo, x, 64, z);
        if shift[0].abs() < 1.0e-6 && shift[2].abs() < 1.0e-6 {
            continue;
        }
        let world = world_with_block(x, 64, z, "minecraft:bamboo");
        let bbox = Aabb::new(
            x as f64 - 0.5,
            64.0,
            z as f64 - 0.5,
            x as f64 + 1.5,
            65.0,
            z as f64 + 1.5,
        );
        let collisions = block_collisions(&world, registry, &bbox);
        let placed = collisions
            .iter()
            .find(|placed| {
                (placed.offset[0] - (x as f64 + shift[0])).abs() < 1.0e-9
                    && (placed.offset[2] - (z as f64 + shift[2])).abs() < 1.0e-9
            })
            .unwrap_or_else(|| {
                panic!(
                    "no collision box placed at the shifted position for ({x}, 64, {z}); \
                     shifts {shift:?}, found {:?}",
                    collisions.iter().map(|p| p.offset).collect::<Vec<_>>()
                )
            });
        assert!(
            placed.shape.size(Axis::Y) > 0,
            "bamboo has a collision shape to move"
        );
        checked += 1;
    }
    assert!(checked >= 2, "the test positions need differing offsets");
}

/// A block with no offset type is placed exactly in its cell.
#[test]
fn a_plain_block_has_no_shift() {
    let registry = Registry::get();
    let stone = state("minecraft:stone");
    assert_eq!(registry.shape_offset(stone, 4, 64, 0), [0.0; 3]);
    let world = world_with_block(4, 64, 0, "minecraft:stone");
    let bbox = Aabb::new(3.5, 64.0, -0.5, 5.5, 65.0, 1.5);
    let collisions = block_collisions(&world, registry, &bbox);
    assert!(
        collisions
            .iter()
            .any(|placed| placed.offset == [4.0, 64.0, 0.0]),
        "stone sits at its own position"
    );
}
