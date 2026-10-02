//! Block collision shapes, ported from `VoxelShape` so collisions match
//! vanilla to the last bit.
//!
//! Every block shape used in collision has been moved to its block position
//! (`VoxelShape.move` → `ArrayVoxelShape` over `OffsetDoubleList`), so
//! coordinates are always `base + offset`, found by binary search.

use crate::aabb::{Aabb, Axis};

const EPSILON: f64 = 1.0e-7;

/// An unmoved block shape as exported from vanilla.
#[derive(Debug, Clone)]
pub struct Shape {
    /// Per axis coordinate lists (`getCoords`), one longer than the cell count.
    coords: [Vec<f64>; 3],
    /// Filled cells, indexed x-major: `(x * ysize + y) * zsize + z`.
    fill: Vec<bool>,
    /// First and last filled cell index along each axis.
    first_full: [usize; 3],
    last_full: [usize; 3],
    /// `== Shapes.block()`, which `BlockCollisions` tests with a plain AABB
    /// intersection instead of `joinIsNotEmpty`.
    pub is_block: bool,
}

/// A shape placed at a block position.
#[derive(Debug, Clone, Copy)]
pub struct PlacedShape<'a> {
    pub shape: &'a Shape,
    pub offset: [f64; 3],
}

impl Shape {
    pub fn new(coords: [Vec<f64>; 3], fill: Vec<bool>, is_block: bool) -> Self {
        let sizes = [coords[0].len() - 1, coords[1].len() - 1, coords[2].len() - 1];
        assert_eq!(fill.len(), sizes[0] * sizes[1] * sizes[2]);
        let mut first_full = [usize::MAX; 3];
        let mut last_full = [0; 3];
        for x in 0..sizes[0] {
            for y in 0..sizes[1] {
                for z in 0..sizes[2] {
                    if fill[(x * sizes[1] + y) * sizes[2] + z] {
                        for (axis, v) in [x, y, z].into_iter().enumerate() {
                            first_full[axis] = first_full[axis].min(v);
                            last_full[axis] = last_full[axis].max(v);
                        }
                    }
                }
            }
        }
        Self { coords, fill, first_full, last_full, is_block }
    }

    pub fn size(&self, axis: Axis) -> usize {
        self.coords[axis.index()].len() - 1
    }

    pub fn coords(&self, axis: Axis) -> &[f64] {
        &self.coords[axis.index()]
    }

    fn is_full(&self, x: usize, y: usize, z: usize) -> bool {
        self.fill[(x * self.size(Axis::Y) + y) * self.size(Axis::Z) + z]
    }

    /// `DiscreteVoxelShape.isFullWide`: out-of-range cells are empty.
    fn is_full_wide(&self, x: i64, y: i64, z: i64) -> bool {
        if x < 0 || y < 0 || z < 0 {
            return false;
        }
        let (x, y, z) = (x as usize, y as usize, z as usize);
        x < self.size(Axis::X) && y < self.size(Axis::Y) && z < self.size(Axis::Z) && self.is_full(x, y, z)
    }

    /// Whether a cell is filled.
    pub(crate) fn cell(&self, x: usize, y: usize, z: usize) -> bool {
        self.is_full(x, y, z)
    }

    /// Whether a point, relative to the block's corner, is in a filled
    /// cell (`findIndex` on each axis, then `isFullWide`).
    pub(crate) fn contains_point(&self, p: [f64; 3]) -> bool {
        let placed = self.at(0, 0, 0);
        self.is_full_wide(placed.find_index(Axis::X, p[0]), placed.find_index(Axis::Y, p[1]), placed.find_index(Axis::Z, p[2]))
    }

    pub fn at(&self, x: i32, y: i32, z: i32) -> PlacedShape<'_> {
        PlacedShape { shape: self, offset: [x as f64, y as f64, z as f64] }
    }
}

impl PlacedShape<'_> {
    /// `OffsetDoubleList.getDouble`: base coordinate plus offset.
    pub fn get(&self, axis: Axis, i: usize) -> f64 {
        self.shape.coords[axis.index()][i] + self.offset[axis.index()]
    }

    /// Coordinates along `axis`, moved.
    pub fn coords(&self, axis: Axis) -> impl Iterator<Item = f64> + '_ {
        let offset = self.offset[axis.index()];
        self.shape.coords[axis.index()].iter().map(move |c| c + offset)
    }

    /// `VoxelShape.findIndex`: `Mth.binarySearch(0, size + 1, i -> coord < get(i)) - 1`.
    fn find_index(&self, axis: Axis, coord: f64) -> i64 {
        let mut from = 0usize;
        let mut len = self.shape.size(axis) + 1;
        while len > 0 {
            let half = len / 2;
            let middle = from + half;
            if coord < self.get(axis, middle) {
                len = half;
            } else {
                from = middle + 1;
                len -= half + 1;
            }
        }
        from as i64 - 1
    }

    pub fn min(&self, axis: Axis) -> f64 {
        self.get(axis, self.shape.first_full[axis.index()])
    }

    pub fn max(&self, axis: Axis) -> f64 {
        self.get(axis, self.shape.last_full[axis.index()] + 1)
    }

    /// `VoxelShape.collide` / `collideX`: how far `moving` can travel along
    /// `axis` (up to `distance`) before hitting this shape.
    pub fn collide(&self, axis: Axis, moving: &Aabb, mut distance: f64) -> f64 {
        if distance.abs() < EPSILON {
            return 0.0;
        }
        let (b_axis, c_axis) = axis.cycle();
        let max_a = moving.max(axis);
        let min_a = moving.min(axis);
        let a_min = self.find_index(axis, min_a + EPSILON);
        let a_max = self.find_index(axis, max_a - EPSILON);
        let b_min = self.find_index(b_axis, moving.min(b_axis) + EPSILON).max(0);
        let b_max = (self.find_index(b_axis, moving.max(b_axis) - EPSILON) + 1).min(self.shape.size(b_axis) as i64);
        let c_min = self.find_index(c_axis, moving.min(c_axis) + EPSILON).max(0);
        let c_max = (self.find_index(c_axis, moving.max(c_axis) - EPSILON) + 1).min(self.shape.size(c_axis) as i64);
        let a_size = self.shape.size(axis) as i64;

        // Map (a, b, c) back to (x, y, z) for the fill lookup.
        let full = |a: i64, b: i64, c: i64| {
            let mut xyz = [0i64; 3];
            xyz[axis.index()] = a;
            xyz[b_axis.index()] = b;
            xyz[c_axis.index()] = c;
            self.shape.is_full_wide(xyz[0], xyz[1], xyz[2])
        };

        if distance > 0.0 {
            for a in (a_max + 1)..a_size {
                for b in b_min..b_max {
                    for c in c_min..c_max {
                        if full(a, b, c) {
                            let new_distance = self.get(axis, a as usize) - max_a;
                            if new_distance >= -EPSILON {
                                distance = distance.min(new_distance);
                            }
                            // Vanilla returns at the first filled cell, even
                            // when it is behind the box.
                            return distance;
                        }
                    }
                }
            }
        } else if distance < 0.0 {
            let mut a = a_min - 1;
            while a >= 0 {
                for b in b_min..b_max {
                    for c in c_min..c_max {
                        if full(a, b, c) {
                            let new_distance = self.get(axis, (a + 1) as usize) - min_a;
                            if new_distance <= EPSILON {
                                distance = distance.max(new_distance);
                            }
                            return distance;
                        }
                    }
                }
                a -= 1;
            }
        }
        distance
    }

    /// `Shapes.joinIsNotEmpty(this, Shapes.create(box), BooleanOp.AND)`.
    /// The box shape is a single full cell spanning the box (entity boxes
    /// never sit on the 1/8 grid that would make it a cube shape).
    pub fn intersects_box(&self, b: &Aabb) -> bool {
        for axis in Axis::ALL {
            if self.max(axis) < b.min(axis) - EPSILON || b.max(axis) < self.min(axis) - EPSILON {
                return false;
            }
        }
        let mut merged: [Vec<(i64, i64)>; 3] = Default::default();
        for axis in Axis::ALL {
            let first: Vec<f64> = self.coords(axis).collect();
            let second = [b.min(axis), b.max(axis)];
            match merge(&first, &second) {
                Some(pairs) => merged[axis.index()] = pairs,
                None => return false,
            }
        }
        for &(x1, x2) in &merged[0] {
            for &(y1, y2) in &merged[1] {
                for &(z1, z2) in &merged[2] {
                    // The box shape is full only at cell (0, 0, 0).
                    let second_full = x2 == 0 && y2 == 0 && z2 == 0;
                    if second_full && self.shape.is_full_wide(x1, y1, z1) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// `Shapes.createIndexMerger` for an AND with neither side mattering alone.
/// Returns the `(firstIndex, secondIndex)` pairs `forMergedIndexes` visits,
/// or `None` for a `NonOverlappingMerger` (which can never satisfy AND).
fn merge(first: &[f64], second: &[f64]) -> Option<Vec<(i64, i64)>> {
    let first_size = first.len() - 1;
    let second_size = second.len() - 1;
    if first[first_size] < second[0] - EPSILON || second[second_size] < first[0] - EPSILON {
        return None;
    }
    if first_size == second_size && first == second {
        // IdenticalMerger.
        return Some((0..first_size as i64).map(|i| (i, i)).collect());
    }

    // IndirectMerger(first, second, false, false).
    let capacity = first.len() + second.len();
    let mut result = Vec::with_capacity(capacity);
    let mut first_indices = Vec::with_capacity(capacity);
    let mut second_indices = Vec::with_capacity(capacity);
    let mut last_value = f64::NAN;
    let (mut fi, mut si) = (0usize, 0usize);
    loop {
        let ran_out_first = fi >= first.len();
        let ran_out_second = si >= second.len();
        if ran_out_first && ran_out_second {
            break;
        }
        let chose_first = !ran_out_first && (ran_out_second || first[fi] < second[si] + EPSILON);
        if chose_first {
            fi += 1;
            if si == 0 || ran_out_second {
                continue;
            }
        } else {
            si += 1;
            if fi == 0 || ran_out_first {
                continue;
            }
        }
        let cur_first = fi as i64 - 1;
        let cur_second = si as i64 - 1;
        let next_value = if chose_first { first[fi - 1] } else { second[si - 1] };
        if !(last_value >= next_value - EPSILON) {
            first_indices.push(cur_first);
            second_indices.push(cur_second);
            result.push(next_value);
            last_value = next_value;
        } else {
            let n = first_indices.len();
            first_indices[n - 1] = cur_first;
            second_indices[n - 1] = cur_second;
        }
    }
    let result_length = result.len().max(1);
    Some((0..result_length - 1).map(|i| (first_indices[i], second_indices[i])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Shape {
        Shape::new([vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]], vec![true], true)
    }

    fn slab() -> Shape {
        Shape::new([vec![0.0, 1.0], vec![0.0, 0.5], vec![0.0, 1.0]], vec![true], false)
    }

    #[test]
    fn falling_onto_a_block() {
        let c = cube();
        let placed = c.at(0, 63, 0);
        let player = Aabb::new(0.2, 64.5, 0.2, 0.8, 66.3, 0.8);
        assert_eq!(placed.collide(Axis::Y, &player, -1.0), -0.5);
        // Moving up and away is unaffected.
        assert_eq!(placed.collide(Axis::Y, &player, 1.0), 1.0);
    }

    #[test]
    fn sideways_into_a_slab_only_if_overlapping_vertically() {
        let s = slab();
        let placed = s.at(1, 64, 0);
        let low = Aabb::new(0.2, 64.0, 0.2, 0.8, 65.8, 0.8);
        assert!((placed.collide(Axis::X, &low, 1.0) - 0.2).abs() < 1e-12);
        let high = Aabb::new(0.2, 64.5, 0.2, 0.8, 66.3, 0.8);
        assert_eq!(placed.collide(Axis::X, &high, 1.0), 1.0);
    }

    #[test]
    fn join_is_not_empty() {
        let s = slab();
        let placed = s.at(0, 64, 0);
        assert!(placed.intersects_box(&Aabb::new(0.2, 64.2, 0.2, 0.8, 66.0, 0.8)));
        assert!(!placed.intersects_box(&Aabb::new(0.2, 64.6, 0.2, 0.8, 66.0, 0.8)));
        // Merely touching the top face is not an intersection.
        assert!(!placed.intersects_box(&Aabb::new(0.2, 64.5, 0.2, 0.8, 66.0, 0.8)));
    }
}
