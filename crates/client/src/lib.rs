//! The bot client.
//!
//! [`join`] takes a bot from nothing to the play state the way the vanilla
//! client does: address resolution, handshake, login, configuration.

pub mod address;
mod bot;
pub mod chat;
pub mod clock;
pub mod combat;
pub mod commands;
pub mod controller;
pub mod crypto;
pub mod door;
pub mod entities;
mod game;
pub mod interact;
pub mod inventory;
mod login;
pub mod mouse;
pub mod nav;
mod net;
mod packs;
pub mod path;
mod placement;
mod proxy;
pub mod status;
/// A harness for testing a controller outside the game loop, behind the
/// `test-support` feature.
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod text;
pub mod webshare;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rapidbot_buf::Identifier;
use rapidbot_protocol::packets::common::ClientInformation;
use rapidbot_protocol::packets::configuration::RegistryEntry;
use rapidbot_protocol::packets::handshake::{Intent, Intention};
use rapidbot_protocol::packets::login::GameProfile;
use rapidbot_protocol::{Connection, ConnectionError, PROTOCOL_VERSION};
use tracing::info;
use uuid::Uuid;

pub use bot::{Bot, BotEvent};
pub use controller::{Controller, Idle, TickContext};
pub use game::{DisplaySettings, LocalPlayer};
pub use mouse::MouseSettings;
pub use net::{ObservedPacket, PacketObserver, PacketStage};
pub use proxy::{Proxy, ProxyAuth, ProxyError};
pub use rapidbot_human as human;
pub use rapidbot_physics::{Keys, math};
pub use rapidbot_world as world;
pub use webshare::{WebshareClient, WebshareError, WebshareMode, WebshareProxy};

#[derive(Debug, Clone)]
pub enum Account {
    /// For servers in offline mode. Uses the offline UUID a cracked client
    /// would send.
    Offline { name: String },
    /// A Microsoft account, from [`rapidbot_auth::Authenticator::login`].
    Microsoft(rapidbot_auth::MinecraftAccount),
}

impl Account {
    pub fn name(&self) -> &str {
        match self {
            Account::Offline { name } => name,
            Account::Microsoft(account) => &account.name,
        }
    }

    pub fn uuid(&self) -> Uuid {
        match self {
            Account::Offline { name } => crypto::offline_uuid(name),
            Account::Microsoft(account) => account.id,
        }
    }

    pub fn chat_keys(&self) -> Option<&rapidbot_auth::ChatKeys> {
        match self {
            Account::Offline { .. } => None,
            Account::Microsoft(account) => account.chat_keys.as_ref(),
        }
    }
}

/// How controller instructions reach the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// The default: keys, mouse movement, clicks, hotbar keys, chat and
    /// container closes pass through the human input model and happen with
    /// human timing.
    #[default]
    Human,
    /// For tests and debugging: controller instructions are applied in the
    /// same tick without reaction, travel, typing or key-press delays. The
    /// vanilla packet order and screen/death/loading gates are still kept.
    /// Servers can see the difference, so do not use this to imitate a
    /// person.
    Direct,
}

/// What to do when a server pushes a resource pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourcePackPolicy {
    /// Like clicking "No" on the prompt. Vanilla disconnects itself if the
    /// pack is required.
    #[default]
    Decline,
    /// Like clicking "Yes"/"Proceed" on the prompt: the pack is really
    /// downloaded (with vanilla's request) and reported as loaded.
    Accept,
    /// As if the server were saved in the server list with resource packs
    /// enabled: accepted without a prompt.
    Enabled,
}

/// The async factory behind a [`ChatKeySource`]: call it for the next pair.
type RenewChatKeys = dyn Fn() -> Pin<Box<dyn Future<Output = Result<rapidbot_auth::ChatKeys, String>> + Send>>
    + Send
    + Sync;

/// Where a long-running session gets renewed chat-signing keys.
///
/// Vanilla fetches a new `ProfileKeyPair` once the announced one's
/// `refreshed_after` has passed (at most once an hour) and announces it with
/// `chat_session_update`; signatures stop being accepted when the key pair
/// expires. A bot that stays connected longer than that needs the same, so
/// [`ClientConfig::chat_key_renewer`] takes one of these.
///
/// [`Authenticator`](rapidbot_auth::Authenticator) is the usual source:
/// [`ChatKeySource::from_authenticator`] wraps one.
#[derive(Clone)]
pub struct ChatKeySource(Arc<RenewChatKeys>);

impl ChatKeySource {
    /// Builds a source from an async factory. The factory runs again for
    /// every renewal.
    pub fn new<F, Fut>(renew: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<rapidbot_auth::ChatKeys, String>> + Send + 'static,
    {
        Self(Arc::new(move || {
            Box::pin(renew()) as Pin<Box<dyn Future<Output = _> + Send>>
        }))
    }

    /// Renews through a shared [`Authenticator`](rapidbot_auth::Authenticator)
    /// with [`refresh_if_needed`](rapidbot_auth::Authenticator::refresh_if_needed),
    /// so tokens that are still fresh are not requested again.
    pub fn from_authenticator(
        authenticator: Arc<tokio::sync::Mutex<rapidbot_auth::Authenticator>>,
    ) -> Self {
        Self::new(move || {
            let authenticator = authenticator.clone();
            async move {
                let account = authenticator
                    .lock()
                    .await
                    .refresh_if_needed(rapidbot_auth::REFRESH_SKEW_SECS)
                    .await
                    .map_err(|e| e.to_string())?;
                account
                    .chat_keys
                    .ok_or_else(|| "the account has no chat keys".to_owned())
            }
        })
    }

    pub(crate) async fn renew(&self) -> Result<rapidbot_auth::ChatKeys, String> {
        (self.0)().await
    }
}

impl std::fmt::Debug for ChatKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatKeySource")
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// As a player would type it into the server list: `host[:port]`.
    pub address: String,
    pub account: Account,
    /// Optional SOCKS5 or HTTP CONNECT proxy for the game connection.
    pub proxy: Option<Proxy>,
    pub information: ClientInformation,
    pub resource_packs: ResourcePackPolicy,
    /// Human input timing, or direct instruction execution for tests and
    /// debugging. The default is [`InputMode::Human`].
    pub input_mode: InputMode,
    pub display: DisplaySettings,
    pub mouse: MouseSettings,
    /// Seed for this bot's human traits (reaction speed, aim habits). By
    /// default it is derived from the account name, so the same account
    /// always behaves like the same person.
    pub human_seed: Option<u64>,
    /// Mouse habits fitted to a real player (from `rapidbot-recorder
    /// analyze`). Without one, traits are generated from the seed.
    pub mouse_profile: Option<rapidbot_human::MouseProfile>,
    /// Renews the account's chat-signing keys while a session runs; `None`
    /// keeps the keys the account was created with, which is enough for a
    /// session shorter than the key pair's lifetime.
    pub chat_key_renewer: Option<ChatKeySource>,
    /// Mojang's `PROFILE_KEY` service keys
    /// ([`Authenticator::services_keys`](rapidbot_auth::Authenticator::services_keys)).
    /// With them, a player's announced chat key is accepted only when Mojang
    /// signed it; without them the key is trusted, which is a documented
    /// approximation of vanilla (which fetches the keys at startup and drops
    /// every announcement it cannot check).
    pub profile_keys: Option<rapidbot_auth::ServicesKeySet>,
    /// Click "Respawn" on the death screen after a human delay.
    pub auto_respawn: bool,
    /// The server's code of conduct was accepted on an earlier visit
    /// (vanilla remembers per saved server): acknowledge it at once instead
    /// of after reading it.
    pub code_of_conduct_accepted: bool,
    /// Where to remember downloaded resource packs (as vanilla's
    /// `downloads` folder does), so a pack with a known hash is not fetched
    /// again on the next join. `None`: every join downloads, like a fresh
    /// installation.
    pub pack_cache: Option<std::path::PathBuf>,
    /// Called for every packet this connection decodes or queues, in login,
    /// configuration and play (a reconfiguration included). `None` (the
    /// default) costs nothing. See [`net::PacketObserver`]: the callback runs
    /// on the connection's threads, so it must not block, and bodies may
    /// contain sensitive data.
    pub packet_observer: Option<net::PacketObserver>,
    /// What the observer reports as this connection's id, so an application
    /// running several bots can tell them apart. Defaults to 0.
    pub connection_id: u64,
}

impl ClientConfig {
    pub fn new(address: impl Into<String>, account: Account) -> Self {
        Self {
            address: address.into(),
            account,
            proxy: None,
            information: ClientInformation::default(),
            resource_packs: ResourcePackPolicy::default(),
            input_mode: InputMode::default(),
            display: DisplaySettings::default(),
            mouse: MouseSettings::default(),
            human_seed: None,
            mouse_profile: None,
            chat_key_renewer: None,
            profile_keys: None,
            auto_respawn: true,
            code_of_conduct_accepted: false,
            pack_cache: None,
            packet_observer: None,
            connection_id: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Resolve(#[from] address::ResolveError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("disconnected: {0}")]
    Disconnected(String),
    #[error("server requires authentication, which an offline account cannot do")]
    AuthenticationRequired,
    #[error("session join failed: {0}")]
    SessionJoin(#[from] rapidbot_auth::JoinError),
    #[error("encryption failed: {0}")]
    Encryption(String),
    #[error("server requires a resource pack and the policy declines it")]
    ResourcePackRequired,
    #[error("server transferred the client to {host}:{port}")]
    Transfer { host: String, port: i32 },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("stopped by the owner")]
    Stopped,
}

impl From<rapidbot_buf::DecodeError> for ClientError {
    fn from(e: rapidbot_buf::DecodeError) -> Self {
        ClientError::Connection(e.into())
    }
}

/// State carried from login and configuration into play.
#[derive(Debug, Default)]
pub struct SessionData {
    pub server_brand: Option<String>,
    /// `ClientboundStoreCookiePacket` values, answered on request.
    pub cookies: HashMap<Identifier, Vec<u8>>,
    /// One entry per synchronized registry, in the order collected.
    pub registries: Vec<(Identifier, Vec<RegistryEntry>)>,
    pub enabled_features: Vec<Identifier>,
    /// Raw `ClientboundUpdateTagsPacket` bodies, decoded once the world model
    /// needs them.
    pub tags: Vec<Vec<u8>>,
    pub profile: Option<GameProfile>,
    pub session_id: Option<Uuid>,
}

impl SessionData {
    /// `RegistryDataCollector.appendContents`: a registry's contents arrive in
    /// one packet, so a later packet for the same registry replaces what this
    /// session collected before. Vanilla builds a fresh collector for every
    /// configuration phase, including the one `/debugconfig` starts; keeping
    /// one entry per registry gives that behaviour across a reconfiguration
    /// instead of counting each registry twice.
    pub fn set_registry(&mut self, registry: Identifier, entries: Vec<RegistryEntry>) {
        match self
            .registries
            .iter_mut()
            .find(|(name, _)| *name == registry)
        {
            Some((_, existing)) => *existing = entries,
            None => self.registries.push((registry, entries)),
        }
    }
}

/// Connects, logs in and idles until disconnected. Returns why the
/// connection ended.
pub async fn run(config: ClientConfig) -> ClientError {
    run_with(config, Idle).await
}

/// Runs a bot to completion with a [`Controller`] deciding what the player
/// does. For events and a way to stop it, use [`Bot::spawn`] instead.
pub async fn run_with(config: ClientConfig, controller: impl Controller) -> ClientError {
    Bot::spawn(config, controller).wait().await
}

/// Login runs here; configuration and play run on a dedicated main thread,
/// like the vanilla client's, with the network side on tokio.
async fn run_inner(
    mut config: ClientConfig,
    mut controller: Box<dyn Controller>,
    mut link: bot::BotLink,
) -> Result<ClientError, ClientError> {
    // Unpack the embedded block and item data while logging in, so the
    // main thread does not stall on first use in the middle of play.
    std::thread::spawn(|| {
        rapidbot_world::Registry::get();
        rapidbot_world::item::ItemData::get();
    });

    let mut session = SessionData::default();
    loop {
        if link.stopped() {
            return Ok(ClientError::Stopped);
        }
        let resolved = address::resolve(&config.address).await?;
        info!(address = %resolved.socket, host = %resolved.handshake_host, "connecting");
        let mut connection = match &config.proxy {
            Some(proxy) => {
                info!(?proxy, "connecting through proxy");
                Connection::from_stream(proxy::connect(proxy, resolved.socket).await?)?
            }
            None => Connection::connect(resolved.socket).await?,
        };

        // The login handshake runs on the un-split connection, so its
        // packets are reported here and by `login` rather than by the
        // reader and writer tasks.
        let observer = net::Observer::new(config.connection_id, config.packet_observer.clone());

        // Vanilla sends the intention and then Hello, each flushed.
        login::send_observed(
            &mut connection,
            &observer,
            &Intention {
                protocol_version: PROTOCOL_VERSION,
                host: resolved.handshake_host.as_str().into(),
                port: resolved.handshake_port,
                intent: Intent::Login,
            },
        )
        .await?;

        let (profile, session_id) =
            login::login(&mut connection, &config, &mut session, &observer).await?;
        info!(name = %profile.name.0, uuid = %profile.id, "logged in");
        link.emit(BotEvent::LoggedIn {
            name: profile.name.0.clone(),
            uuid: profile.id,
        });
        session.profile = Some(profile);
        session.session_id = Some(session_id);

        let (reader, writer) = connection.into_split();
        let (sender, writer_task) = net::spawn_writer(writer, observer);
        let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
        let reader_task = net::spawn_reader(reader, sender.clone(), inbound_tx);

        let game = game::Game::new(
            config,
            sender,
            inbound_rx,
            session,
            controller,
            link,
            tokio::runtime::Handle::current(),
        );
        let outcome = match tokio::task::spawn_blocking(move || game.run()).await {
            Ok(outcome) => outcome,
            Err(e) => {
                reader_task.abort();
                writer_task.abort();
                return Err(ClientError::Protocol(format!("main thread panicked: {e}")));
            }
        };
        // The bot is done with this connection: close both halves now rather
        // than waiting for the 30-second read timeout. A transfer opens a
        // fresh connection on the next loop iteration.
        reader_task.abort();
        writer_task.abort();

        let game::GameOutcome {
            reason,
            config: mut next_config,
            session: next_session,
            controller: next_controller,
            link: next_link,
        } = outcome;
        match reason {
            // handleTransfer: disconnect, keep the cookie jar (the only
            // per-server state the server can see) and start a fresh login at
            // the new address. Vanilla's TransferState also carries the
            // UI-only seenPlayers and seenInsecureChatWarning; those are not
            // modelled here.
            ClientError::Transfer { host, port } => {
                next_config.address = transfer_address(&host, port)?;
                info!(address = %next_config.address, "transferring");
                config = next_config;
                session = SessionData {
                    cookies: next_session.cookies,
                    ..SessionData::default()
                };
                controller = next_controller;
                link = next_link;
            }
            reason => return Ok(reason),
        }
    }
}

/// `new ServerAddress(packet.host(), packet.port())` rendered in the form
/// [`address::parse_address`] accepts. The server can send an IPv6 literal
/// without brackets.
fn transfer_address(host: &str, port: i32) -> Result<String, ClientError> {
    let port = u16::try_from(port)
        .map_err(|_| ClientError::Protocol(format!("transfer to invalid port {port}")))?;
    if host.is_empty() {
        return Err(ClientError::Protocol(
            "transfer to an empty host".to_string(),
        ));
    }
    let host = if host.starts_with('[') || !host.contains(':') {
        host.to_owned()
    } else {
        format!("[{host}]")
    };
    Ok(format!("{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::{Account, ClientConfig, InputMode, SessionData, transfer_address};
    use rapidbot_buf::Identifier;
    use rapidbot_protocol::packets::configuration::RegistryEntry;

    #[test]
    fn direct_input_is_opt_in() {
        let config = ClientConfig::new(
            "localhost:25565",
            Account::Offline {
                name: "Steve".into(),
            },
        );
        assert_eq!(config.input_mode, InputMode::Human);
    }

    /// `RegistryDataCollector` is per configuration phase, so a second phase
    /// (a reconfiguration) must not leave the first phase's entries counted
    /// again: the chunk reader takes the registry length from this list.
    #[test]
    fn configuration_phases_replace_registry_contents() {
        let entry = |name: &str| RegistryEntry {
            id: Identifier::from(name),
            data: None,
        };
        let biome = Identifier::from("minecraft:worldgen/biome");
        let mut session = SessionData::default();
        session.set_registry(
            biome.clone(),
            vec![entry("minecraft:plains"), entry("minecraft:desert")],
        );
        // A reconfiguration re-sends every synchronized registry.
        session.set_registry(
            biome.clone(),
            vec![entry("minecraft:plains"), entry("minecraft:desert")],
        );
        assert_eq!(session.registries.len(), 1);
        assert_eq!(session.registries[0].1.len(), 2);

        // Other registries collected in the same phase stay separate.
        session.set_registry(
            Identifier::from("minecraft:dimension_type"),
            vec![entry("minecraft:overworld")],
        );
        assert_eq!(session.registries.len(), 2);
        assert_eq!(
            session.registries[1].0.normalized(),
            "minecraft:dimension_type"
        );
        assert_eq!(session.registries[1].1.len(), 1);
    }

    #[test]
    fn transfer_addresses() {
        assert_eq!(
            transfer_address("play.example.net", 25565).unwrap(),
            "play.example.net:25565"
        );
        assert_eq!(transfer_address("::1", 25565).unwrap(), "[::1]:25565");
        assert_eq!(transfer_address("[::1]", 25565).unwrap(), "[::1]:25565");
        assert!(transfer_address("", 25565).is_err());
        assert!(transfer_address("host", -1).is_err());
        assert!(transfer_address("host", 65536).is_err());
    }
}
