//! Ray casts against block outlines: `BlockGetter.clip` with
//! `ClipContext.Block.OUTLINE` and no fluids, which is what the crosshair
//! uses (`Entity.pick`).

use crate::aabb::Axis;
use crate::shape::Shape;
use crate::{Registry, World};

/// `Direction`, in vanilla's order (the ordinal goes on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Direction {
    pub const VALUES: [Direction; 6] =
        [Direction::Down, Direction::Up, Direction::North, Direction::South, Direction::West, Direction::East];

    pub fn normal(self) -> (i32, i32, i32) {
        match self {
            Direction::Down => (0, -1, 0),
            Direction::Up => (0, 1, 0),
            Direction::North => (0, 0, -1),
            Direction::South => (0, 0, 1),
            Direction::West => (-1, 0, 0),
            Direction::East => (1, 0, 0),
        }
    }

    pub fn opposite(self) -> Direction {
        match self {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
        }
    }

    /// `Direction.getApproximateNearest`: in floats, and starting from
    /// `Float.MIN_VALUE`, so a zero vector gives north.
    pub fn approximate_nearest(dx: f64, dy: f64, dz: f64) -> Direction {
        let (dx, dy, dz) = (dx as f32, dy as f32, dz as f32);
        let mut result = Direction::North;
        let mut highest = f32::from_bits(1);
        for direction in Direction::VALUES {
            let (nx, ny, nz) = direction.normal();
            let dot = dx * nx as f32 + dy * ny as f32 + dz * nz as f32;
            if dot > highest {
                highest = dot;
                result = direction;
            }
        }
        result
    }
}

/// `BlockHitResult`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockHit {
    pub pos: (i32, i32, i32),
    pub location: [f64; 3],
    pub direction: Direction,
    /// The ray started inside the shape.
    pub inside: bool,
    pub miss: bool,
}

const EPSILON: f64 = 1.0e-7;

/// `AABB.clipPoint`.
#[allow(clippy::too_many_arguments)]
fn clip_point(
    scale: &mut f64,
    direction: Option<Direction>,
    da: f64,
    db: f64,
    dc: f64,
    point: f64,
    min_b: f64,
    max_b: f64,
    min_c: f64,
    max_c: f64,
    new_direction: Direction,
    from_a: f64,
    from_b: f64,
    from_c: f64,
) -> Option<Direction> {
    let s = (point - from_a) / da;
    let pb = from_b + s * db;
    let pc = from_c + s * dc;
    if 0.0 < s && s < *scale && min_b - EPSILON < pb && pb < max_b + EPSILON && min_c - EPSILON < pc && pc < max_c + EPSILON {
        *scale = s;
        Some(new_direction)
    } else {
        direction
    }
}

/// `AABB.getDirection`: the face of a box the ray enters through, if it
/// does so before `scale`.
pub fn box_entry(
    min: [f64; 3],
    max: [f64; 3],
    from: [f64; 3],
    scale: &mut f64,
    mut direction: Option<Direction>,
    d: [f64; 3],
) -> Option<Direction> {
    let [dx, dy, dz] = d;
    if dx > EPSILON {
        direction = clip_point(scale, direction, dx, dy, dz, min[0], min[1], max[1], min[2], max[2], Direction::West, from[0], from[1], from[2]);
    } else if dx < -EPSILON {
        direction = clip_point(scale, direction, dx, dy, dz, max[0], min[1], max[1], min[2], max[2], Direction::East, from[0], from[1], from[2]);
    }
    if dy > EPSILON {
        direction = clip_point(scale, direction, dy, dz, dx, min[1], min[2], max[2], min[0], max[0], Direction::Down, from[1], from[2], from[0]);
    } else if dy < -EPSILON {
        direction = clip_point(scale, direction, dy, dz, dx, max[1], min[2], max[2], min[0], max[0], Direction::Up, from[1], from[2], from[0]);
    }
    if dz > EPSILON {
        direction = clip_point(scale, direction, dz, dx, dy, min[2], min[0], max[0], min[1], max[1], Direction::North, from[2], from[0], from[1]);
    } else if dz < -EPSILON {
        direction = clip_point(scale, direction, dz, dx, dy, max[2], min[0], max[0], min[1], max[1], Direction::South, from[2], from[0], from[1]);
    }
    direction
}

impl Shape {
    /// `VoxelShape.clip` for this shape at block `pos`.
    pub fn clip(&self, from: [f64; 3], to: [f64; 3], pos: (i32, i32, i32)) -> Option<BlockHit> {
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1.0e-7 {
            return None;
        }
        let offset = [pos.0 as f64, pos.1 as f64, pos.2 as f64];
        let test = [from[0] + d[0] * 0.001, from[1] + d[1] * 0.001, from[2] + d[2] * 0.001];
        if self.contains_point([test[0] - offset[0], test[1] - offset[1], test[2] - offset[2]]) {
            let direction = Direction::approximate_nearest(d[0], d[1], d[2]).opposite();
            return Some(BlockHit { pos, location: test, direction, inside: true, miss: false });
        }
        // AABB.clip over toAabbs(). Vanilla merges neighbouring cells into
        // larger boxes first; cell by cell finds the same entry point.
        let mut scale = 1.0;
        let mut direction = None;
        let (xs, ys, zs) = (self.coords(Axis::X), self.coords(Axis::Y), self.coords(Axis::Z));
        for x in 0..self.size(Axis::X) {
            for y in 0..self.size(Axis::Y) {
                for z in 0..self.size(Axis::Z) {
                    if !self.cell(x, y, z) {
                        continue;
                    }
                    let min = [xs[x] + offset[0], ys[y] + offset[1], zs[z] + offset[2]];
                    let max = [xs[x + 1] + offset[0], ys[y + 1] + offset[1], zs[z + 1] + offset[2]];
                    direction = box_entry(min, max, from, &mut scale, direction, d);
                }
            }
        }
        let direction = direction?;
        let location = [from[0] + scale * d[0], from[1] + scale * d[1], from[2] + scale * d[2]];
        Some(BlockHit { pos, location, direction, inside: false, miss: false })
    }
}

/// `Vec3.subtract(..).lengthSqr()`.
fn distance_sqr(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (x, y, z) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    x * x + y * y + z * z
}

/// `Mth.lerp(alpha, p0, p1)`.
fn lerp(alpha: f64, p0: f64, p1: f64) -> f64 {
    p0 + alpha * (p1 - p0)
}

/// `Mth.floor`.
fn floor(v: f64) -> i32 {
    let i = v as i32;
    if v < i as f64 { i - 1 } else { i }
}

/// `Mth.frac`.
fn frac(v: f64) -> f64 {
    v - (v.floor() as i64) as f64
}

fn sign(v: f64) -> i32 {
    if v == 0.0 {
        0
    } else if v > 0.0 {
        1
    } else {
        -1
    }
}

/// `BlockGetter.traverseBlocks`: the blocks a segment passes through, in
/// order, until `visit` returns something.
pub fn traverse_blocks<T>(from: [f64; 3], to: [f64; 3], mut visit: impl FnMut((i32, i32, i32)) -> Option<T>) -> Option<T> {
    if from == to {
        return None;
    }
    let to_x = lerp(-1.0e-7, to[0], from[0]);
    let to_y = lerp(-1.0e-7, to[1], from[1]);
    let to_z = lerp(-1.0e-7, to[2], from[2]);
    let from_x = lerp(-1.0e-7, from[0], to[0]);
    let from_y = lerp(-1.0e-7, from[1], to[1]);
    let from_z = lerp(-1.0e-7, from[2], to[2]);
    let (mut x, mut y, mut z) = (floor(from_x), floor(from_y), floor(from_z));
    if let Some(found) = visit((x, y, z)) {
        return Some(found);
    }
    let (dx, dy, dz) = (to_x - from_x, to_y - from_y, to_z - from_z);
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let t_delta_x = if sx == 0 { f64::MAX } else { sx as f64 / dx };
    let t_delta_y = if sy == 0 { f64::MAX } else { sy as f64 / dy };
    let t_delta_z = if sz == 0 { f64::MAX } else { sz as f64 / dz };
    let mut t_x = t_delta_x * if sx > 0 { 1.0 - frac(from_x) } else { frac(from_x) };
    let mut t_y = t_delta_y * if sy > 0 { 1.0 - frac(from_y) } else { frac(from_y) };
    let mut t_z = t_delta_z * if sz > 0 { 1.0 - frac(from_z) } else { frac(from_z) };
    while t_x <= 1.0 || t_y <= 1.0 || t_z <= 1.0 {
        if t_x < t_y {
            if t_x < t_z {
                x += sx;
                t_x += t_delta_x;
            } else {
                z += sz;
                t_z += t_delta_z;
            }
        } else if t_y < t_z {
            y += sy;
            t_y += t_delta_y;
        } else {
            z += sz;
            t_z += t_delta_z;
        }
        if let Some(found) = visit((x, y, z)) {
            return Some(found);
        }
    }
    None
}

/// `BlockGetter.clip` for block outlines. Blocks whose outline is offset
/// by their position (`BlockBehaviour.OffsetType`: bamboo, small flowers)
/// are tested at their unshifted place.
pub fn clip(world: &World, from: [f64; 3], to: [f64; 3]) -> BlockHit {
    let registry = Registry::get();
    let hit = traverse_blocks(from, to, |pos| {
        let state = world.block_state_or_air(pos.0, pos.1, pos.2);
        let mut hit = registry.outline_shape(state)?.clip(from, to, pos)?;
        // clipWithInteractionOverride
        if let Some(inner) = registry.interaction_shape(state).and_then(|s| s.clip(from, to, pos)) {
            if distance_sqr(inner.location, from) < distance_sqr(hit.location, from) {
                hit.direction = inner.direction;
            }
        }
        Some(hit)
    });
    hit.unwrap_or_else(|| {
        let direction = Direction::approximate_nearest(from[0] - to[0], from[1] - to[1], from[2] - to[2]);
        BlockHit { pos: (floor(to[0]), floor(to[1]), floor(to[2])), location: to, direction, inside: false, miss: true }
    })
}
