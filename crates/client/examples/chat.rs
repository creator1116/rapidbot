//! Joins, types the lines given on the command line into chat (messages,
//! or commands when they start with `/`), and prints what comes back.
//!
//!     cargo run --release -p rapidbot-client --example chat -- localhost:25565 Walker "hello" "/msg Walker hi"

use rapidbot_client::{Account, Bot, BotEvent, ClientConfig, Idle, ResourcePackPolicy};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "localhost".into());
    let name = args.next().unwrap_or_else(|| "Walker".into());
    let lines: Vec<String> = args.collect();

    let mut config = ClientConfig::new(address, Account::Offline { name });
    // RAPIDBOT_PACKS=accept answers the resource pack prompt with "Yes";
    // =enabled skips the prompt, like a saved server with packs enabled.
    config.resource_packs = match std::env::var("RAPIDBOT_PACKS").as_deref() {
        Ok("accept") => ResourcePackPolicy::Accept,
        Ok("enabled") => ResourcePackPolicy::Enabled,
        _ => ResourcePackPolicy::Decline,
    };
    let mut bot = Bot::spawn(config, Idle);
    let started = std::time::Instant::now();
    while let Some(event) = bot.next_event().await {
        match event {
            BotEvent::Loaded => {
                for line in &lines {
                    bot.chat(line.clone());
                }
            }
            BotEvent::Chat { name, text, .. } => {
                println!("[{:6.1}s] <{}> {text}", started.elapsed().as_secs_f64(), name.as_deref().unwrap_or("?"));
            }
            BotEvent::SystemMessage { text, overlay: false } => {
                println!("[{:6.1}s] {text}", started.elapsed().as_secs_f64());
            }
            BotEvent::Disconnected(reason) => println!("disconnected: {reason}"),
            _ => {}
        }
    }
}
