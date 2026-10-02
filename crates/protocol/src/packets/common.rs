//! Packets shared by the configuration and play states (vanilla's
//! `net.minecraft.network.protocol.common` and `cookie`). Each state gets its
//! own copy of the types because packet IDs differ between states.

use rapidbot_buf::{Decode, Encode};

/// `ClientInformation`, sent in configuration and again in play whenever
/// options change.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ClientInformation {
    pub language: rapidbot_buf::BoundedString<16>,
    pub view_distance: i8,
    pub chat_visibility: ChatVisibility,
    pub chat_colors: bool,
    /// Bit mask of `PlayerModelPart`s; all seven are on by default.
    pub model_customisation: u8,
    pub main_hand: HumanoidArm,
    pub text_filtering_enabled: bool,
    pub allows_listing: bool,
    pub particle_status: ParticleStatus,
}

impl Default for ClientInformation {
    /// What a fresh vanilla install sends (`Options` field defaults).
    fn default() -> Self {
        Self {
            language: "en_us".into(),
            view_distance: 12,
            chat_visibility: ChatVisibility::Full,
            chat_colors: true,
            model_customisation: 0x7f,
            main_hand: HumanoidArm::Right,
            text_filtering_enabled: false,
            allows_listing: true,
            particle_status: ParticleStatus::All,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum ChatVisibility {
    Full,
    System,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum HumanoidArm {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum ParticleStatus {
    All,
    Decreased,
    Minimal,
}

/// `ServerboundResourcePackPacket.Action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum ResourcePackAction {
    SuccessfullyLoaded,
    Declined,
    FailedDownload,
    Accepted,
    Downloaded,
    InvalidUrl,
    FailedReload,
    Discarded,
}

/// Defines the common packets inside a state module.
macro_rules! common_packets {
    ($state:ident) => {
        pub mod common_clientbound {
            use rapidbot_buf::{BoundedString, ByteArray, Decode, Encode, Identifier, RemainingBytes};
            use rapidbot_nbt::Tag;
            use uuid::Uuid;

            use $crate::packets::packet;

            /// `ClientboundCookieRequestPacket`.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct CookieRequest {
                pub key: Identifier,
            }
            packet!(CookieRequest, $state, clientbound, COOKIE_REQUEST);

            /// `ClientboundCustomPayloadPacket`. `minecraft:brand` carries a
            /// string; anything else is discarded by vanilla.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct CustomPayload {
                pub channel: Identifier,
                pub data: RemainingBytes,
            }
            packet!(CustomPayload, $state, clientbound, CUSTOM_PAYLOAD);

            /// `ClientboundDisconnectPacket`: an NBT text component.
            #[derive(Debug, Clone, PartialEq, Encode, Decode)]
            pub struct Disconnect {
                pub reason: Tag,
            }
            packet!(Disconnect, $state, clientbound, DISCONNECT);

            /// `ClientboundKeepAlivePacket`.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct KeepAlive {
                pub id: i64,
            }
            packet!(KeepAlive, $state, clientbound, KEEP_ALIVE);

            /// `ClientboundPingPacket`, the "transaction" anticheats use to
            /// measure latency.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct Ping {
                pub id: i32,
            }
            packet!(Ping, $state, clientbound, PING);

            /// `ClientboundResourcePackPopPacket`: `None` removes all packs.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct ResourcePackPop {
                pub id: Option<Uuid>,
            }
            packet!(ResourcePackPop, $state, clientbound, RESOURCE_PACK_POP);

            /// `ClientboundResourcePackPushPacket`.
            #[derive(Debug, Clone, PartialEq, Encode, Decode)]
            pub struct ResourcePackPush {
                pub id: Uuid,
                pub url: String,
                pub hash: BoundedString<40>,
                pub required: bool,
                pub prompt: Option<Tag>,
            }
            packet!(ResourcePackPush, $state, clientbound, RESOURCE_PACK_PUSH);

            /// `ClientboundStoreCookiePacket`. Payload is at most 5120 bytes.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct StoreCookie {
                pub key: Identifier,
                pub payload: ByteArray,
            }
            packet!(StoreCookie, $state, clientbound, STORE_COOKIE);

            /// `ClientboundTransferPacket`.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct Transfer {
                pub host: String,
                #[var]
                pub port: i32,
            }
            packet!(Transfer, $state, clientbound, TRANSFER);
        }

        pub mod common_serverbound {
            use rapidbot_buf::{ByteArray, Decode, Encode, Identifier, RemainingBytes};
            use uuid::Uuid;

            use $crate::packets::common::ResourcePackAction;
            use $crate::packets::packet;

            /// `ServerboundClientInformationPacket`.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct ClientInformation(pub $crate::packets::common::ClientInformation);
            packet!(ClientInformation, $state, serverbound, CLIENT_INFORMATION);

            /// `ServerboundCookieResponsePacket`.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct CookieResponse {
                pub key: Identifier,
                pub payload: Option<ByteArray>,
            }
            packet!(CookieResponse, $state, serverbound, COOKIE_RESPONSE);

            /// `ServerboundCustomPayloadPacket`. Vanilla only ever sends
            /// `minecraft:brand`.
            #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
            pub struct CustomPayload {
                pub channel: Identifier,
                pub data: RemainingBytes,
            }
            packet!(CustomPayload, $state, serverbound, CUSTOM_PAYLOAD);

            /// `ServerboundKeepAlivePacket`.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct KeepAlive {
                pub id: i64,
            }
            packet!(KeepAlive, $state, serverbound, KEEP_ALIVE);

            /// `ServerboundPongPacket`.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct Pong {
                pub id: i32,
            }
            packet!(Pong, $state, serverbound, PONG);

            /// `ServerboundResourcePackPacket`.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
            pub struct ResourcePack {
                pub id: Uuid,
                pub action: ResourcePackAction,
            }
            packet!(ResourcePack, $state, serverbound, RESOURCE_PACK);
        }
    };
}
pub(crate) use common_packets;
