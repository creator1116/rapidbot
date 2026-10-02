//! Configuration state (`ClientConfigurationPacketListenerImpl`), main-thread
//! half. Keep-alives and disconnects are handled on the network side.

use rapidbot_buf::{ByteArray, Decode};
use rapidbot_protocol::packets::configuration::{KnownPack, clientbound as cb, serverbound as sb};
use rapidbot_protocol::{Direction, Packet, RawPacket, State};
use tracing::{debug, info, warn};

use super::Game;
use crate::ClientError;

/// Built-in packs a vanilla 26.3 client knows (`BuiltInPackSource` core pack
/// plus the feature packs in `data/minecraft/datapacks`).
pub const VANILLA_KNOWN_PACKS: &[(&str, &str, &str)] = &[
    ("minecraft", "core", rapidbot_protocol::VERSION_NAME),
    ("minecraft", "minecart_improvements", rapidbot_protocol::VERSION_NAME),
    ("minecraft", "redstone_experiments", rapidbot_protocol::VERSION_NAME),
    ("minecraft", "trade_rebalance", rapidbot_protocol::VERSION_NAME),
];

fn is_known(pack: &KnownPack) -> bool {
    VANILLA_KNOWN_PACKS
        .iter()
        .any(|&(ns, id, version)| pack.namespace == ns && pack.id == id && pack.version == version)
}

impl Game {
    pub(super) fn handle_configuration(&mut self, packet: RawPacket) -> Result<(), ClientError> {
        match packet.id {
            cb::Ping::ID => {
                let id = packet.decode::<cb::Ping>()?.id;
                self.net.send(&sb::Pong { id });
            }
            cb::CustomPayload::ID => {
                // Only minecraft:brand reaches the main thread.
                let payload = packet.decode::<cb::CustomPayload>()?;
                let brand = String::decode(&mut payload.data.0.as_slice())?;
                debug!(%brand, "server brand");
                self.session.server_brand = Some(brand);
            }
            cb::SelectKnownPacks::ID => {
                let offered = packet.decode::<cb::SelectKnownPacks>()?.packs;
                // KnownPacksManager.trySelectingPacks keeps the server's order.
                let packs: Vec<KnownPack> = offered.into_iter().filter(is_known).collect();
                debug!(count = packs.len(), "selecting known packs");
                self.net.send(&sb::SelectKnownPacks { packs });
            }
            cb::RegistryData::ID => {
                let data = packet.decode::<cb::RegistryData>()?;
                self.session.registries.push((data.registry, data.entries));
            }
            cb::UpdateTags::ID => {
                self.session.tags.push(packet.decode::<cb::UpdateTags>()?.data.0);
            }
            cb::UpdateEnabledFeatures::ID => {
                self.session.enabled_features = packet.decode::<cb::UpdateEnabledFeatures>()?.features;
            }
            cb::CookieRequest::ID => {
                let key = packet.decode::<cb::CookieRequest>()?.key;
                let payload = self.session.cookies.get(&key).cloned().map(ByteArray);
                self.net.send(&sb::CookieResponse { key, payload });
            }
            cb::StoreCookie::ID => {
                let cookie = packet.decode::<cb::StoreCookie>()?;
                self.session.cookies.insert(cookie.key, cookie.payload.0);
            }
            cb::ResourcePackPush::ID => {
                let push = packet.decode::<cb::ResourcePackPush>()?;
                self.resource_pack_push(push.id, &push.url, &push.hash.0, push.required)?;
            }
            rapidbot_protocol::ids::configuration::clientbound::RESOURCE_PACK_POP => {
                let id = Option::<uuid::Uuid>::decode(&mut packet.body.as_slice())?;
                if let Some(packs) = &mut self.packs {
                    packs.pop(id);
                }
            }
            cb::CodeOfConduct::ID => {
                if self.seen_code_of_conduct {
                    return Err(ClientError::Protocol("server sent duplicate code of conduct".into()));
                }
                self.seen_code_of_conduct = true;
                if self.config.code_of_conduct_accepted {
                    // serverData.hasAcceptedCodeOfConduct: no screen.
                    self.net.send(&sb::AcceptCodeOfConduct);
                } else {
                    // The screen shows the text with "Acknowledge" under
                    // it: a person at least skims it before clicking.
                    let length = packet.body.len() as f64;
                    let seconds = self.noise.lognormal(3.5, 0.5).clamp(1.2, 30.0) + (length * 0.004).min(12.0);
                    debug!(seconds, "reading the code of conduct");
                    self.conduct_click = Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds));
                }
            }
            cb::Transfer::ID => {
                let t = packet.decode::<cb::Transfer>()?;
                return Err(ClientError::Transfer { host: t.host, port: t.port });
            }
            cb::FinishConfiguration::ID => {
                self.net.send(&sb::FinishConfiguration);
                info!("configuration finished");
            }
            other => {
                // Reset chat, server links, report details, dialogs, post
                // effects: nothing to answer.
                if State::Configuration.packet_name(Direction::Clientbound, other).is_none() {
                    warn!(id = other, "unknown configuration packet");
                }
            }
        }
        Ok(())
    }
}
