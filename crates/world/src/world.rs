//! The client's view of one dimension.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rapidbot_nbt::Compound;

use crate::chunk::{BlockEntity, Chunk};
use crate::registry::{AIR, Registry, StateId};

/// Height range of a dimension (`DimensionType.minY` / `height`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimensionHeight {
    pub min_y: i32,
    pub height: i32,
}

impl DimensionHeight {
    pub const OVERWORLD: Self = Self {
        min_y: -64,
        height: 384,
    };
    pub const NETHER: Self = Self {
        min_y: 0,
        height: 256,
    };
    pub const END: Self = Self {
        min_y: 0,
        height: 256,
    };

    /// Vanilla dimension types by name, for registries sent without data
    /// because the client has them from the core pack.
    pub fn vanilla(dimension_type: &str) -> Option<Self> {
        match dimension_type {
            "minecraft:overworld" | "minecraft:overworld_caves" => Some(Self::OVERWORLD),
            "minecraft:the_nether" => Some(Self::NETHER),
            "minecraft:the_end" => Some(Self::END),
            _ => None,
        }
    }

    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }
}

pub struct World {
    pub height: DimensionHeight,
    /// Loaded chunks behind a copy-on-write pointer, so a consumer can keep
    /// one without holding a lock and without stalling the tick thread.
    chunks: HashMap<(i32, i32), ChunkEntry>,
    /// The newest revision any chunk reached: `observation_id`.
    revision: u64,
    /// This world's identity, unique among the worlds of one process.
    generation: u64,
    /// The level's rain level, set by the `rain_level` game event. Vanilla
    /// also lerps it for rendering, which a headless client does not do.
    pub rain_level: f32,
    /// `Level.canHaveWeather`: the dimension type has skylight, no ceiling,
    /// and the dimension is not the End. Without weather `isRainingAt` is
    /// always false, whatever the rain level says.
    pub can_have_weather: bool,
    /// The level's sea level, from the spawn packet; it anchors the snow
    /// line in `Biome.coldEnoughToSnow`.
    pub sea_level: i32,
    /// The biome registry's precipitation fields, by biome id.
    pub biomes: Vec<BiomeWeather>,
}

/// The precipitation fields of one biome, as `Biome.climateSettings` and the
/// frozen temperature modifier read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiomeWeather {
    pub has_precipitation: bool,
    pub temperature: f32,
    pub frozen: bool,
}

impl Default for BiomeWeather {
    /// The conservative read of an unknown biome: no precipitation, so the
    /// rain check fails and nothing rides on the missing data.
    fn default() -> Self {
        Self {
            has_precipitation: false,
            temperature: 0.5,
            frozen: false,
        }
    }
}

/// One loaded chunk and the revision of its contents.
struct ChunkEntry {
    chunk: Arc<Chunk>,
    revision: u64,
}

impl World {
    pub fn new(height: DimensionHeight) -> Self {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        Self {
            height,
            chunks: HashMap::new(),
            revision: 0,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            rain_level: 0.0,
            can_have_weather: true,
            sea_level: 63,
            biomes: Vec::new(),
        }
    }

    /// An identity for this world that never repeats within the process.
    /// Every `World` takes a fresh one, including the one a respawn or a
    /// reconfiguration builds, so a consumer's cache keyed by it cannot mix
    /// two levels together.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A revision that moves whenever something a snapshot consumer can see
    /// changes: a chunk loaded or unloaded, a block update, or a block
    /// entity. Reading it is cheap, so a consumer can poll it to decide
    /// whether to take fresh snapshots.
    pub fn observation_id(&self) -> u64 {
        self.revision
    }

    /// An immutable snapshot of a loaded chunk, with the revision it is at.
    /// `None` means the chunk is not loaded, which stays distinguishable from
    /// a chunk of air.
    ///
    /// Holding the `Arc` needs no lock and does not stall the bot: the next
    /// update to that chunk copies it out from under the snapshot (copy on
    /// write), so the snapshot keeps the contents it was taken with.
    pub fn chunk_snapshot(&self, x: i32, z: i32) -> Option<(u64, Arc<Chunk>)> {
        let entry = self.chunks.get(&(x, z))?;
        Some((entry.revision, Arc::clone(&entry.chunk)))
    }

    /// Snapshots of every loaded chunk in the inclusive chunk range
    /// `from..=to`, without looking at or copying the rest of the world.
    pub fn chunk_snapshots_in(
        &self,
        from: (i32, i32),
        to: (i32, i32),
    ) -> Vec<((i32, i32), u64, Arc<Chunk>)> {
        let mut out = Vec::new();
        for x in from.0..=to.0 {
            for z in from.1..=to.1 {
                if let Some((revision, chunk)) = self.chunk_snapshot(x, z) {
                    out.push(((x, z), revision, chunk));
                }
            }
        }
        out
    }

    /// Loads a chunk, or replaces the one already there. Either way the
    /// coordinates get a new revision, so a snapshot of the old chunk is
    /// recognisably out of date.
    pub fn insert_chunk(&mut self, x: i32, z: i32, chunk: Chunk) {
        self.revision += 1;
        let revision = self.revision;
        self.chunks.insert(
            (x, z),
            ChunkEntry {
                chunk: Arc::new(chunk),
                revision,
            },
        );
    }

    /// Unloads a chunk: the entry is forgotten, and a later load of the same
    /// coordinates is a different chunk with a newer revision. A snapshot
    /// taken before the unload stays readable.
    pub fn remove_chunk(&mut self, x: i32, z: i32) {
        if self.chunks.remove(&(x, z)).is_some() {
            self.revision += 1;
        }
    }

    pub fn has_chunk(&self, x: i32, z: i32) -> bool {
        self.chunks.contains_key(&(x, z))
    }

    pub fn has_chunk_at(&self, bx: i32, bz: i32) -> bool {
        self.has_chunk(bx >> 4, bz >> 4)
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The block entity at this position, if the chunk is loaded and the
    /// server sent one.
    pub fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<&BlockEntity> {
        self.chunks
            .get(&(x >> 4, z >> 4))?
            .chunk
            .block_entities
            .get(&((x & 15) as u8, y, (z & 15) as u8))
    }

    /// `ClientPacketListener.handleBlockEntityData`: a matching type replaces
    /// the tag; a different type is ignored. If the block entity appeared
    /// after its chunk was sent, the packet creates the record.
    pub fn apply_block_entity_data(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        type_id: i32,
        tag: Compound,
    ) -> bool {
        let Some(entry) = self.chunks.get_mut(&(x >> 4, z >> 4)) else {
            return false;
        };
        let key = ((x & 15) as u8, y, (z & 15) as u8);
        let changed = {
            let chunk = Arc::make_mut(&mut entry.chunk);
            match chunk.block_entities.get_mut(&key) {
                Some(entity) if entity.type_id != type_id => false,
                Some(entity) => {
                    entity.tag = Some(tag);
                    true
                }
                None => {
                    chunk.block_entities.insert(
                        key,
                        BlockEntity {
                            type_id,
                            tag: Some(tag),
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.revision += 1;
            entry.revision = self.revision;
        }
        changed
    }

    /// `None` when the chunk is not loaded; air outside the build height.
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        let chunk = &self.chunks.get(&(x >> 4, z >> 4))?.chunk;
        if y < self.height.min_y || y > self.height.max_y() {
            return Some(AIR);
        }
        let section = ((y - self.height.min_y) >> 4) as usize;
        Some(match chunk.sections.get(section) {
            Some(s) => s.get((x & 15) as usize, (y & 15) as usize, (z & 15) as usize),
            None => AIR,
        })
    }

    /// Air where unloaded, like `EmptyLevelChunk`.
    pub fn block_state_or_air(&self, x: i32, y: i32, z: i32) -> StateId {
        self.block_state(x, y, z).unwrap_or(AIR)
    }

    /// `Level.getBiome`: the biome id at the block position, or `None` when
    /// the chunk is not loaded.
    pub fn biome_at(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let chunk = &self.chunks.get(&(x >> 4, z >> 4))?.chunk;
        if y < self.height.min_y || y > self.height.max_y() {
            return None;
        }
        let section = ((y - self.height.min_y) >> 4) as usize;
        let s = chunk.sections.get(section)?;
        Some(s.biome_at((x & 15) as usize, (y & 15) as usize, (z & 15) as usize))
    }

    /// `Level.isRainingAt`: weather is possible here, the level is raining,
    /// the sky reaches the position, and the biome is warm enough for rain
    /// rather than snow.
    pub fn is_raining_at(&self, pos: (i32, i32, i32)) -> bool {
        if !(self.can_have_weather && self.rain_level > 0.2) {
            return false;
        }
        if !self.sky_visible(pos) {
            return false;
        }
        let Some(id) = self.biome_at(pos.0, pos.1, pos.2) else {
            return false;
        };
        let Some(weather) = self.biomes.get(id as usize) else {
            return false;
        };
        // `Biome.getPrecipitationAt`: a biome with precipitation, warm
        // enough for rain rather than snow.
        weather.has_precipitation && !self.cold_enough_to_snow(weather, pos)
    }

    /// The two sky checks of `Level.precipitationAt` in one column scan:
    /// `canSeeSky` and the `MOTION_BLOCKING` heightmap both fail exactly
    /// when something at or above the position blocks motion or is a fluid.
    /// Vanilla reads a stored heightmap and the sky-light engine; the light
    /// engine is not modelled, and for real blocks nothing passes skylight
    /// without also passing this scan.
    fn sky_visible(&self, pos: (i32, i32, i32)) -> bool {
        for y in pos.1..=self.height.max_y() {
            let Some(state) = self.block_state(pos.0, y, pos.2) else {
                continue;
            };
            if blocks_motion_or_fluid(state) {
                return false;
            }
        }
        true
    }

    /// `Biome.coldEnoughToSnow`: the temperature falling below 0.15, with
    /// the height adjustment above `seaLevel + 17` dropping it further.
    /// Vanilla also subtracts a temperature-noise term there, and its frozen
    /// temperature modifier decides ice patches against open water with two
    /// more noise fields; both are worldgen noise this client does not
    /// model, so a frozen biome is treated as snowing — the conservative
    /// direction, where the riptide waits for the server instead of
    /// predicting a launch the server would not make.
    fn cold_enough_to_snow(&self, weather: &BiomeWeather, pos: (i32, i32, i32)) -> bool {
        if weather.frozen {
            return true;
        }
        let mut temperature = weather.temperature;
        let snow_level = self.sea_level + 17;
        if pos.1 > snow_level {
            temperature -= (pos.1 - snow_level) as f32 * 0.05 / 40.0;
        }
        temperature < 0.15
    }

    pub fn set_block_state(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        if y < self.height.min_y || y > self.height.max_y() {
            return;
        }
        let old_state = self.block_state(x, y, z);
        let Some(entry) = self.chunks.get_mut(&(x >> 4, z >> 4)) else {
            return;
        };
        let mut changed = false;
        {
            // Copy on write: a snapshot keeps the chunk as it was.
            let chunk = Arc::make_mut(&mut entry.chunk);
            let section = ((y - self.height.min_y) >> 4) as usize;
            if let Some(s) = chunk.sections.get_mut(section) {
                let (sx, sy, sz) = ((x & 15) as usize, (y & 15) as usize, (z & 15) as usize);
                if s.get(sx, sy, sz) != state {
                    s.set(sx, sy, sz, state);
                    changed = true;
                }
            }
            // `LevelChunk.setBlockState` removes the block entity when the
            // block changes. Property-only updates keep it; copper chests
            // and golem statues keep it across their oxidation family too.
            let registry = Registry::get();
            let block_changed = old_state.is_some_and(|old| {
                old != state
                    && registry
                        .states
                        .get(old as usize)
                        .zip(registry.states.get(state as usize))
                        .is_some_and(|(old, new)| {
                            old.block != new.block
                                && !keeps_block_entity_on_block_change(
                                    &registry.blocks[old.block as usize].name,
                                    &registry.blocks[new.block as usize].name,
                                )
                        })
            });
            if block_changed
                && chunk
                    .block_entities
                    .remove(&((x & 15) as u8, y, (z & 15) as u8))
                    .is_some()
            {
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
            entry.revision = self.revision;
        }
    }
}

/// The `MOTION_BLOCKING` heightmap's test: a state counts when its collision
/// shape is not empty or its fluid is not empty.
fn blocks_motion_or_fluid(state: StateId) -> bool {
    let registry = Registry::get();
    registry.collision_shape(state).is_some() || !registry.state(state).fluid.is_empty()
}

/// `BlockBehaviour.shouldChangedStateKeepBlockEntity` overrides in 26.3:
/// copper chests and copper golem statues keep their block entity when the
/// block changes within the same family (for example, oxidation).
fn keeps_block_entity_on_block_change(old_block: &str, new_block: &str) -> bool {
    fn family(name: &str) -> Option<&'static str> {
        if name.ends_with("copper_chest") {
            Some("copper_chest")
        } else if name.ends_with("copper_golem_statue") {
            Some("copper_golem_statue")
        } else {
            None
        }
    }
    family(old_block).is_some_and(|old| family(new_block) == Some(old))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_nbt::Tag;

    fn tag(value: &str) -> Compound {
        let mut compound = Compound::default();
        compound.insert("id", Tag::String(value.into()));
        compound
    }

    fn states_of(name: &str) -> Vec<StateId> {
        let registry = Registry::get();
        let block = registry.block_by_name(name).unwrap().id;
        registry
            .states
            .iter()
            .enumerate()
            .filter_map(|(id, state)| (state.block == block).then_some(id as StateId))
            .collect()
    }

    #[test]
    fn copper_families_keep_block_entities_across_block_changes() {
        assert!(keeps_block_entity_on_block_change(
            "minecraft:copper_chest",
            "minecraft:oxidized_copper_chest"
        ));
        assert!(keeps_block_entity_on_block_change(
            "minecraft:waxed_copper_golem_statue",
            "minecraft:weathered_copper_golem_statue"
        ));
        assert!(!keeps_block_entity_on_block_change(
            "minecraft:copper_chest",
            "minecraft:copper_golem_statue"
        ));
        assert!(!keeps_block_entity_on_block_change(
            "minecraft:copper_chest",
            "minecraft:stone"
        ));
    }

    /// One chunk under an open sky, raining, whose only biome is a warm one
    /// with precipitation — the shape the riptide's rain check needs.
    fn raining_world() -> World {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        world.rain_level = 1.0;
        world.biomes = vec![BiomeWeather {
            has_precipitation: true,
            temperature: 0.8,
            frozen: false,
        }];
        world
    }

    #[test]
    fn rain_reaches_an_open_sky_block_in_a_warm_biome() {
        let world = raining_world();
        assert!(world.is_raining_at((1, 70, 1)));
    }

    #[test]
    fn rain_needs_the_rain_level_above_vanillas_threshold() {
        let mut world = raining_world();
        world.rain_level = 0.2;
        assert!(!world.is_raining_at((1, 70, 1)));
        world.rain_level = 0.21;
        assert!(world.is_raining_at((1, 70, 1)));
    }

    #[test]
    fn dimensions_without_weather_never_rain() {
        let mut world = raining_world();
        world.can_have_weather = false;
        assert!(!world.is_raining_at((1, 70, 1)));
    }

    #[test]
    fn rain_stops_under_a_roof() {
        let mut world = raining_world();
        world.set_block_state(1, 72, 1, states_of("minecraft:stone")[0]);
        assert!(!world.is_raining_at((1, 70, 1)));
    }

    /// The `MOTION_BLOCKING` heightmap counts a fluid at the position itself,
    /// so standing in ankle-deep water is not rain either.
    #[test]
    fn water_at_the_position_blocks_rain_like_the_heightmap() {
        let mut world = raining_world();
        world.set_block_state(1, 70, 1, states_of("minecraft:water")[0]);
        assert!(!world.is_raining_at((1, 70, 1)));
    }

    #[test]
    fn a_snowy_biome_is_not_rain() {
        let mut world = raining_world();
        world.biomes[0].temperature = 0.05;
        assert!(!world.is_raining_at((1, 70, 1)));
    }

    /// The frozen temperature modifier's ice-patch noise is not modelled;
    /// treating it as snowing keeps the riptide from predicting a launch the
    /// server would refuse.
    #[test]
    fn a_frozen_biome_is_treated_as_snowing() {
        let mut world = raining_world();
        world.biomes[0].frozen = true;
        assert!(!world.is_raining_at((1, 70, 1)));
    }

    #[test]
    fn block_entity_data_updates_only_a_matching_type() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        assert!(!world.apply_block_entity_data(1, 64, 1, 3, tag("first")));

        world.insert_chunk(0, 0, Chunk::empty(24));
        assert!(world.apply_block_entity_data(1, 64, 1, 3, tag("first")));
        assert_eq!(
            world.block_entity(1, 64, 1).unwrap().tag,
            Some(tag("first"))
        );

        // A different block entity type at the same position is ignored.
        assert!(!world.apply_block_entity_data(1, 64, 1, 4, tag("wrong")));
        assert_eq!(world.block_entity(1, 64, 1).unwrap().type_id, 3);
        assert!(world.apply_block_entity_data(1, 64, 1, 3, tag("second")));
        assert_eq!(
            world.block_entity(1, 64, 1).unwrap().tag,
            Some(tag("second"))
        );
    }

    #[test]
    fn block_entities_are_dropped_with_their_chunk_or_block() {
        let chest = states_of("minecraft:chest");
        let stone = states_of("minecraft:stone");
        assert!(chest.len() >= 2);

        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        world.set_block_state(3, 64, 7, chest[0]);
        assert!(world.apply_block_entity_data(3, 64, 7, 5, tag("chest")));
        assert!(world.block_entity(3, 64, 7).is_some());

        // A property-only change keeps the block entity.
        world.set_block_state(3, 64, 7, chest[1]);
        assert!(world.block_entity(3, 64, 7).is_some());

        // Replacing the block drops it.
        world.set_block_state(3, 64, 7, stone[0]);
        assert!(world.block_entity(3, 64, 7).is_none());

        world.insert_chunk(0, 0, Chunk::empty(24));
        assert!(world.block_entity(3, 64, 7).is_none());
        world.remove_chunk(0, 0);
        assert!(!world.apply_block_entity_data(3, 64, 7, 5, tag("chest")));
    }

    /// A snapshot is immutable: the world updates copy the chunk out from
    /// under it, and the revision tells a consumer that it happened.
    #[test]
    fn a_chunk_snapshot_survives_a_block_update() {
        let stone = states_of("minecraft:stone")[0];
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        let (revision, snapshot) = world.chunk_snapshot(0, 0).unwrap();
        let before = world.observation_id();
        let air = world.block_state(1, 64, 1).unwrap();
        // y = 64 is section 8 of an overworld chunk.
        let section = ((64 - world.height.min_y) >> 4) as usize;

        world.set_block_state(1, 64, 1, stone);
        assert_eq!(
            snapshot.sections[section].get(1, 0, 1),
            air,
            "the snapshot kept its contents"
        );

        let (fresh_revision, fresh) = world.chunk_snapshot(0, 0).unwrap();
        assert!(fresh_revision > revision, "the chunk's revision moved");
        assert!(world.observation_id() > before, "and so did the world's");
        assert!(!Arc::ptr_eq(&fresh, &snapshot), "copy on write");
        assert_eq!(fresh.sections[section].get(1, 0, 1), stone);

        // A write that changes nothing leaves the revision alone.
        let unchanged = world.observation_id();
        world.set_block_state(1, 64, 1, stone);
        assert_eq!(world.observation_id(), unchanged);
    }

    /// Unloading forgets the chunk, and loading the same coordinates again is
    /// a different chunk: a cache keyed by the revision cannot reuse the old
    /// one, while the snapshot itself stays readable.
    #[test]
    fn unloading_and_reloading_a_chunk_invalidates_snapshots() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        let (first, snapshot) = world.chunk_snapshot(0, 0).unwrap();
        let loaded = world.observation_id();

        world.remove_chunk(0, 0);
        assert!(world.chunk_snapshot(0, 0).is_none(), "unloaded is not air");
        assert!(world.observation_id() > loaded);
        assert_eq!(snapshot.sections.len(), 24, "still readable");

        world.insert_chunk(0, 0, Chunk::empty(24));
        let (second, _) = world.chunk_snapshot(0, 0).unwrap();
        assert!(second > first, "{second} > {first}");
    }

    /// Every world carries its own identity, so a replaced level (a respawn
    /// or a reconfiguration) cannot be confused with the one before it.
    #[test]
    fn every_world_has_its_own_generation() {
        let first = World::new(DimensionHeight::OVERWORLD);
        let second = World::new(DimensionHeight::OVERWORLD);
        assert_ne!(first.generation(), second.generation());
        assert_eq!(first.observation_id(), 0, "nothing has happened yet");

        let mut third = World::new(DimensionHeight::OVERWORLD);
        third.insert_chunk(0, 0, Chunk::empty(24));
        assert_eq!(third.observation_id(), 1);
    }

    /// A bounded region query shares the loaded chunks rather than copying
    /// them, and does not walk the rest of the world.
    #[test]
    fn a_region_query_shares_chunks_rather_than_copying_them() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        world.insert_chunk(1, 0, Chunk::empty(24));
        world.insert_chunk(5, 5, Chunk::empty(24));

        let (_, live) = world.chunk_snapshot(0, 0).unwrap();
        let region = world.chunk_snapshots_in((0, 0), (1, 0));
        assert_eq!(region.len(), 2);
        assert_eq!(region[0].0, (0, 0));
        assert_eq!(region[1].0, (1, 0));
        assert!(Arc::ptr_eq(&region[0].2, &live), "no copy while unchanged");
        assert!(world.chunk_snapshots_in((9, 9), (10, 10)).is_empty());
    }

    /// Block entity changes are part of what a snapshot sees, so they move
    /// the revision too.
    #[test]
    fn block_entity_changes_move_the_revision() {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        world.insert_chunk(0, 0, Chunk::empty(24));
        let before = world.observation_id();
        assert!(world.apply_block_entity_data(1, 64, 1, 3, tag("chest")));
        assert!(world.observation_id() > before);

        // A type that does not match the record changes nothing.
        let unchanged = world.observation_id();
        assert!(!world.apply_block_entity_data(1, 64, 1, 4, tag("wrong")));
        assert_eq!(world.observation_id(), unchanged);
    }
}
