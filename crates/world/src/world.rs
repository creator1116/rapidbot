//! The client's view of one dimension.

use std::collections::HashMap;

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
    chunks: HashMap<(i32, i32), Chunk>,
}

impl World {
    pub fn new(height: DimensionHeight) -> Self {
        Self {
            height,
            chunks: HashMap::new(),
        }
    }

    pub fn insert_chunk(&mut self, x: i32, z: i32, chunk: Chunk) {
        self.chunks.insert((x, z), chunk);
    }

    pub fn remove_chunk(&mut self, x: i32, z: i32) {
        self.chunks.remove(&(x, z));
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
        let Some(chunk) = self.chunks.get_mut(&(x >> 4, z >> 4)) else {
            return false;
        };
        let key = ((x & 15) as u8, y, (z & 15) as u8);
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
    }

    /// `None` when the chunk is not loaded; air outside the build height.
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        let chunk = self.chunks.get(&(x >> 4, z >> 4))?;
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

    pub fn set_block_state(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        if y < self.height.min_y || y > self.height.max_y() {
            return;
        }
        let old_state = self.block_state(x, y, z);
        if let Some(chunk) = self.chunks.get_mut(&(x >> 4, z >> 4)) {
            let section = ((y - self.height.min_y) >> 4) as usize;
            if let Some(s) = chunk.sections.get_mut(section) {
                s.set(
                    (x & 15) as usize,
                    (y & 15) as usize,
                    (z & 15) as usize,
                    state,
                );
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
            if block_changed {
                chunk
                    .block_entities
                    .remove(&((x & 15) as u8, y, (z & 15) as u8));
            }
        }
    }
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
}
