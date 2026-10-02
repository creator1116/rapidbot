//! Tags from `ClientboundUpdateTagsPacket`. The client uses the server's
//! tags, not its built-in ones, so these must come from the packet.

use std::collections::{HashMap, HashSet};

use rapidbot_buf::{Decode, DecodeError, Identifier, VarInt};

#[derive(Debug, Default, Clone)]
pub struct Tags {
    /// registry -> tag -> element IDs in that registry.
    registries: HashMap<String, HashMap<String, HashSet<u32>>>,
}

impl Tags {
    /// Merges one `update_tags` payload: VarInt registry count, then for each
    /// registry its identifier and a map of tag identifier -> VarInt ID list.
    pub fn apply(&mut self, mut buf: &[u8]) -> Result<(), DecodeError> {
        let registries = VarInt::decode(&mut buf)?.0;
        for _ in 0..registries {
            let registry = Identifier::decode(&mut buf)?.normalized().into_owned();
            let tags = VarInt::decode(&mut buf)?.0;
            let map = self.registries.entry(registry).or_default();
            map.clear();
            for _ in 0..tags {
                let tag = Identifier::decode(&mut buf)?.normalized().into_owned();
                let ids = Vec::<VarInt>::decode(&mut buf)?;
                map.insert(tag, ids.into_iter().map(|v| v.0 as u32).collect());
            }
        }
        Ok(())
    }

    /// Adds an element to a tag (tests, and worlds built without a server).
    pub fn insert(&mut self, registry: &str, tag: &str, id: u32) {
        self.registries.entry(registry.to_owned()).or_default().entry(tag.to_owned()).or_default().insert(id);
    }

    pub fn contains(&self, registry: &str, tag: &str, id: u32) -> bool {
        self.registries
            .get(registry)
            .and_then(|r| r.get(tag))
            .is_some_and(|ids| ids.contains(&id))
    }

    /// `BlockState.is(tag)` for a block registry ID.
    pub fn block_is(&self, tag: &str, block_id: u32) -> bool {
        self.contains("minecraft:block", tag, block_id)
    }
}
