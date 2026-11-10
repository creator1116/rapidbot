//! Block states and their collision/movement data, extracted from the
//! vanilla jar by tools/extract_blocks.py.

use std::collections::HashMap;
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
    /// Index into [`Registry::feature_sets`]: the feature flags the server
    /// must have enabled for this block to be usable.
    pub features: u16,
    /// `BlockBehaviour.OffsetType`: 0 none, 1 XZ, 2 XYZ.
    pub offset_type: u8,
    /// Whether the block's shapes are exactly `base.move(state.getOffset(pos))`.
    /// Grass and ferns have an offset type that only moves how they are
    /// *drawn*; flowers, bamboo and dripstone move their crosshair shapes with
    /// it, and bamboo, dripstone and sulfur spikes move their collision boxes.
    pub offset_shape: bool,
    /// `getMaxHorizontalOffset`: 0.25 unless the block overrides it.
    pub max_h_offset: f32,
    /// `getMaxVerticalOffset`: 0.2 unless the block overrides it (small
    /// dripleaf is 0.1, but its shape does not follow the offset).
    pub max_v_offset: f32,
    /// `Block.defaultBlockState()`: not the first state for most blocks.
    pub default_state: StateId,
    /// The block declares its own `useItemOn`/`useWithoutItem`, so a
    /// right-click on it can consume the action before the held item is
    /// used (`Minecraft.startUseItem`).
    pub interactive: bool,
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
    /// The distinct `Block.requiredFeatures` sets, indexed by
    /// [`BlockInfo::features`].
    pub feature_sets: Vec<Vec<String>>,
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
        // Feature-flag sets are interned: nearly every block requires exactly
        // `minecraft:vanilla`, so there is one set for almost all of them.
        let mut feature_sets: Vec<Vec<String>> = vec![Vec::new()];
        let mut feature_index: HashMap<Vec<String>, u16> = HashMap::new();
        feature_index.insert(Vec::new(), 0);
        let mut blocks: Vec<BlockInfo> = raw
            .blocks
            .into_iter()
            .map(|b| {
                let features = match feature_index.get(&b.features) {
                    Some(index) => *index,
                    None => {
                        let index = feature_sets.len() as u16;
                        feature_sets.push(b.features.clone());
                        feature_index.insert(b.features, index);
                        index
                    }
                };
                BlockInfo {
                    id: b.id,
                    name: b.name,
                    friction: b.friction,
                    speed_factor: b.speed_factor,
                    jump_factor: b.jump_factor,
                    bounce: b.bounce,
                    dynamic_shape: b.dynamic_shape,
                    features,
                    offset_type: b.offset_type,
                    offset_shape: b.offset_shape,
                    max_h_offset: b.max_h_offset,
                    max_v_offset: b.max_v_offset,
                    default_state: b.default_state,
                    interactive: b.interactive,
                }
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
            feature_sets,
            blocks,
            states,
        }
    }

    pub fn state(&self, id: StateId) -> &StateInfo {
        &self.states[id as usize]
    }

    /// `Block.requiredFeatures`: the feature flags the server has to have
    /// enabled for this block to be usable at all. Nearly everything is just
    /// `minecraft:vanilla`.
    pub fn required_features(&self, id: StateId) -> &[String] {
        let block = self.state(id).block;
        self.blocks
            .get(block as usize)
            .and_then(|info| self.feature_sets.get(info.features as usize))
            .map_or(&[], |set| set.as_slice())
    }

    pub fn block_of(&self, id: StateId) -> &BlockInfo {
        &self.blocks[self.states[id as usize].block as usize]
    }

    /// `BlockState.getOffset(pos)`: where inside its cell the block sits. The
    /// hash reads the height as zero, so `y` is only here to mirror vanilla's
    /// signature.
    pub fn block_offset(&self, id: StateId, x: i32, _y: i32, z: i32) -> [f64; 3] {
        offset_at(self.block_of(id), x, z)
    }

    /// What has to be added to this block's extracted shape, which was taken
    /// at `BlockPos.ZERO`, to get the shape it really has at `(x, y, z)`.
    /// Zero for everything whose shape does not follow its offset.
    ///
    /// The subtraction re-associates vanilla's `base + offset(pos)` into
    /// `(base + offset(0,0)) + (offset(pos) - offset(0,0))`, which can move a
    /// coordinate by one bit at the end; the ray cast and collision epsilons
    /// are far wider than that.
    pub fn shape_offset(&self, id: StateId, x: i32, _y: i32, z: i32) -> [f64; 3] {
        let block = self.block_of(id);
        if !block.offset_shape || block.offset_type == 0 {
            return [0.0; 3];
        }
        let here = offset_at(block, x, z);
        let origin = offset_at(block, 0, 0);
        [
            here[0] - origin[0],
            here[1] - origin[1],
            here[2] - origin[2],
        ]
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

/// `Mth.getSeed(x, y, z)`: the hash a position-based block offset uses.
pub fn get_seed(x: i32, y: i32, z: i32) -> i64 {
    // Java: `x * 3129871 ^ z * 116129781L ^ y`, the first product in int.
    let mut seed =
        (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42317861)
        .wrapping_add(seed.wrapping_mul(11));
    seed >> 16
}

/// `BlockBehaviour.Properties.offsetType`'s function: where inside its cell
/// the block sits. The horizontal step divides in float and is widened
/// afterwards, exactly as vanilla writes it.
fn offset_at(block: &BlockInfo, x: i32, z: i32) -> [f64; 3] {
    if block.offset_type == 0 {
        return [0.0; 3];
    }
    let seed = get_seed(x, 0, z);
    let max_h = block.max_h_offset as f64;
    let step = |bits: i64| ((bits & 15) as f32 / 15.0f32) as f64 - 0.5;
    let clamp = |value: f64| value.clamp(-max_h, max_h);
    let y_offset = if block.offset_type == 2 {
        let step_y = ((seed >> 4 & 15) as f32 / 15.0f32) as f64 - 1.0;
        step_y * block.max_v_offset as f64
    } else {
        0.0
    };
    [
        clamp(step(seed) * 0.5),
        y_offset,
        clamp(step(seed >> 8) * 0.5),
    ]
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
    /// `Block.requiredFeatures`: `minecraft:vanilla` for nearly everything.
    features: Vec<String>,
    offset_type: u8,
    offset_shape: bool,
    max_h_offset: f32,
    max_v_offset: f32,
    default_state: StateId,
    interactive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Mth.getSeed` and the offset it drives, against what the JVM returns
    /// for these blocks and positions (`BlockState.getOffset`, printed by a
    /// throwaway probe run through `tools/extract_blocks.py`'s classpath).
    #[test]
    fn block_offsets_match_the_jvm() {
        let r = Registry::get();
        let cases: [(&str, i32, i32, [f64; 3]); 7] = [
            (
                "minecraft:short_grass",
                10,
                20,
                [0.116_666_674_613_952_64, -0.200_000_002_980_232_24, -0.25],
            ),
            (
                "minecraft:short_grass",
                0,
                0,
                [-0.25, -0.200_000_002_980_232_24, -0.25],
            ),
            (
                "minecraft:short_grass",
                -12,
                5,
                [
                    -0.216_666_664_928_197_86,
                    -0.093_333_329_161_008_12,
                    -0.216_666_664_928_197_86,
                ],
            ),
            (
                "minecraft:tall_dry_grass",
                7,
                -3,
                [
                    -0.083_333_328_366_279_6,
                    -0.200_000_002_980_232_24,
                    -0.183_333_329_856_395_72,
                ],
            ),
            (
                "minecraft:dandelion",
                3,
                -7,
                [-0.149_999_998_509_883_88, 0.0, -0.183_333_329_856_395_72],
            ),
            (
                "minecraft:small_dripleaf",
                1,
                1,
                [
                    -0.083_333_328_366_279_6,
                    -0.026_666_665_474_573_747,
                    -0.083_333_328_366_279_6,
                ],
            ),
            (
                "minecraft:fern",
                20,
                20,
                [
                    -0.049_999_997_019_767_76,
                    -0.053_333_330_949_147_495,
                    -0.216_666_664_928_197_86,
                ],
            ),
        ];
        for (name, x, z, expected) in cases {
            let state = r.block_by_name(name).unwrap().default_state;
            let got = r.block_offset(state, x, 64, z);
            for axis in 0..3 {
                assert!(
                    (got[axis] - expected[axis]).abs() < 1.0e-12,
                    "{name} at ({x}, 64, {z}): {got:?} != {expected:?}"
                );
            }
        }
    }

    /// Only the blocks whose shapes move with the offset get a shift; grass is
    /// drawn shifted but hit where it stands.
    #[test]
    fn only_shifting_shapes_get_an_offset() {
        let r = Registry::get();
        let dandelion = r
            .block_by_name("minecraft:dandelion")
            .unwrap()
            .default_state;
        let shifted = r.shape_offset(dandelion, 10, 64, 20);
        assert!(
            shifted.iter().any(|v| v.abs() > 1.0e-6),
            "a flower's shape follows its offset: {shifted:?}"
        );
        let grass = r
            .block_by_name("minecraft:short_grass")
            .unwrap()
            .default_state;
        assert_eq!(r.shape_offset(grass, 10, 64, 20), [0.0; 3]);
        let stone = r.block_by_name("minecraft:stone").unwrap().default_state;
        assert_eq!(r.shape_offset(stone, 10, 64, 20), [0.0; 3]);
        assert_eq!(r.block_offset(stone, 10, 64, 20), [0.0; 3]);
    }

    /// `Block.requiredFeatures`: in 26.3 every block asks for
    /// `minecraft:vanilla` and nothing else, which is the set the client's
    /// use gate compares against the server's announced flags.
    #[test]
    fn blocks_require_the_vanilla_flag() {
        let r = Registry::get();
        let vanilla = vec!["minecraft:vanilla".to_string()];
        for name in ["minecraft:stone", "minecraft:air", "minecraft:crafter"] {
            let state = r.block_by_name(name).unwrap().default_state;
            assert_eq!(r.required_features(state), vanilla, "{name}");
        }
    }

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

    /// `hasCustomUse`: blocks that declare `useItemOn`/`useWithoutItem`
    /// consume a right-click before the held item is used.
    #[test]
    fn interactive_blocks_are_flagged() {
        let r = Registry::get();
        assert!(
            r.block_by_name("minecraft:crafting_table")
                .unwrap()
                .interactive
        );
        assert!(r.block_by_name("minecraft:oak_door").unwrap().interactive);
        assert!(r.block_by_name("minecraft:chest").unwrap().interactive);
        assert!(!r.block_by_name("minecraft:stone").unwrap().interactive);
        assert!(!r.block_by_name("minecraft:oak_log").unwrap().interactive);
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
