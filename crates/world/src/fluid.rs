//! Fluids: `FluidState` heights and `FlowingFluid.getFlow`.

use crate::aabb::Aabb;
use crate::registry::Registry;
use crate::tags::Tags;
use crate::world::World;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidKind {
    Water,
    Lava,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fluid {
    pub kind: FluidKind,
    /// 1-8 for flowing levels; a source reports 8. `getOwnHeight` is
    /// `amount / 9`.
    pub amount: u8,
    pub falling: bool,
    pub source: bool,
}

impl Fluid {
    /// `FluidState.getOwnHeight`.
    pub fn own_height(&self) -> f32 {
        self.amount as f32 / 9.0
    }
}

/// North, east, south, west: `Direction.Plane.HORIZONTAL` order, with the
/// bit each has in `StateInfo::face_flags`.
const HORIZONTAL: [(i32, i32, u8); 4] = [(0, -1, 1), (1, 0, 2), (0, 1, 4), (-1, 0, 8)];

/// The fluid at a block (`BlockState.getFluidState`), `None` if empty or
/// the chunk is not loaded.
pub fn fluid_at(world: &World, x: i32, y: i32, z: i32) -> Option<Fluid> {
    let info = Registry::get().state(world.block_state(x, y, z)?);
    let (kind, source) = match info.fluid.as_str() {
        "" => return None,
        "minecraft:water" => (FluidKind::Water, true),
        "minecraft:flowing_water" => (FluidKind::Water, false),
        "minecraft:lava" => (FluidKind::Lava, true),
        "minecraft:flowing_lava" => (FluidKind::Lava, false),
        _ => return None,
    };
    Some(Fluid { kind, amount: info.fluid_amount, falling: info.fluid_falling, source })
}

/// `FlowingFluid.getHeight`: full if the same fluid is above.
pub fn height(world: &World, fluid: Fluid, x: i32, y: i32, z: i32) -> f32 {
    if fluid_at(world, x, y + 1, z).is_some_and(|above| above.kind == fluid.kind) { 1.0 } else { fluid.own_height() }
}

/// `FluidState.getHeightForCamera`: a source under a sturdy block fills
/// its cell. (Approximated with "full-cube collision" for the block above;
/// vanilla asks whether its bottom face is sturdy.)
pub fn height_for_camera(world: &World, fluid: Fluid, x: i32, y: i32, z: i32) -> f32 {
    if fluid.source {
        let above = Registry::get().state(world.block_state_or_air(x, y + 1, z));
        if above.face_flags & 32 != 0 {
            return 1.0;
        }
    }
    height(world, fluid, x, y, z)
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1.0e-5f32 as f64 { [0.0; 3] } else { [v[0] / len, v[1] / len, v[2] / len] }
}

/// `FlowingFluid.isSolidFace`.
fn is_solid_face(world: &World, kind: FluidKind, x: i32, y: i32, z: i32, face_bit: u8) -> bool {
    if fluid_at(world, x, y, z).is_some_and(|f| f.kind == kind) {
        return false;
    }
    let info = Registry::get().state(world.block_state_or_air(x, y, z));
    if info.face_flags & 16 != 0 {
        return false;
    }
    info.face_flags & face_bit != 0
}

/// `FlowingFluid.getFlow`: the unit direction the fluid pushes in.
pub fn flow(world: &World, tags: &Tags, fluid: Fluid, x: i32, y: i32, z: i32) -> [f64; 3] {
    let registry = Registry::get();
    // affectsFlow: empty or the same fluid.
    let affects = |f: Option<Fluid>| f.is_none_or(|f| f.kind == fluid.kind);
    let (mut flow_x, mut flow_z) = (0.0f64, 0.0f64);

    for (dx, dz, _) in HORIZONTAL {
        let (nx, nz) = (x + dx, z + dz);
        let neighbour = fluid_at(world, nx, y, nz);
        if !affects(neighbour) {
            continue;
        }
        let mut neighbour_height = neighbour.map_or(0.0, |f| f.own_height());
        let mut distance = 0.0f32;
        if neighbour_height == 0.0 {
            let block = registry.block_of(world.block_state_or_air(nx, y, nz));
            if !tags.block_is("minecraft:blocks_fluid_flow", block.id) {
                let below = fluid_at(world, nx, y - 1, nz);
                if affects(below) {
                    neighbour_height = below.map_or(0.0, |f| f.own_height());
                    if neighbour_height > 0.0 {
                        distance = fluid.own_height() - (neighbour_height - 0.8888889);
                    }
                }
            }
        } else if neighbour_height > 0.0 {
            distance = fluid.own_height() - neighbour_height;
        }
        if distance != 0.0 {
            flow_x += (dx as f32 * distance) as f64;
            flow_z += (dz as f32 * distance) as f64;
        }
    }

    let mut result = [flow_x, 0.0, flow_z];
    if fluid.falling {
        for (dx, dz, bit) in HORIZONTAL {
            let (nx, nz) = (x + dx, z + dz);
            if is_solid_face(world, fluid.kind, nx, y, nz, bit) || is_solid_face(world, fluid.kind, nx, y + 1, nz, bit) {
                let n = normalize(result);
                result = [n[0], n[1] - 6.0, n[2]];
                break;
            }
        }
    }
    normalize(result)
}

/// `LevelReader.containsAnyLiquid`.
pub fn contains_any_liquid(world: &World, bbox: &Aabb) -> bool {
    let floor = |v: f64| v.floor() as i32;
    for x in floor(bbox.min_x)..=floor(bbox.max_x) {
        for y in floor(bbox.min_y)..=floor(bbox.max_y) {
            for z in floor(bbox.min_z)..=floor(bbox.max_z) {
                if fluid_at(world, x, y, z).is_some() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::world::DimensionHeight;

    fn water(level: u8) -> u32 {
        let r = Registry::get();
        let block = r.block_by_name("minecraft:water").unwrap();
        r.states
            .iter()
            .position(|s| s.block == block.id && s.properties == format!("level={level}"))
            .unwrap() as u32
    }

    #[test]
    fn heights_and_flow_down_a_slope() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        for x in 0..16 {
            for z in 0..16 {
                world.set_block_state(x, 63, z, 1);
            }
        }
        // A source at x=4 flowing east: levels 0 (source), 1, 2, 3.
        for (i, x) in (4..8).enumerate() {
            world.set_block_state(x, 64, 8, water(i as u8));
        }
        let source = fluid_at(&world, 4, 64, 8).unwrap();
        assert!(source.source);
        assert_eq!(source.amount, 8);
        assert!((source.own_height() - 8.0 / 9.0).abs() < 1e-6);
        let mid = fluid_at(&world, 5, 64, 8).unwrap();
        assert_eq!(mid.amount, 7);
        assert!(!mid.source);

        // Water above makes the cell full.
        world.set_block_state(4, 65, 8, water(0));
        assert_eq!(height(&world, source, 4, 64, 8), 1.0);

        // Flow at the middle of the slope points east (+x).
        let f = flow(&world, &Tags::default(), mid, 5, 64, 8);
        assert!(f[0] > 0.9 && f[2].abs() < 0.5, "{f:?}");
        assert!(contains_any_liquid(&world, &Aabb::new(5.2, 64.0, 8.2, 5.8, 65.8, 8.8)));
        assert!(!contains_any_liquid(&world, &Aabb::new(10.2, 64.0, 8.2, 10.8, 65.8, 8.8)));
    }
}
