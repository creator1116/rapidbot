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
pub mod text;

use std::collections::HashMap;

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
pub use rapidbot_human as human;
pub use rapidbot_physics::{Keys, math};
pub use rapidbot_world as world;

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

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// As a player would type it into the server list: `host[:port]`.
    pub address: String,
    pub account: Account,
    pub information: ClientInformation,
    pub resource_packs: ResourcePackPolicy,
    pub display: DisplaySettings,
    pub mouse: MouseSettings,
    /// Seed for this bot's human traits (reaction speed, aim habits). By
    /// default it is derived from the account name, so the same account
    /// always behaves like the same person.
    pub human_seed: Option<u64>,
    /// Mouse habits fitted to a real player (from `rapidbot-recorder
    /// analyze`). Without one, traits are generated from the seed.
    pub mouse_profile: Option<rapidbot_human::MouseProfile>,
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
}

impl ClientConfig {
    pub fn new(address: impl Into<String>, account: Account) -> Self {
        Self {
            address: address.into(),
            account,
            information: ClientInformation::default(),
            resource_packs: ResourcePackPolicy::default(),
            display: DisplaySettings::default(),
            mouse: MouseSettings::default(),
            human_seed: None,
            mouse_profile: None,
            auto_respawn: true,
            code_of_conduct_accepted: false,
            pack_cache: None,
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
    #[error("server asked to transfer to {host}:{port} (not supported yet)")]
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
    pub registries: Vec<(Identifier, Vec<RegistryEntry>)>,
    pub enabled_features: Vec<Identifier>,
    /// Raw `ClientboundUpdateTagsPacket` bodies, decoded once the world model
    /// needs them.
    pub tags: Vec<Vec<u8>>,
    pub profile: Option<GameProfile>,
    pub session_id: Option<Uuid>,
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
    config: ClientConfig,
    controller: Box<dyn Controller>,
    link: bot::BotLink,
) -> Result<ClientError, ClientError> {
    // Unpack the embedded block and item data while logging in, so the
    // main thread does not stall on first use in the middle of play.
    std::thread::spawn(|| {
        rapidbot_world::Registry::get();
        rapidbot_world::item::ItemData::get();
    });
    let resolved = address::resolve(&config.address).await?;
    info!(address = %resolved.socket, host = %resolved.handshake_host, "connecting");
    let mut connection = Connection::connect(resolved.socket).await?;

    // Vanilla sends the intention and then Hello, each flushed.
    connection
        .send(&Intention {
            protocol_version: PROTOCOL_VERSION,
            host: resolved.handshake_host.as_str().into(),
            port: resolved.handshake_port,
            intent: Intent::Login,
        })
        .await?;

    let mut session = SessionData::default();
    let (profile, session_id) = login::login(&mut connection, &config, &mut session).await?;
    info!(name = %profile.name.0, uuid = %profile.id, "logged in");
    link.emit(BotEvent::LoggedIn {
        name: profile.name.0.clone(),
        uuid: profile.id,
    });
    session.profile = Some(profile);
    session.session_id = Some(session_id);

    let (reader, writer) = connection.into_split();
    let sender = net::spawn_writer(writer);
    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
    net::spawn_reader(reader, sender.clone(), inbound_tx);

    let game = game::Game::new(config, sender, inbound_rx, session, controller, link);
    let reason = tokio::task::spawn_blocking(move || game.run())
        .await
        .map_err(|e| ClientError::Protocol(format!("main thread panicked: {e}")))?;
    Ok(reason)
}
