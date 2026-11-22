//! Vanilla player movement, ported from the 26.3 client.

pub mod attributes;
pub mod boat;
pub mod math;
#[allow(dead_code)]
mod mth_tables;
pub mod player;

pub use boat::{Boat, BoatInput, Status as BoatStatus};
pub use player::{Abilities, Effects, Keys, Player, Pose};
