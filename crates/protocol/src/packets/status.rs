use rapidbot_buf::{Decode, Encode};

use super::packet;

pub mod clientbound {
    use super::*;

    /// `ClientboundStatusResponsePacket`: the server list JSON.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct StatusResponse {
        pub json: String,
    }
    packet!(StatusResponse, status, clientbound, STATUS_RESPONSE);

    /// `ClientboundPongResponsePacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PongResponse {
        pub time: i64,
    }
    packet!(PongResponse, status, clientbound, PONG_RESPONSE);
}

pub mod serverbound {
    use super::*;

    /// `ServerboundStatusRequestPacket`: no body.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct StatusRequest;
    packet!(StatusRequest, status, serverbound, STATUS_REQUEST);

    /// `ServerboundPingRequestPacket`. Vanilla sends `Util.getMillis()`,
    /// which is `System.nanoTime() / 1e6`: a monotonic clock, not epoch time.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct PingRequest {
        pub time: i64,
    }
    packet!(PingRequest, status, serverbound, PING_REQUEST);
}
