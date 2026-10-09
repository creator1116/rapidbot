//! Running a bot from another program.
//!
//! ```no_run
//! use rapidbot_client::{Account, Bot, BotEvent, ClientConfig, Idle};
//!
//! # async fn demo() {
//! let config = ClientConfig::new("play.example.net", Account::Offline { name: "Steve".into() });
//! let mut bot = Bot::spawn(config, Idle);
//! while let Some(event) = bot.next_event().await {
//!     match event {
//!         BotEvent::Disconnected(reason) => println!("left: {reason}"),
//!         _ => {}
//!     }
//! }
//! # }
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{ClientConfig, ClientError, Controller};

/// What a running bot reports to its owner.
#[derive(Debug, Clone)]
pub enum BotEvent {
    /// Login finished; configuration is starting.
    LoggedIn { name: String, uuid: Uuid },
    /// The server put the player into a level (also after reconfiguration).
    Spawned { entity_id: i32, dimension: String },
    /// The world around the player has loaded and the bot can act.
    Loaded,
    /// The player died. With `ClientConfig::auto_respawn` the bot clicks
    /// "Respawn" after a human delay.
    Died,
    /// A player said something in chat (signed or not).
    Chat {
        sender: Uuid,
        name: Option<String>,
        text: String,
    },
    /// A server message: command feedback, broadcasts, plugin chat.
    SystemMessage { text: String, overlay: bool },
    /// The connection ended. Always the last event.
    Disconnected(String),
}

/// Channels the game loop uses to talk to the [`Bot`] handle.
pub(crate) struct BotLink {
    pub events: mpsc::UnboundedSender<BotEvent>,
    pub stop: Arc<AtomicBool>,
    /// Chat lines the owner asked for, picked up by the game loop.
    pub chat: Arc<Mutex<Vec<String>>>,
}

impl BotLink {
    pub fn emit(&self, event: BotEvent) {
        let _ = self.events.send(event);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// A bot running in the background. Dropping the handle does not stop it;
/// call [`disconnect`](Self::disconnect).
pub struct Bot {
    events: mpsc::UnboundedReceiver<BotEvent>,
    stop: Arc<AtomicBool>,
    chat: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<ClientError>,
}

impl Bot {
    /// Connects and plays on the current tokio runtime. The controller is
    /// called once per client tick on the bot's own main thread.
    pub fn spawn(config: ClientConfig, controller: impl Controller) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let chat = Arc::new(Mutex::new(Vec::new()));
        let link = BotLink {
            events: tx.clone(),
            stop: stop.clone(),
            chat: chat.clone(),
        };
        let task = tokio::spawn(async move {
            let reason = match crate::run_inner(config, Box::new(controller), link).await {
                Ok(reason) | Err(reason) => reason,
            };
            let _ = tx.send(BotEvent::Disconnected(reason.to_string()));
            reason
        });
        Self {
            events,
            stop,
            chat,
            task,
        }
    }

    /// The next event, or `None` once the bot has finished and all events
    /// have been read.
    pub async fn next_event(&mut self) -> Option<BotEvent> {
        self.events.recv().await
    }

    /// Types a line into chat: a message, or a command if it starts with
    /// `/`. The bot opens the chat box, types at human speed (standing
    /// still meanwhile, as the keys are busy) and presses Enter.
    pub fn chat(&self, line: impl Into<String>) {
        if let Ok(mut queue) = self.chat.lock() {
            queue.push(line.into());
        }
    }

    /// Asks the bot to close the connection; it stops within a frame.
    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Waits for the bot to finish and returns why.
    pub async fn wait(self) -> ClientError {
        self.task
            .await
            .unwrap_or_else(|e| ClientError::Protocol(format!("bot task failed: {e}")))
    }
}
