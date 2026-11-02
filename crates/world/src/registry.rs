//! Block states and their collision/movement data, extracted from the
//! vanilla jar by tools/extract_blocks.py.

use std::io::Read;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::raycast::Direction;
use crate::shape::Shape;

/// A block state ID (`Block.BLOCK_STATE_REGISTRY`), as sent on the wire.
pub type StateId = u32;

pub const AIR: StateId = 0;

#[derive(Debug, Clone)]
pub struct BlockInfo {
    /// Registry ID in `minecraft:block`; tags refer to blocks by this.
    pub id: u32,
    pub name: String,
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
    /// `Block.getBounceRestitution` (slime blocks bounce at 1.0).
    pub bounce: f32,
    pub dynamic_shape: bool,
    /// `Block.defaultBlockState()`: not the first state for most blocks.
    pub default_state: StateId,
}

#[derive(Debug, Clone)]
pub struct StateInfo {
    pub block: u32,
    pub shape: u16,
    pub is_air: bool,
    pub has_large_collision_shape: bool,
    /// Fluid type (`minecraft:water`, ...) or empty.
    pub fluid: String,
    pub fluid_amount: u8,
    pub fluid_falling: bool,
    /// `name=value,...` as in vanilla's reports.
    pub properties: String,
    /// Bits 0-3: `isFaceSturdy` (FULL) for north, east, south, west. Bit 4:
    /// ice. Bit 5: full-cube collision shape. Bit 6: requires a correct
    /// tool. Bit 7-8: `isFaceSturdy` up/down. Bit 9: `isSolid`. Bits 10-13:
    /// `SupportType.CENTER` for north, east, south, west. Bit 14-15: center
    /// support up/down.
    pub face_flags: u16,
    /// Index of the outline shape (`getShape`), which the crosshair
    /// raycast uses; 0 is empty.
    pub outline: u16,
    /// `getDestroySpeed`: seconds-ish hardness, -1 for unbreakable.
    pub hardness: f32,
    /// Index of `getInteractionShape`, empty (0) for nearly every block.
    pub interaction: u16,
    /// `BlockState.canBeReplaced()`: the no-context flag used by
    /// `BlockBehaviour.canBeReplaced(BlockState, BlockPlaceContext)`.
    pub replaceable: bool,
}

/// An entry of the (built-in, unsynced) `minecraft:attribute` registry.
#[derive(Debug, Clone, Deserialize)]
pub struct AttributeInfo {
    pub id: u32,
    pub name: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
}

/// How the client moves an entity between the server's updates
/// (`Entity.createInterpolationHandler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    /// `InterpolationHandler.NO_OP`: it jumps to where the server says.
    Snap,
    /// `LinearInterpolationHandler` over this many ticks.
    Linear(u32),
    /// `SteppedInterpolationHandler` (living entities).
    Stepped,
}

/// Facts about an entity type that come from its Java class.
#[derive(Debug, Clone)]
pub struct EntityKind {
    /// `EntityType.updateInterval`: ticks between the server's position
    /// updates, and the length of one stepped interpolation step.
    pub update_interval: u32,
    /// Simple names of the entity class and its superclasses.
    pub classes: Vec<String>,
    pub interpolation: Interpolation,
    pub living: bool,
}

impl EntityKind {
    pub fn is(&self, class: &str) -> bool {
        self.classes.iter().any(|c| c == class)
    }

    fn new(update_interval: u32, classes: &str) -> Self {
        let classes: Vec<String> = classes
            .split(' ')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect();
        let is = |class: &str| classes.iter().any(|c| c == class);
        let interpolation = if is("Shulker") {
            Interpolation::Snap
        } else if is("LivingEntity") {
            Interpolation::Stepped
        } else if is("AbstractBoat")
            || is("AbstractMinecart")
            || is("ExperienceOrb")
            || is("FishingHook")
        {
            // Minecarts use NO_OP with the experimental movement feature.
            Interpolation::Linear(3)
        } else {
            // Display entities are Linear(0), which snaps as well.
            Interpolation::Snap
        };
        let living = is("LivingEntity");
        Self {
            update_interval,
            classes,
            interpolation,
            living,
        }
    }
}

pub struct Registry {
    pub version: String,
    pub attributes: Vec<AttributeInfo>,
    /// `minecraft:entity_type` names by registry ID.
    pub entity_types: Vec<String>,
    /// Width, height and eye height of each entity type, by registry ID.
    pub entity_dimensions: Vec<(f32, f32, f32)>,
    /// What the client needs to know about each entity type, by registry ID.
    pub entity_kinds: Vec<EntityKind>,
    /// `minecraft:mob_effect` names by registry ID.
    pub mob_effects: Vec<String>,
    /// `minecraft:command_argument_type` names by registry ID.
    pub command_argument_types: Vec<String>,
    /// `minecraft:item` names by registry ID.
    pub items: Vec<String>,
    /// `minecraft:block` registry ID placed by each item, or -1 when the
    /// item is not a `BlockItem`.
    pub item_blocks: Vec<i32>,
    /// The wall variant of a `StandingAndWallBlockItem`, or -1.
    pub item_wall_blocks: Vec<i32>,
    /// `StandingAndWallBlockItem.attachmentDirection` as a `Direction`
    /// ordinal when `item_wall_blocks` is not -1.
    pub item_wall_directions: Vec<i8>,
    pub shapes: Vec<Shape>,
    pub blocks: Vec<BlockInfo>,
    pub states: Vec<StateInfo>,
}

impl Registry {
    pub fn get() -> &'static Registry {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(Registry::load)
    }

    fn load() -> Registry {
        let gz: &[u8] = include_bytes!("../data/blocks.json.gz");
        let mut json = String::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_string(&mut json)
            .expect("embedded block data");
        let raw: RawData = serde_json::from_str(&json).expect("embedded block data");

        let shapes = raw
            .shapes
            .into_iter()
            .map(|s| match s {
                None => Shape::new(
                    [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
                    vec![false],
                    false,
                ),
                Some(s) => Shape::new(
                    [s.x, s.y, s.z],
                    s.fill.bytes().map(|b| b == b'1').collect(),
                    s.block,
                ),
            })
            .collect();
        let mut blocks: Vec<BlockInfo> = raw
            .blocks
            .into_iter()
            .map(|b| BlockInfo {
                id: b.id,
                name: b.name,
                friction: b.friction,
                speed_factor: b.speed_factor,
                jump_factor: b.jump_factor,
                bounce: b.bounce,
                dynamic_shape: b.dynamic_shape,
                default_state: b.default_state,
            })
            .collect();
        blocks.sort_by_key(|b| b.id);
        let states = raw
            .states
            .into_iter()
            .map(
                |(
                    block,
                    shape,
                    air,
                    large,
                    fluid,
                    amount,
                    falling,
                    properties,
                    face_flags,
                    outline,
                    hardness,
                    interaction,
                    replaceable,
                )| StateInfo {
                    block,
                    shape,
                    is_air: air != 0,
                    has_large_collision_shape: large != 0,
                    fluid,
                    fluid_amount: amount,
                    fluid_falling: falling != 0,
                    properties,
                    face_flags,
                    outline,
                    hardness,
                    interaction,
                    replaceable: replaceable != 0,
                },
            )
            .collect();
        let mut attributes = raw.attributes;
        attributes.sort_by_key(|a| a.id);
        Registry {
            version: raw.version,
            attributes,
            entity_types: raw.entity_types,
            entity_dimensions: raw.entity_dimensions,
            entity_kinds: raw
                .entity_info
                .iter()
                .map(|(interval, classes)| EntityKind::new(*interval, classes))
                .collect(),
            mob_effects: raw.mob_effects,
            command_argument_types: raw.command_argument_types,
            items: raw.items,
            item_blocks: raw.item_blocks,
            item_wall_blocks: raw.item_wall_blocks,
            item_wall_directions: raw.item_wall_directions,
            shapes,
            blocks,
            states,
        }
    }

    pub fn state(&self, id: StateId) -> &StateInfo {
        &self.states[id as usize]
    }

    pub fn block_of(&self, id: StateId) -> &BlockInfo {
        &self.blocks[self.states[id as usize].block as usize]
    }

    /// The collision shape with an empty collision context. Index 0 is the
    /// empty shape.
    pub fn collision_shape(&self, id: StateId) -> Option<&Shape> {
        match self.states[id as usize].shape {
            0 => None,
            i => Some(&self.shapes[i as usize]),
        }
    }

    /// The outline shape (`BlockState.getShape`): what the crosshair hits.
    pub fn outline_shape(&self, id: StateId) -> Option<&Shape> {
        match self.state(id).outline {
            0 => None,
            index => Some(&self.shapes[index as usize]),
        }
    }

    /// `BlockState.getInteractionShape`.
    pub fn interaction_shape(&self, id: StateId) -> Option<&Shape> {
        match self.state(id).interaction {
            0 => None,
            index => Some(&self.shapes[index as usize]),
        }
    }

    /// `requiresCorrectToolForDrops`.
    pub fn requires_tool(&self, id: StateId) -> bool {
        self.state(id).face_flags & 64 != 0
    }

    pub fn block_by_name(&self, name: &str) -> Option<&BlockInfo> {
        self.blocks.iter().find(|b| b.name == name)
    }

    /// The block an item places (`BlockItem.getBlock`), or `None` for items
    /// that do not place a block. Item and block names are not always equal
    /// (`minecraft:redstone` places `minecraft:redstone_wire`).
    pub fn block_for_item(&self, item: u32) -> Option<&BlockInfo> {
        let block = *self.item_blocks.get(item as usize)?;
        if block < 0 {
            return None;
        }
        self.blocks.get(block as usize)
    }

    /// The wall variant placed by a `StandingAndWallBlockItem` (torches,
    /// signs, banners, skulls, coral fans, hanging signs).
    pub fn wall_block_for_item(&self, item: u32) -> Option<&BlockInfo> {
        let block = *self.item_wall_blocks.get(item as usize)?;
        if block < 0 {
            return None;
        }
        self.blocks.get(block as usize)
    }

    /// `StandingAndWallBlockItem.attachmentDirection`: the direction in
    /// which the standing variant is attached (`DOWN` for torches and
    /// signs, `UP` for hanging signs).
    pub fn wall_attachment_direction(&self, item: u32) -> Option<Direction> {
        self.item_wall_directions
            .get(item as usize)
            .and_then(|d| Direction::from_ordinal(*d as i32))
    }

    /// `BlockState.setValue`, but tolerant like
    /// `BlockItemStateProperties.apply`: unknown properties and invalid
    /// values leave the state unchanged.
    ///
    /// Properties are applied in order, so later values win for duplicate
    /// keys.
    pub fn state_with_properties(&self, state: StateId, properties: &[(&str, &str)]) -> StateId {
        properties.iter().fold(state, |state, (key, value)| {
            self.state_with_property(state, key, value)
        })
    }

    fn state_with_property(&self, state: StateId, key: &str, value: &str) -> StateId {
        let Some(current) = self.property(state, key) else {
            return state;
        };
        if current == value {
            return state;
        }
        let block = self.state(state).block;
        let start = self.states.partition_point(|s| s.block < block);
        let end = self.states.partition_point(|s| s.block <= block);
        let base = self.states[state as usize].properties.as_str();
        self.states[start..end]
            .iter()
            .position(|candidate| same_properties_except(&candidate.properties, base, key, value))
            .map_or(state, |i| (start + i) as StateId)
    }

    /// Attribute name for a registry ID from `update_attributes`.
    pub fn attribute(&self, id: u32) -> Option<&AttributeInfo> {
        self.attributes.get(id as usize).filter(|a| a.id == id)
    }

    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// A property value of a state, e.g. `property(id, "bottom")`.
    pub fn property<'a>(&'a self, id: StateId, key: &str) -> Option<&'a str> {
        self.states[id as usize]
            .properties
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

fn property_in<'a>(properties: &'a str, key: &str) -> Option<&'a str> {
    properties
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Whether `candidate` changes `key` to `value` and keeps every other
/// property of `base`.
fn same_properties_except(candidate: &str, base: &str, key: &str, value: &str) -> bool {
    property_in(candidate, key) == Some(value)
        && base
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .all(|(k, v)| k == key || property_in(candidate, k) == Some(v))
}

type RawState = (
    u32,
    u16,
    u8,
    u8,
    String,
    u8,
    u8,
    String,
    u16,
    u16,
    f32,
    u16,
    u8,
);

#[derive(Deserialize)]
struct RawData {
    version: String,
    attributes: Vec<AttributeInfo>,
    entity_types: Vec<String>,
    mob_effects: Vec<String>,
    command_argument_types: Vec<String>,
    items: Vec<String>,
    item_blocks: Vec<i32>,
    item_wall_blocks: Vec<i32>,
    item_wall_directions: Vec<i8>,
    shapes: Vec<Option<RawShape>>,
    blocks: Vec<RawBlock>,
    entity_dimensions: Vec<(f32, f32, f32)>,
    entity_info: Vec<(u32, String)>,
    states: Vec<RawState>,
}

#[derive(Deserialize)]
struct RawShape {
    block: bool,
    x: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    fill: String,
}

#[derive(Deserialize)]
struct RawBlock {
    id: u32,
    name: String,
    friction: f32,
    speed_factor: f32,
    jump_factor: f32,
    bounce: f32,
    dynamic_shape: bool,
    default_state: StateId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads() {
        let r = Registry::get();
        assert_eq!(r.version, "26.3");
        assert!(r.state(AIR).is_air);
        let stone = r.block_by_name("minecraft:stone").unwrap();
        assert_eq!(stone.friction, 0.6);
        let ice = r.block_by_name("minecraft:ice").unwrap();
        assert_eq!(ice.friction, 0.98);
        assert!(r.collision_shape(AIR).is_none());
        assert!(
            r.collision_shape(1).unwrap().is_block,
            "stone is a full cube"
        );
        assert_eq!(r.item_blocks.len(), r.items.len());
    }

    #[test]
    fn default_states_are_not_always_the_first_state() {
        let r = Registry::get();
        let log = r.block_by_name("minecraft:oak_log").unwrap();
        assert_eq!(r.property(log.default_state, "axis"), Some("y"));
        let first = (0..r.state_count() as StateId)
            .find(|id| r.state(*id).block == log.id)
            .unwrap();
        assert_ne!(first, log.default_state);
    }

    #[test]
    fn item_block_mapping_is_not_name_based() {
        let r = Registry::get();
        let redstone = r
            .items
            .iter()
            .position(|name| name == "minecraft:redstone")
            .unwrap() as u32;
        assert_eq!(
            r.block_for_item(redstone).unwrap().name,
            "minecraft:redstone_wire"
        );
        let stick = r
            .items
            .iter()
            .position(|name| name == "minecraft:stick")
            .unwrap() as u32;
        assert!(r.block_for_item(stick).is_none());
    }

    #[test]
    fn state_properties_apply_tolerantly() {
        let r = Registry::get();
        let slab = r.block_by_name("minecraft:oak_slab").unwrap();
        let top = r.state_with_properties(
            slab.default_state,
            &[
                ("type", "top"),
                ("missing", "value"),
                ("waterlogged", "nope"),
            ],
        );
        assert_eq!(r.property(top, "type"), Some("top"));
        assert_eq!(r.property(top, "waterlogged"), Some("false"));
        assert_eq!(r.state(top).block, slab.id);
    }
}
