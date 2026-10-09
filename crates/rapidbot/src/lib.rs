//! rapidbot: a headless Minecraft Java Edition client framework modeling
//! vanilla protocol, timing, and gameplay behavior.
//!
//! This crate is the one to depend on. It re-exports the pieces:
//!
//! - [`Bot`], [`ClientConfig`], [`Account`]: connect and run a bot.
//! - [`Controller`] / [`TickContext`]: your logic, called once per client
//!   tick. Say where to look ([`TickContext::aim`]) and which keys to hold
//!   ([`TickContext::set_keys`]); input models produce timed input and
//!   vanilla-exact physics moves the player.
//! - [`nav::Walker`]: route finding and movement using modeled input.
//! - [`BotEvent`]: lifecycle, chat, and connection events.
//! - [`auth`]: Microsoft account login for online-mode servers.
//!
//! ```no_run
//! use rapidbot::prelude::*;
//!
//! struct Forward;
//!
//! impl Controller for Forward {
//!     fn tick(&mut self, ctx: &mut TickContext<'_>) {
//!         ctx.aim(90.0, 5.0, 3.0);
//!         ctx.set_keys(Keys { forward: true, ..Keys::default() });
//!     }
//! }
//!
//! # async fn demo() {
//! let config = ClientConfig::new("localhost:25565", Account::Offline { name: "Steve".into() });
//! let mut bot = Bot::spawn(config, Forward);
//! while let Some(event) = bot.next_event().await {
//! }
//! # }
//! ```

pub use rapidbot_auth as auth;
pub use rapidbot_client::{
    Account, Bot, BotEvent, ClientConfig, ClientError, Controller, DisplaySettings, Idle, Keys,
    MouseSettings, ResourcePackPolicy, TickContext, chat, combat, commands, controller, entities,
    interact, inventory, math, nav, path, run, run_with,
};
pub use rapidbot_human as human;
pub use rapidbot_physics as physics;
pub use rapidbot_protocol as protocol;
pub use rapidbot_world as world;

/// The names most bot code needs.
pub mod prelude {
    pub use crate::math::Vec3;
    pub use crate::nav::Walker;
    pub use crate::{
        Account, Bot, BotEvent, ClientConfig, ClientError, Controller, Keys, TickContext,
    };
}
