//! Block data, chunk storage and vanilla-exact block collision.

pub mod aabb;
pub mod chunk;
pub mod collision;
pub mod fluid;
pub mod item;
pub mod raycast;
pub mod registry;
pub mod shape;
pub mod tags;
pub mod world;

pub use aabb::{Aabb, Axis};
pub use registry::{AIR, Registry, StateId};
pub use world::{DimensionHeight, World};

/// `BlockPos.asLong` unpacking: 26 bits x, 12 bits y, 26 bits z.
pub fn unpack_block_pos(packed: i64) -> (i32, i32, i32) {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    (x, y, z)
}

/// `BlockPos.asLong`.
pub fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
}

/// `SectionPos.asLong` unpacking: 22 bits x, 20 bits y, 22 bits z.
pub fn unpack_section_pos(packed: i64) -> (i32, i32, i32) {
    let x = (packed >> 42) as i32;
    let y = ((packed << 44) >> 44) as i32;
    let z = ((packed << 22) >> 42) as i32;
    (x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_block(x: i32, y: i32, z: i32) -> i64 {
        ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
    }

    fn pack_section(x: i32, y: i32, z: i32) -> i64 {
        ((x as i64 & 0x3F_FFFF) << 42) | (y as i64 & 0xF_FFFF) | ((z as i64 & 0x3F_FFFF) << 20)
    }

    #[test]
    fn positions() {
        for p in [(0, 0, 0), (-1, -64, -1), (29_999_999, 319, -29_999_999), (-77, 83, -665)] {
            assert_eq!(unpack_block_pos(pack_block(p.0, p.1, p.2)), p);
        }
        for p in [(0, 0, 0), (-5, -4, 7), (1_000_000, 19, -1_000_000)] {
            assert_eq!(unpack_section_pos(pack_section(p.0, p.1, p.2)), p);
        }
    }
}
