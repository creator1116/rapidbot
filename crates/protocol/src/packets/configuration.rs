//! Configuration state: the common packets plus configuration-only ones.

use rapidbot_buf::{Decode, Encode};

super::common::common_packets!(configuration);

/// `KnownPack`: a data pack both sides may already have.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

/// `RegistrySynchronization.PackedRegistryEntry`. `data` is `None` when the
/// server expects the client to take the entry from a known pack.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct RegistryEntry {
    pub id: rapidbot_buf::Identifier,
    pub data: Option<rapidbot_nbt::Tag>,
}

pub mod clientbound {
    pub use super::common_clientbound::*;
    use rapidbot_buf::{Decode, Encode, Identifier, RemainingBytes};

    use super::{KnownPack, RegistryEntry};
    use crate::packets::packet;

    /// `ClientboundFinishConfigurationPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct FinishConfiguration;
    packet!(FinishConfiguration, configuration, clientbound, FINISH_CONFIGURATION);

    /// `ClientboundResetChatPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct ResetChat;
    packet!(ResetChat, configuration, clientbound, RESET_CHAT);

    /// `ClientboundRegistryDataPacket`.
    #[derive(Debug, Clone, PartialEq, Encode, Decode)]
    pub struct RegistryData {
        pub registry: Identifier,
        pub entries: Vec<RegistryEntry>,
    }
    packet!(RegistryData, configuration, clientbound, REGISTRY_DATA);

    /// `ClientboundUpdateEnabledFeaturesPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct UpdateEnabledFeatures {
        pub features: Vec<Identifier>,
    }
    packet!(UpdateEnabledFeatures, configuration, clientbound, UPDATE_ENABLED_FEATURES);

    /// `ClientboundUpdateTagsPacket`. Kept raw until the world model needs tags.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct UpdateTags {
        pub data: RemainingBytes,
    }
    packet!(UpdateTags, configuration, clientbound, UPDATE_TAGS);

    /// `ClientboundSelectKnownPacks`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct SelectKnownPacks {
        pub packs: Vec<KnownPack>,
    }
    packet!(SelectKnownPacks, configuration, clientbound, SELECT_KNOWN_PACKS);

    /// `ClientboundCodeOfConductPacket`.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct CodeOfConduct {
        pub text: String,
    }
    packet!(CodeOfConduct, configuration, clientbound, CODE_OF_CONDUCT);
}

pub mod serverbound {
    pub use super::common_serverbound::*;
    use rapidbot_buf::{Decode, Encode};

    use super::KnownPack;
    use crate::packets::packet;

    /// `ServerboundFinishConfigurationPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct FinishConfiguration;
    packet!(FinishConfiguration, configuration, serverbound, FINISH_CONFIGURATION);

    /// `ServerboundSelectKnownPacks`: at most 64 entries.
    #[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
    pub struct SelectKnownPacks {
        pub packs: Vec<KnownPack>,
    }
    packet!(SelectKnownPacks, configuration, serverbound, SELECT_KNOWN_PACKS);

    /// `ServerboundAcceptCodeOfConductPacket`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
    pub struct AcceptCodeOfConduct;
    packet!(AcceptCodeOfConduct, configuration, serverbound, ACCEPT_CODE_OF_CONDUCT);
}
