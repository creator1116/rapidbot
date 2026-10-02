//! The Minecraft Java Edition protocol.

mod cipher;
mod connection;
pub mod frame;
pub mod ids;
pub mod packets;

pub use connection::{Connection, ConnectionError, RawPacket, Reader, Writer};
pub use ids::{PROTOCOL_VERSION, RESOURCE_PACK_FORMAT, VERSION_ID, VERSION_NAME};

use rapidbot_buf::{Decode, DecodeError, Encode, VarInt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Clientbound,
    Serverbound,
}

impl State {
    /// Vanilla's resource name for a packet ID, for logging.
    pub fn packet_name(self, direction: Direction, id: i32) -> Option<&'static str> {
        use Direction::*;
        let names = match (self, direction) {
            (State::Handshake, Serverbound) => ids::handshake::serverbound::NAMES,
            (State::Handshake, Clientbound) => &[],
            (State::Status, Clientbound) => ids::status::clientbound::NAMES,
            (State::Status, Serverbound) => ids::status::serverbound::NAMES,
            (State::Login, Clientbound) => ids::login::clientbound::NAMES,
            (State::Login, Serverbound) => ids::login::serverbound::NAMES,
            (State::Configuration, Clientbound) => ids::configuration::clientbound::NAMES,
            (State::Configuration, Serverbound) => ids::configuration::serverbound::NAMES,
            (State::Play, Clientbound) => ids::play::clientbound::NAMES,
            (State::Play, Serverbound) => ids::play::serverbound::NAMES,
        };
        usize::try_from(id).ok().and_then(|i| names.get(i).copied())
    }
}

/// A packet with a fixed ID in a given state and direction.
pub trait Packet: Encode + Decode {
    const ID: i32;
    const STATE: State;
    const DIRECTION: Direction;

    /// Packet ID followed by the body, ready for framing.
    fn to_frame_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        VarInt(Self::ID).encode(&mut buf);
        self.encode(&mut buf);
        buf
    }

    /// Decodes a packet body. Like vanilla, trailing bytes are an error.
    fn from_body(mut body: &[u8]) -> Result<Self, DecodeError> {
        let packet = Self::decode(&mut body)?;
        if !body.is_empty() {
            return Err(DecodeError::Custom(format!(
                "packet {} was larger than expected, found {} bytes extra",
                Self::ID,
                body.len()
            )));
        }
        Ok(packet)
    }
}
