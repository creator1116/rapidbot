//! Vanilla player movement, ported from the 26.3 client.

pub mod attributes;
pub mod math;
#[allow(dead_code)]
mod mth_tables;
pub mod player;

pub use player::{Abilities, Effects, Keys, Player, Pose};
