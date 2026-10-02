use rapidbot_buf::{BoundedString, Decode, Encode};

use super::packet;

/// `ClientIntentionPacket`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Intention {
    #[var]
    pub protocol_version: i32,
    /// The client sends the host it connected to *after* SRV resolution.
    pub host: BoundedString<255>,
    pub port: u16,
    pub intent: Intent,
}
packet!(Intention, handshake, serverbound, INTENTION);

/// `ClientIntent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum Intent {
    Status = 1,
    Login = 2,
    Transfer = 3,
}
