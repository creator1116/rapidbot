//! Embedding rapidbot in another program: spawn a bot, feed it goals from
//! your own code, watch its events, stop it when you choose.
//!
//!     cargo run --release -p rapidbot --example embed -- localhost:25565 Steve

use std::sync::mpsc;

use rapidbot::prelude::*;

/// Walks wherever the owning program tells it to.
struct Remote {
    walker: Walker,
    goals: mpsc::Receiver<Vec3>,
}

impl Controller for Remote {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        // Controllers run on the bot's main thread: keep them quick and
        // talk to the rest of your program through channels.
        if let Ok(goal) = self.goals.try_recv() {
            self.walker.go_to(goal);
        }
        self.walker.tick(ctx);
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "localhost".into());
    let name = args.next().unwrap_or_else(|| "Steve".into());

    let (goal_tx, goals) = mpsc::channel();
    let config = ClientConfig::new(address, Account::Offline { name });
    let mut bot = Bot::spawn(
        config,
        Remote {
            walker: Walker::new(1),
            goals,
        },
    );

    while let Some(event) = bot.next_event().await {
        match event {
            BotEvent::Loaded => {
                // Your program decides what the bot does.
                let _ = goal_tx.send(Vec3::new(10.0, 0.0, 10.0));
            }
            BotEvent::Disconnected(reason) => println!("disconnected: {reason}"),
            other => println!("{other:?}"),
        }
    }
}
