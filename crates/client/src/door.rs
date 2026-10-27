//! Doors: the two-block state, and what it takes to walk through one.
//!
//! A closed door is a thin panel, so the ordinary collision check at a
//! block's centre sees no obstacle. Navigation therefore treats a closed
//! door as passable *after* a right click, and open doors as ordinary
//! surfaces. Iron doors cannot be opened by hand, and powered doors would
//! only close again, so neither is a route.

use rapidbot_physics::math::Vec3;
use rapidbot_world::{Axis, Registry, World};

pub type Pos = (i32, i32, i32);

/// The two halves of a door, as one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Door {
    pub lower: Pos,
    pub upper: Pos,
    pub open: bool,
    pub powered: bool,
    /// Whether a player can open this door by hand (everything but iron).
    pub hand_openable: bool,
}

impl Door {
    /// True if this is the lower half: the half a plan refers to.
    pub fn is_lower(&self, pos: Pos) -> bool {
        self.lower == pos
    }

    /// True if the bot can open this door now. Iron doors cannot be opened
    /// by hand, and a powered door is held closed by redstone.
    pub fn can_open(&self) -> bool {
        self.hand_openable && !self.open && !self.powered
    }

    /// The centre of the closed panel: what to put the crosshair on.
    pub fn aim_point(&self, world: &World) -> Option<Vec3> {
        let state = world.block_state(self.lower.0, self.lower.1, self.lower.2)?;
        let shape = Registry::get().outline_shape(state)?;
        let bounds = shape.at(self.lower.0, self.lower.1, self.lower.2);
        Some(Vec3::new(
            (bounds.min(Axis::X) + bounds.max(Axis::X)) * 0.5,
            (bounds.min(Axis::Y) + bounds.max(Axis::Y)) * 0.5,
            (bounds.min(Axis::Z) + bounds.max(Axis::Z)) * 0.5,
        ))
    }

    /// The feet height after the door is open. Like an empty cell, a door
    /// needs a full block beneath it; doors on slabs are left to a later
    /// refinement.
    pub fn standing_height(&self, world: &World) -> Option<f64> {
        let below = world.block_state(self.lower.0, self.lower.1 - 1, self.lower.2)?;
        let top = Registry::get()
            .collision_shape(below)?
            .at(0, 0, 0)
            .max(Axis::Y);
        (top >= 1.0).then_some(self.lower.1 as f64 + (top - 1.0))
    }
}

fn flag(registry: &Registry, state: u32, key: &str) -> bool {
    matches!(registry.property(state, key), Some("true"))
}

/// The door containing `(x, y, z)`, whether that is its lower or upper half.
pub fn at(world: &World, x: i32, y: i32, z: i32) -> Option<Door> {
    let registry = Registry::get();
    let state = world.block_state(x, y, z)?;
    // Trapdoors also have `half`; only doors have a hinge.
    registry.property(state, "hinge")?;
    let half = registry.property(state, "half")?;
    if half != "lower" && half != "upper" {
        return None;
    }

    let (lower, upper) = if half == "lower" {
        ((x, y, z), (x, y.checked_add(1)?, z))
    } else {
        ((x, y.checked_sub(1)?, z), (x, y, z))
    };
    let lower_state = world.block_state(lower.0, lower.1, lower.2)?;
    let upper_state = world.block_state(upper.0, upper.1, upper.2)?;
    let lower_info = registry.state(lower_state);
    let upper_info = registry.state(upper_state);
    if lower_info.block != upper_info.block
        || registry.property(lower_state, "half") != Some("lower")
        || registry.property(upper_state, "half") != Some("upper")
    {
        return None;
    }
    let name = registry.blocks[lower_info.block as usize].name.as_str();
    if !name.ends_with("_door") {
        return None;
    }

    Some(Door {
        lower,
        upper,
        open: flag(registry, lower_state, "open"),
        powered: flag(registry, lower_state, "powered"),
        hand_openable: name != "minecraft:iron_door",
    })
}

#[cfg(test)]
mod tests {
    use rapidbot_world::DimensionHeight;
    use rapidbot_world::chunk::Chunk;

    use super::*;

    fn state(name: &str, properties: &str) -> u32 {
        let registry = Registry::get();
        let block = registry.block_by_name(name).unwrap();
        registry
            .states
            .iter()
            .position(|s| s.block == block.id && s.properties.contains(properties))
            .unwrap() as u32
    }

    fn world_with_door(name: &str, open: bool, powered: bool) -> World {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        for cx in -1..=1 {
            for cz in -1..=1 {
                world.insert_chunk(cx, cz, Chunk::empty(24));
            }
        }
        world.set_block_state(0, 63, 0, 1);
        world.set_block_state(0, 64, 0, 1);
        let properties = |half: &str| {
            format!("facing=north,half={half},hinge=left,open={open},powered={powered}")
        };
        world.set_block_state(0, 64, 0, state(name, &properties("lower")));
        world.set_block_state(0, 65, 0, state(name, &properties("upper")));
        world
    }

    #[test]
    fn finds_both_halves_and_the_panel() {
        let world = world_with_door("minecraft:oak_door", false, false);
        let lower = at(&world, 0, 64, 0).unwrap();
        let upper = at(&world, 0, 65, 0).unwrap();
        assert_eq!(lower, upper);
        assert_eq!(lower.lower, (0, 64, 0));
        assert_eq!(lower.upper, (0, 65, 0));
        assert!(lower.hand_openable && lower.can_open());
        assert_eq!(lower.standing_height(&world), Some(64.0));
        let aim = lower.aim_point(&world).unwrap();
        assert!((aim.x - 0.5).abs() < 1.0e-9);
        assert!(aim.z > 0.8 && aim.z < 1.0, "aim at the panel: {aim:?}");
    }

    #[test]
    fn iron_and_powered_doors_do_not_open_by_hand() {
        let iron = at(
            &world_with_door("minecraft:iron_door", false, false),
            0,
            64,
            0,
        )
        .unwrap();
        assert!(!iron.hand_openable && !iron.can_open());

        let powered = at(
            &world_with_door("minecraft:oak_door", false, true),
            0,
            64,
            0,
        )
        .unwrap();
        assert!(powered.hand_openable && !powered.can_open());

        let open = at(&world_with_door("minecraft:oak_door", true, true), 0, 64, 0).unwrap();
        assert!(open.open && !open.can_open());
    }

    #[test]
    fn plain_blocks_are_not_doors() {
        let world = world_with_door("minecraft:oak_door", false, false);
        assert!(at(&world, 0, 63, 0).is_none());
        assert!(at(&world, 1, 64, 0).is_none());
    }
}
