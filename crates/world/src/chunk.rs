//! Chunk columns: `LevelChunkSection` block states as `PalettedContainer`s.

use std::collections::HashMap;

use rapidbot_buf::{Decode, DecodeError, VarInt, take};
use rapidbot_nbt::{Compound, OptionalCompound};

use crate::registry::{AIR, StateId};

/// Blocks per section.
const SECTION_VOLUME: usize = 4096;

/// `Mth.ceillog2`.
fn ceillog2(n: usize) -> u32 {
    if n <= 1 {
        0
    } else {
        usize::BITS - (n - 1).leading_zeros()
    }
}

/// A section's block states, kept in the compact form the server sent until
/// something writes to it.
#[derive(Debug, Clone)]
enum Container {
    Single(StateId),
    Packed {
        /// `None` means global IDs are stored directly.
        palette: Option<Vec<StateId>>,
        bits: u32,
        data: Vec<u64>,
    },
    Direct(Box<[StateId]>),
}

impl Container {
    fn get(&self, index: usize) -> StateId {
        match self {
            Container::Single(v) => *v,
            Container::Packed {
                palette,
                bits,
                data,
            } => {
                // SimpleBitStorage: values never straddle two longs.
                let per_long = (64 / bits) as usize;
                let word = data[index / per_long];
                let shift = (index % per_long) as u32 * bits;
                let raw = ((word >> shift) & ((1u64 << bits) - 1)) as u32;
                match palette {
                    Some(p) => p.get(raw as usize).copied().unwrap_or(AIR),
                    None => raw,
                }
            }
            Container::Direct(values) => values[index],
        }
    }

    fn set(&mut self, index: usize, value: StateId) {
        if !matches!(self, Container::Direct(_)) {
            let values: Box<[StateId]> = (0..SECTION_VOLUME).map(|i| self.get(i)).collect();
            *self = Container::Direct(values);
        }
        if let Container::Direct(values) = self {
            values[index] = value;
        }
    }

    /// `PalettedContainer.read` with the block-state `Strategy`.
    fn read_block_states(
        buf: &mut &[u8],
        global_bits: u32,
        state_count: usize,
    ) -> Result<Self, DecodeError> {
        let bits = u8::decode(buf)? as u32;
        match bits {
            0 => {
                let value = VarInt::decode(buf)?.0 as StateId;
                // ZeroBitStorage: no longs follow.
                Ok(Container::Single(value))
            }
            // 1-4 bits all use a 4-bit linear palette; 5-8 a hash-map palette.
            1..=8 => {
                let storage_bits = if bits <= 4 { 4 } else { bits };
                let palette = read_palette(buf)?;
                let data = read_longs(buf, storage_bits)?;
                Ok(Container::Packed {
                    palette: Some(palette),
                    bits: storage_bits,
                    data,
                })
            }
            // Global palette: stored at the in-memory width of the registry.
            _ => {
                let data = read_longs(buf, global_bits)?;
                validate_global_states(&data, global_bits, state_count)?;
                Ok(Container::Packed {
                    palette: None,
                    bits: global_bits,
                    data,
                })
            }
        }
    }
}

fn validate_global_states(data: &[u64], bits: u32, state_count: usize) -> Result<(), DecodeError> {
    if bits == 0 || bits > 64 {
        return Err(DecodeError::Custom(
            "invalid global block state bit width".into(),
        ));
    }
    let per_long = (64 / bits) as usize;
    let mask = u64::MAX >> (64 - bits);
    for index in 0..SECTION_VOLUME {
        let word = data[index / per_long];
        let shift = (index % per_long) as u32 * bits;
        let raw = (word >> shift) & mask;
        if usize::try_from(raw).map_or(true, |id| id >= state_count) {
            return Err(DecodeError::Custom(format!(
                "global block state ID {raw} exceeds registry size {state_count}"
            )));
        }
    }
    Ok(())
}

fn read_palette(buf: &mut &[u8]) -> Result<Vec<StateId>, DecodeError> {
    let size = VarInt::decode(buf)?.0;
    let size = usize::try_from(size).map_err(|_| DecodeError::NegativeLength(size))?;
    if size > buf.len() {
        return Err(DecodeError::UnexpectedEof);
    }
    (0..size)
        .map(|_| VarInt::decode(buf).map(|v| v.0 as StateId))
        .collect()
}

/// `readFixedSizeLongArray` sized for `SimpleBitStorage(bits, 4096)`.
fn read_longs(buf: &mut &[u8], bits: u32) -> Result<Vec<u64>, DecodeError> {
    read_longs_for(buf, bits, SECTION_VOLUME)
}

fn read_longs_for(buf: &mut &[u8], bits: u32, entries: usize) -> Result<Vec<u64>, DecodeError> {
    if bits == 0 {
        return Ok(Vec::new());
    }
    let per_long = (64 / bits) as usize;
    let count = entries.div_ceil(per_long);
    let bytes = take(buf, count * 8)?;
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_be_bytes(*c))
        .collect())
}

/// Skips a biome container (`Strategy.createForBiomes`, 64 entries).
fn skip_biomes(buf: &mut &[u8], global_bits: u32) -> Result<(), DecodeError> {
    let bits = u8::decode(buf)? as u32;
    match bits {
        0 => {
            VarInt::decode(buf)?;
        }
        1..=3 => {
            read_palette(buf)?;
            read_longs_for(buf, bits, 64)?;
        }
        _ => {
            read_longs_for(buf, global_bits, 64)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Section {
    states: Container,
}

impl Section {
    pub fn get(&self, x: usize, y: usize, z: usize) -> StateId {
        self.states.get((y << 4 | z) << 4 | x)
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, state: StateId) {
        self.states.set((y << 4 | z) << 4 | x, state);
    }
}

/// A block entity as the client sees it: the registry ID of its type and the
/// update tag the server sent. Chunk packets may omit the tag when the block
/// entity has nothing to send.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntity {
    pub type_id: i32,
    pub tag: Option<Compound>,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub sections: Vec<Section>,
    /// Block entities by chunk-relative `(x, y, z)`.
    pub block_entities: HashMap<(u8, i32, u8), BlockEntity>,
}

impl Chunk {
    /// An all-air chunk with `sections` sections, for building test worlds.
    pub fn empty(sections: usize) -> Self {
        Chunk {
            sections: (0..sections)
                .map(|_| Section {
                    states: Container::Single(AIR),
                })
                .collect(),
            block_entities: HashMap::new(),
        }
    }

    /// Parses the section buffer of `ClientboundLevelChunkPacketData`
    /// (after the heightmaps). `state_count` and `biome_count` are the
    /// registry sizes that fix the global-palette widths.
    pub fn read_sections(
        mut buf: &[u8],
        state_count: usize,
        biome_count: usize,
    ) -> Result<Self, DecodeError> {
        let state_bits = ceillog2(state_count);
        let biome_bits = ceillog2(biome_count);
        let mut sections = Vec::new();
        while !buf.is_empty() {
            let _non_empty = i16::decode(&mut buf)?;
            let _fluids = i16::decode(&mut buf)?;
            let states = Container::read_block_states(&mut buf, state_bits, state_count)?;
            skip_biomes(&mut buf, biome_bits)?;
            sections.push(Section { states });
        }
        Ok(Chunk {
            sections,
            block_entities: HashMap::new(),
        })
    }

    /// Parses the whole chunk payload of `ClientboundLevelChunkWithLightPacket`
    /// after the x/z coordinates: heightmaps, section buffer, block entities,
    /// light (ignored).
    pub fn read_packet(
        mut buf: &[u8],
        state_count: usize,
        biome_count: usize,
    ) -> Result<Self, DecodeError> {
        // Heightmaps: map of VarInt type -> long array.
        let heightmaps = VarInt::decode(&mut buf)?.0;
        let heightmaps =
            usize::try_from(heightmaps).map_err(|_| DecodeError::NegativeLength(heightmaps))?;
        for _ in 0..heightmaps {
            VarInt::decode(&mut buf)?;
            let len = VarInt::decode(&mut buf)?.0;
            let len = usize::try_from(len).map_err(|_| DecodeError::NegativeLength(len))?;
            take(&mut buf, len * 8)?;
        }
        let data = rapidbot_buf::ByteArray::decode(&mut buf)?;
        let mut chunk = Self::read_sections(&data.0, state_count, biome_count)?;

        let count = VarInt::decode(&mut buf)?.0;
        let count = usize::try_from(count).map_err(|_| DecodeError::NegativeLength(count))?;
        // Each entry is at least a byte, a short, a VarInt and an End tag.
        if count > buf.len() / 5 {
            return Err(DecodeError::UnexpectedEof);
        }
        for _ in 0..count {
            let packed_xz = i8::decode(&mut buf)?;
            let y = i16::decode(&mut buf)?;
            let type_id = VarInt::decode(&mut buf)?.0;
            let tag = OptionalCompound::decode(&mut buf)?.0;
            let x = ((packed_xz >> 4) & 15) as u8;
            let z = (packed_xz & 15) as u8;
            chunk
                .block_entities
                .insert((x, i32::from(y), z), BlockEntity { type_id, tag });
        }
        Ok(chunk)
    }
}

#[cfg(test)]
mod tests {
    use rapidbot_buf::Encode;

    use super::*;

    #[test]
    fn ceillog2_matches_mth() {
        assert_eq!(ceillog2(1), 0);
        assert_eq!(ceillog2(2), 1);
        assert_eq!(ceillog2(16), 4);
        assert_eq!(ceillog2(17), 5);
        assert_eq!(ceillog2(35723), 16);
    }

    #[test]
    fn palette_count_cannot_exceed_remaining_bytes() {
        let mut oversized = Vec::new();
        VarInt(i32::MAX).encode(&mut oversized);
        assert!(matches!(
            read_palette(&mut oversized.as_slice()),
            Err(DecodeError::UnexpectedEof)
        ));

        let mut negative = Vec::new();
        VarInt(-1).encode(&mut negative);
        assert!(matches!(
            read_palette(&mut negative.as_slice()),
            Err(DecodeError::NegativeLength(-1))
        ));
    }

    #[test]
    fn packed_section_roundtrip() {
        // One section: 4-bit linear palette [air, stone], y=0 layer is stone.
        let mut buf = Vec::new();
        buf.extend_from_slice(&256i16.to_be_bytes());
        buf.extend_from_slice(&0i16.to_be_bytes());
        buf.push(1); // 1 bit requested -> 4-bit linear storage
        buf.extend_from_slice(&[2, 0, 1]);
        for word in 0..256u64 {
            // 16 entries per long; the first 256 entries (y = 0) are stone.
            let v = if word < 16 {
                0x1111_1111_1111_1111u64
            } else {
                0
            };
            buf.extend_from_slice(&v.to_be_bytes());
        }
        // Biomes: single value.
        buf.extend_from_slice(&[0, 0]);

        let chunk = Chunk::read_sections(&buf, 35723, 64).unwrap();
        assert_eq!(chunk.sections.len(), 1);
        assert_eq!(chunk.sections[0].get(3, 0, 7), 1);
        assert_eq!(chunk.sections[0].get(3, 1, 7), 0);

        let mut s = chunk.sections[0].clone();
        s.set(3, 1, 7, 42);
        assert_eq!(s.get(3, 1, 7), 42);
        assert_eq!(s.get(3, 0, 7), 1);
    }

    #[test]
    fn heightmap_count_rejects_negative_values() {
        let negative = rapidbot_buf::to_bytes(&VarInt(-1));
        assert!(matches!(
            Chunk::read_packet(&negative, 1, 1),
            Err(DecodeError::NegativeLength(-1))
        ));

        let mut empty = rapidbot_buf::to_bytes(&VarInt(0));
        empty.extend(rapidbot_buf::to_bytes(&rapidbot_buf::ByteArray::default()));
        empty.extend(rapidbot_buf::to_bytes(&VarInt(0)));
        assert!(
            Chunk::read_packet(&empty, 1, 1)
                .unwrap()
                .sections
                .is_empty()
        );
    }

    #[test]
    fn read_packet_loads_block_entities() {
        let mut compound = Compound::default();
        compound.insert("id", rapidbot_nbt::Tag::String("minecraft:chest".into()));
        compound.insert("Lock", rapidbot_nbt::Tag::String("secret".into()));

        let mut buf = rapidbot_buf::to_bytes(&VarInt(0)); // heightmaps
        buf.extend(rapidbot_buf::to_bytes(&rapidbot_buf::ByteArray::default()));
        buf.extend(rapidbot_buf::to_bytes(&VarInt(2)));

        // Chunk-relative x=3, z=7, y=64 with a tag.
        buf.push(0x37);
        buf.extend_from_slice(&64i16.to_be_bytes());
        VarInt(5).encode(&mut buf);
        compound.encode(&mut buf);

        // x=15, z=0, y=-12 without a tag.
        buf.push(0xF0);
        buf.extend_from_slice(&(-12i16).to_be_bytes());
        VarInt(9).encode(&mut buf);
        buf.push(rapidbot_nbt::id::END);

        let chunk = Chunk::read_packet(&buf, 1, 1).unwrap();
        assert_eq!(
            chunk.block_entities.get(&(3, 64, 7)),
            Some(&BlockEntity {
                type_id: 5,
                tag: Some(compound),
            })
        );
        assert_eq!(
            chunk.block_entities.get(&(15, -12, 0)),
            Some(&BlockEntity {
                type_id: 9,
                tag: None,
            })
        );
    }

    #[test]
    fn global_palette_ids_must_exist_in_registry() {
        // Cover invalid IDs in the first, shifted, and final packed slots.
        let read = |state_index: usize, state_id: u64| {
            let mut buf = Vec::new();
            buf.extend_from_slice(&0i16.to_be_bytes());
            buf.extend_from_slice(&0i16.to_be_bytes());
            buf.push(9); // global palette; registry size 17 uses 5-bit IDs
            let per_long = 64 / 5;
            let longs = SECTION_VOLUME.div_ceil(per_long);
            for word_index in 0..longs {
                let word = if word_index == state_index / per_long {
                    state_id << ((state_index % per_long) * 5)
                } else {
                    0
                };
                buf.extend_from_slice(&word.to_be_bytes());
            }
            buf.extend_from_slice(&[0, 0]); // single-value biome container
            Chunk::read_sections(&buf, 17, 64)
        };

        assert_eq!(read(0, 16).unwrap().sections[0].get(0, 0, 0), 16);
        for invalid_index in [0, 5, SECTION_VOLUME - 1] {
            assert!(
                matches!(read(invalid_index, 17), Err(DecodeError::Custom(_))),
                "invalid state ID in packed slot {invalid_index} was accepted"
            );
        }
    }
}
