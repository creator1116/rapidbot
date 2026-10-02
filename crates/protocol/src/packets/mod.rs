//! Packet definitions, grouped by connection state.
//!
//! Field order and encodings follow each vanilla packet's `write` method.

pub mod common;
pub mod configuration;
pub mod handshake;
pub mod login;
pub mod play;
pub mod status;

/// Implements [`crate::Packet`] for a type using its generated ID constant.
macro_rules! packet {
    ($ty:ty, $state:ident, $dir:ident, $id:ident) => {
        impl $crate::Packet for $ty {
            const ID: i32 = $crate::ids::$state::$dir::$id;
            const STATE: $crate::State = packet!(@state $state);
            const DIRECTION: $crate::Direction = packet!(@dir $dir);
        }
    };
    (@state handshake) => { $crate::State::Handshake };
    (@state status) => { $crate::State::Status };
    (@state login) => { $crate::State::Login };
    (@state configuration) => { $crate::State::Configuration };
    (@state play) => { $crate::State::Play };
    (@dir clientbound) => { $crate::Direction::Clientbound };
    (@dir serverbound) => { $crate::Direction::Serverbound };
}
pub(crate) use packet;
