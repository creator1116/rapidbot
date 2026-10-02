//! The client's view of one dimension.

use std::collections::HashMap;

use crate::chunk::Chunk;
use crate::registry::{AIR, StateId};

/// Height range of a dimension (`DimensionType.minY` / `height`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimensionHeight {
    pub min_y: i32,
    pub height: i32,
}

impl DimensionHeight {
    pub const OVERWORLD: Self = Self { min_y: -64, height: 384 };
    pub const NETHER: Self = Self { min_y: 0, height: 256 };
    pub const END: Self = Self { min_y: 0, height: 256 };

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
        Self { height, chunks: HashMap::new() }
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
        if let Some(chunk) = self.chunks.get_mut(&(x >> 4, z >> 4)) {
            let section = ((y - self.height.min_y) >> 4) as usize;
            if let Some(s) = chunk.sections.get_mut(section) {
                s.set((x & 15) as usize, (y & 15) as usize, (z & 15) as usize, state);
            }
        }
    }
}
