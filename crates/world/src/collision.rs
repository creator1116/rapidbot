//! `BlockCollisions` and `Shapes.collide`.

use std::sync::OnceLock;

use crate::aabb::{Aabb, Axis};
use crate::registry::Registry;
use crate::shape::{PlacedShape, Shape};
use crate::world::World;

/// `Mth.floor(double)`.
fn floor(v: f64) -> i32 {
    v.floor() as i32
}

/// The part of `EntityCollisionContext` that affects block collision shapes.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntityCollisionContext {
    /// `Entity.getY()`, or `None` for a context without an entity.
    pub entity_bottom: Option<f64>,
    /// `Entity.isDescending()` (shift held).
    pub descending: bool,
    /// `Entity.fallDistance`.
    pub fall_distance: f64,
    /// Leather boots, or another entity allowed to walk on powder snow.
    pub can_walk_on_powder_snow: bool,
}

impl EntityCollisionContext {
    fn is_above(&self, y: i32, shape_top: f64) -> bool {
        self.entity_bottom
            .is_some_and(|bottom| bottom > y as f64 + shape_top - 1.0e-5)
    }
}

fn full_block_shape() -> &'static Shape {
    static SHAPE: OnceLock<Shape> = OnceLock::new();
    SHAPE.get_or_init(|| {
        Shape::new(
            [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
            vec![true],
            true,
        )
    })
}

fn falling_powder_snow_shape() -> &'static Shape {
    static SHAPE: OnceLock<Shape> = OnceLock::new();
    SHAPE.get_or_init(|| {
        Shape::new(
            [vec![0.0, 1.0], vec![0.0, 0.9], vec![0.0, 1.0]],
            vec![true],
            false,
        )
    })
}

/// `PowderSnowBlock.getCollisionShape` with an entity context.
fn powder_snow_shape(y: i32, context: &EntityCollisionContext) -> Option<&'static Shape> {
    context.entity_bottom?;
    if context.fall_distance > 2.5 {
        return Some(falling_powder_snow_shape());
    }
    if context.can_walk_on_powder_snow && context.is_above(y, 1.0) && !context.descending {
        return Some(full_block_shape());
    }
    None
}

/// Every block collision shape touching `bbox`, in `BlockCollisions` order.
///
/// Entity-dependent shapes are supplied through `context`; entity colliders
/// (boats, shulkers), scaffolding, and the world border are not modelled yet.
pub fn block_collisions<'r>(
    world: &World,
    registry: &'r Registry,
    bbox: &Aabb,
) -> Vec<PlacedShape<'r>> {
    block_collisions_with(world, registry, bbox, EntityCollisionContext::default())
}

/// [`block_collisions`] with an entity collision context.
pub fn block_collisions_with<'r>(
    world: &World,
    registry: &'r Registry,
    bbox: &Aabb,
    context: EntityCollisionContext,
) -> Vec<PlacedShape<'r>> {
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
                let face_type = (x == x0 || x == x1) as u8
                    + (y == y0 || y == y1) as u8
                    + (z == z0 || z == z1) as u8;
                if face_type == 3 {
                    continue;
                }
                // Unloaded chunks contribute nothing (getChunkForCollisions
                // returns null).
                let Some(state) = world.block_state(x, y, z) else {
                    continue;
                };
                let info = registry.state(state);
                let block = registry.block_of(state);
                let powder_snow = block.name == "minecraft:powder_snow";
                if !powder_snow {
                    if face_type == 1 && !info.has_large_collision_shape {
                        continue;
                    }
                    if face_type == 2 && block.name != "minecraft:moving_piston" {
                        continue;
                    }
                }
                let shape = if powder_snow {
                    let Some(shape) = powder_snow_shape(y, &context) else {
                        continue;
                    };
                    shape
                } else {
                    let Some(shape) = registry.collision_shape(state) else {
                        continue;
                    };
                    shape
                };
                let placed = shape.at(x, y, z);
                if shape.is_block {
                    if bbox.intersects(
                        x as f64,
                        y as f64,
                        z as f64,
                        x as f64 + 1.0,
                        y as f64 + 1.0,
                        z as f64 + 1.0,
                    ) {
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
