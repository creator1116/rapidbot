//! `BlockCollisions` and `Shapes.collide`.

use crate::aabb::{Aabb, Axis};
use crate::registry::Registry;
use crate::shape::PlacedShape;
use crate::world::World;

/// `Mth.floor(double)`.
fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// Every block collision shape touching `bbox`, in `BlockCollisions` order.
///
/// Not modelled yet: entity-dependent shapes (scaffolding, powder snow),
/// entity colliders (boats, shulkers) and the world border.
pub fn block_collisions<'r>(world: &World, registry: &'r Registry, bbox: &Aabb) -> Vec<PlacedShape<'r>> {
    let x0 = floor(bbox.min_x - 1.0e-7) - 1;
    let x1 = floor(bbox.max_x + 1.0e-7) + 1;
    let y0 = floor(bbox.min_y - 1.0e-7) - 1;
    let y1 = floor(bbox.max_y + 1.0e-7) + 1;
    let z0 = floor(bbox.min_z - 1.0e-7) - 1;
    let z1 = floor(bbox.max_z + 1.0e-7) + 1;

    let mut out = Vec::new();
    // Cursor3D: x fastest, then y, then z.
    for z in z0..=z1 {
        for y in y0..=y1 {
            for x in x0..=x1 {
                let face_type = (x == x0 || x == x1) as u8 + (y == y0 || y == y1) as u8 + (z == z0 || z == z1) as u8;
                if face_type == 3 {
                    continue;
                }
                // Unloaded chunks contribute nothing (getChunkForCollisions
                // returns null).
                let Some(state) = world.block_state(x, y, z) else { continue };
                let info = registry.state(state);
                if face_type == 1 && !info.has_large_collision_shape {
                    continue;
                }
                if face_type == 2 && registry.block_of(state).name != "minecraft:moving_piston" {
                    continue;
                }
                let Some(shape) = registry.collision_shape(state) else { continue };
                let placed = shape.at(x, y, z);
                if shape.is_block {
                    if bbox.intersects(x as f64, y as f64, z as f64, x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0) {
                        out.push(placed);
                    }
                } else if placed.intersects_box(bbox) {
                    out.push(placed);
                }
            }
        }
    }
    out
}

/// `Shapes.collide`.
pub fn collide(axis: Axis, moving: &Aabb, shapes: &[PlacedShape<'_>], mut distance: f64) -> f64 {
    for shape in shapes {
        if distance.abs() < 1.0e-7 {
            return 0.0;
        }
        distance = shape.collide(axis, moving, distance);
    }
    distance
}
