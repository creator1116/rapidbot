use rapidbot_buf::{BoundedString, ByteArray, Decode, Encode, Identifier, RemainingBytes};
use uuid::Uuid;

use super::packet;

/// `ByteBufCodecs.GAME_PROFILE`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct GameProfile {
    pub id: Uuid,
    pub name: BoundedString<16>,
    pub properties: Vec<ProfileProperty>,
}

/// `ByteBufCodecs.GAME_PROFILE_PROPERTIES` entry (skin textures and the like).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ProfileProperty {
    pub name: BoundedString<64>,
    pub value: String,
    pub signature: Option<BoundedString<1024>>,
}

pub mod clientbound {
    use super::*;

    /// `ClientboundLoginDisconnectPacket`: a JSON text component.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct LoginDisconnect {
        pub reason: BoundedString<262144>,
    }
    packet!(LoginDisconnect, login, clientbound, LOGIN_DISCONNECT);

    /// `ClientboundHelloPacket`: starts encryption.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct Hello {
        pub server_id: BoundedString<20>,
        /// X.509 SubjectPublicKeyInfo, DER.
        pub public_key: ByteArray,
        pub challenge: ByteArray,
        pub should_authenticate: bool,
    }
    packet!(Hello, login, clientbound, HELLO);

    /// `ClientboundLoginFinishedPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct LoginFinished {
        pub profile: GameProfile,
        pub session_id: Uuid,
    }
    packet!(LoginFinished, login, clientbound, LOGIN_FINISHED);

    /// `ClientboundLoginCompressionPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct LoginCompression {
        #[var]
        pub threshold: i32,
    }
    packet!(LoginCompression, login, clientbound, LOGIN_COMPRESSION);

    /// `ClientboundCustomQueryPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct CustomQuery {
        #[var]
        pub transaction_id: i32,
        pub channel: Identifier,
        pub payload: RemainingBytes,
    }
    packet!(CustomQuery, login, clientbound, CUSTOM_QUERY);

    /// `ClientboundCookieRequestPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct CookieRequest {
        pub key: Identifier,
    }
    packet!(CookieRequest, login, clientbound, COOKIE_REQUEST);
}

pub mod serverbound {
    use super::*;

    /// `ServerboundHelloPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct Hello {
        pub name: BoundedString<16>,
        pub profile_id: Uuid,
    }
    packet!(Hello, login, serverbound, HELLO);

    /// `ServerboundKeyPacket`: the shared secret and challenge, both
    /// RSA-encrypted with the server's key.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct Key {
        pub key: ByteArray,
        pub encrypted_challenge: ByteArray,
    }
    packet!(Key, login, serverbound, KEY);

    /// `ServerboundCustomQueryAnswerPacket`. The vanilla client understands
    /// no queries and always answers with a null payload.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct CustomQueryAnswer {
        #[var]
        pub transaction_id: i32,
        pub payload: Option<RemainingBytes>,
    }
    packet!(CustomQueryAnswer, login, serverbound, CUSTOM_QUERY_ANSWER);

    /// `ServerboundLoginAcknowledgedPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct LoginAcknowledged;
    packet!(LoginAcknowledged, login, serverbound, LOGIN_ACKNOWLEDGED);

    /// `ServerboundCookieResponsePacket`. Payload is at most 5120 bytes.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct CookieResponse {
        pub key: Identifier,
        pub payload: Option<ByteArray>,
    }
    packet!(CookieResponse, login, serverbound, COOKIE_RESPONSE);
}
