//! `WorldBorder`, the parts the client simulates.

use crate::aabb::{Aabb, Axis};

/// Vanilla's `WorldBorder.MAX_SIZE`.
pub const MAX_SIZE: f64 = 5.999997e7;
/// Vanilla's `WorldBorder.MAX_CENTER_COORDINATE`.
pub const MAX_CENTER_COORDINATE: f64 = 2.9999984e7;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Lerp {
    from: f64,
    to: f64,
    duration: u32,
    progress: u32,
}

/// `net.minecraft.world.level.border.WorldBorder` for one level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldBorder {
    center_x: f64,
    center_z: f64,
    size: f64,
    previous_size: f64,
    lerp: Option<Lerp>,
    absolute_max_size: f64,
    warning_blocks: i32,
    warning_time: i32,
}

impl Default for WorldBorder {
    fn default() -> Self {
        Self {
            center_x: 0.0,
            center_z: 0.0,
            size: MAX_SIZE,
            previous_size: MAX_SIZE,
            lerp: None,
            absolute_max_size: 29_999_984.0,
            warning_blocks: 5,
            warning_time: 15,
        }
    }
}

impl WorldBorder {
    pub fn center_x(&self) -> f64 {
        self.center_x
    }

    pub fn center_z(&self) -> f64 {
        self.center_z
    }

    pub fn set_center(&mut self, x: f64, z: f64) {
        self.center_x = x;
        self.center_z = z;
    }

    pub fn size(&self) -> f64 {
        self.size
    }

    /// `getLerpTarget`: the size the border is heading to.
    pub fn lerp_target(&self) -> f64 {
        self.lerp.map_or(self.size, |l| l.to)
    }

    /// `getLerpTime`: ticks left in the current lerp.
    pub fn lerp_time(&self) -> u32 {
        self.lerp.map_or(0, |l| l.progress)
    }

    pub fn set_size(&mut self, size: f64) {
        self.size = size;
        self.previous_size = size;
        self.lerp = None;
    }

    /// `lerpSizeBetween`. `ticks` is the duration in ticks.
    pub fn lerp_size_between(&mut self, from: f64, to: f64, ticks: u32) {
        if from == to || ticks == 0 {
            self.set_size(to);
            return;
        }
        self.size = from;
        self.previous_size = from;
        self.lerp = Some(Lerp {
            from,
            to,
            duration: ticks,
            progress: ticks,
        });
    }

    pub fn tick(&mut self) {
        let Some(mut lerp) = self.lerp.take() else {
            return;
        };
        lerp.progress = lerp.progress.saturating_sub(1);
        self.previous_size = self.size;
        let age = (lerp.duration - lerp.progress) as f64;
        let progress = age / lerp.duration as f64;
        self.size = if progress < 1.0 {
            lerp.from + (lerp.to - lerp.from) * progress
        } else {
            lerp.to
        };
        if lerp.progress == 0 {
            self.size = lerp.to;
        } else {
            self.lerp = Some(lerp);
        }
    }

    /// `getSize` at partial tick 0.
    pub fn previous_size(&self) -> f64 {
        self.previous_size
    }

    pub fn min_x(&self) -> f64 {
        clamp(
            self.center_x - self.size / 2.0,
            -self.absolute_max_size,
            self.absolute_max_size,
        )
    }

    pub fn max_x(&self) -> f64 {
        clamp(
            self.center_x + self.size / 2.0,
            -self.absolute_max_size,
            self.absolute_max_size,
        )
    }

    pub fn min_z(&self) -> f64 {
        clamp(
            self.center_z - self.size / 2.0,
            -self.absolute_max_size,
            self.absolute_max_size,
        )
    }

    pub fn max_z(&self) -> f64 {
        clamp(
            self.center_z + self.size / 2.0,
            -self.absolute_max_size,
            self.absolute_max_size,
        )
    }

    pub fn absolute_max_size(&self) -> i32 {
        self.absolute_max_size as i32
    }

    pub fn set_absolute_max_size(&mut self, size: i32) {
        self.absolute_max_size = size as f64;
    }

    pub fn warning_blocks(&self) -> i32 {
        self.warning_blocks
    }

    pub fn set_warning_blocks(&mut self, blocks: i32) {
        self.warning_blocks = blocks;
    }

    pub fn warning_time(&self) -> i32 {
        self.warning_time
    }

    pub fn set_warning_time(&mut self, time: i32) {
        self.warning_time = time;
    }

    /// `getDistanceToBorder(x, z)`: the smallest distance to a side.
    pub fn distance_to_border(&self, x: f64, z: f64) -> f64 {
        let from_north = z - self.min_z();
        let from_south = self.max_z() - z;
        let from_west = x - self.min_x();
        let from_east = self.max_x() - x;
        from_west.min(from_east).min(from_north).min(from_south)
    }

    pub fn is_within_bounds(&self, x: f64, z: f64, margin: f64) -> bool {
        x >= self.min_x() - margin
            && x < self.max_x() + margin
            && z >= self.min_z() - margin
            && z < self.max_z() + margin
    }

    /// `isInsideCloseToBorder`: whether the border shape joins this move.
    pub fn is_inside_close_to_border(&self, source_x: f64, source_z: f64, bbox: &Aabb) -> bool {
        let bb_max = abs_max(bbox.get_xsize(), bbox.get_zsize()).max(1.0);
        self.distance_to_border(source_x, source_z) < bb_max * 2.0
            && self.is_within_bounds(source_x, source_z, bb_max)
    }

    /// The block-aligned bounds of the infinite collision shape.
    pub fn collision_bounds(&self) -> (f64, f64, f64, f64) {
        (
            self.min_x().floor(),
            self.min_z().floor(),
            self.max_x().ceil(),
            self.max_z().ceil(),
        )
    }

    /// Collide `moving` along `axis` with the part of the world outside the
    /// border. This is the axis step of `Shapes.collide` against
    /// `Shapes.join(INFINITY, box, ONLY_FIRST)`.
    pub fn collide(&self, axis: Axis, moving: &Aabb, distance: f64) -> f64 {
        let (west, north, east, south) = self.collision_bounds();
        match axis {
            Axis::X => {
                if distance > 0.0 {
                    let edge = east;
                    if moving.max_x >= edge {
                        0.0
                    } else if moving.max_x + distance > edge {
                        edge - moving.max_x
                    } else {
                        distance
                    }
                } else if distance < 0.0 {
                    let edge = west;
                    if moving.min_x <= edge {
                        0.0
                    } else if moving.min_x + distance < edge {
                        edge - moving.min_x
                    } else {
                        distance
                    }
                } else {
                    0.0
                }
            }
            Axis::Z => {
                if distance > 0.0 {
                    let edge = south;
                    if moving.max_z >= edge {
                        0.0
                    } else if moving.max_z + distance > edge {
                        edge - moving.max_z
                    } else {
                        distance
                    }
                } else if distance < 0.0 {
                    let edge = north;
                    if moving.min_z <= edge {
                        0.0
                    } else if moving.min_z + distance < edge {
                        edge - moving.min_z
                    } else {
                        distance
                    }
                } else {
                    0.0
                }
            }
            Axis::Y => distance,
        }
    }
}

fn clamp(v: f64, min: f64, max: f64) -> f64 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

fn abs_max(a: f64, b: f64) -> f64 {
    a.abs().max(b.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_border_matches_vanilla() {
        let border = WorldBorder::default();
        assert_eq!(border.min_x(), -29_999_984.0);
        assert_eq!(border.max_x(), 29_999_984.0);
        assert_eq!(border.warning_time(), 15);
    }

    #[test]
    fn lerp_starts_at_the_old_size_and_ends_at_the_new_one() {
        let mut border = WorldBorder::default();
        border.set_size(100.0);
        border.lerp_size_between(100.0, 20.0, 4);
        assert_eq!(border.size(), 100.0);
        assert_eq!(border.lerp_target(), 20.0);
        border.tick();
        assert!((border.size() - 80.0).abs() < 1.0e-9);
        border.tick();
        assert!((border.size() - 60.0).abs() < 1.0e-9);
        border.tick();
        assert!((border.size() - 40.0).abs() < 1.0e-9);
        border.tick();
        assert_eq!(border.size(), 20.0);
        assert_eq!(border.lerp_time(), 0);
    }

    #[test]
    fn collision_stops_at_the_block_aligned_edge() {
        let mut border = WorldBorder::default();
        border.set_size(10.0);
        let moving = Aabb::new(4.4, 0.0, 0.0, 4.8, 1.8, 0.4);
        assert!((border.collide(Axis::X, &moving, 0.1) - 0.1).abs() < 1.0e-9);
        let moving = Aabb::new(4.4, 0.0, 0.0, 4.8, 1.8, 0.4);
        assert!((border.collide(Axis::X, &moving, 2.0) - 0.2).abs() < 1.0e-9);
    }
}
