//! Walks a straight line across a gap with the bridge mode chosen by an
//! environment variable, for testing the bridge modes against a live server.
//!
//!     cargo run --release -p rapidbot-client --example telebridge -- localhost:25599 Digger 16
//!     RAPIDBOT_BRIDGE=tele cargo run --release ...
//!
//! Use on a local or explicitly authorized server. `RAPIDBOT_BRIDGE` selects
//! `stop` (the default), `speed`, or `tele`.

use rapidbot_client::math::Vec3;
use rapidbot_client::nav::{Bridge, Walker};
use rapidbot_client::{Account, ClientConfig, Controller, TickContext};

struct Cross {
    walker: Walker,
    goal_x: f64,
    started: bool,
    done: bool,
}

impl Controller for Cross {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        if self.done {
            return;
        }
        if self.walker.arrived() {
            if !self.started {
                self.started = true;
                let home = ctx.player.pos;
                let goal = Vec3::new(self.goal_x, home.y, home.z);
                tracing::info!(goal = ?goal, "walking to");
                self.walker.go_to(goal);
            } else {
                tracing::info!(pos = ?ctx.player.pos, "arrived at the goal");
                self.done = true;
            }
        }
        if ctx.tick.is_multiple_of(40) {
            let p = &ctx.player;
            tracing::info!(
                x = p.pos.x,
                y = p.pos.y,
                z = p.pos.z,
                on_ground = p.on_ground,
                sprint = p.held_keys.sprint,
                sneak = p.held_keys.shift,
                jump = p.held_keys.jump,
                held = ctx.inventory.held().map(|stack| stack.name()),
                "cross"
            );
        }
        self.walker.tick(ctx);
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "localhost".into());
    let name = args.next().unwrap_or_else(|| "Digger".into());
    let goal_x = args
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(16.0);
    let mut walker = Walker::new(1);
    let mode = std::env::var("RAPIDBOT_BRIDGE").unwrap_or_default();
    walker.bridge = match mode.as_str() {
        "speed" => Bridge::Speed,
        "tele" => Bridge::Tele,
        _ => Bridge::Stop,
    };
    // The moving modes place blocks across a gap: plan placements with
    // stone, which the operator gives the bot out of band (`/give <name>
    // minecraft:stone 64` lands in the held hotbar slot of a fresh bot).
    if matches!(mode.as_str(), "speed" | "tele") {
        let stone = rapidbot_world::item::ItemData::get()
            .items
            .iter()
            .position(|item| item.name == "minecraft:stone")
            .expect("stone in the item registry") as u32;
        walker.place = rapidbot_client::path::PlacePolicy::for_item(stone);
    }
    tracing::info!(bridge = ?walker.bridge, "bridge mode");
    let controller = Cross {
        walker,
        goal_x,
        started: false,
        done: false,
    };
    let mut config = ClientConfig::new(address, Account::Offline { name });
    // The telebridge catch has a few ticks of legal window (vanilla refuses
    // a placement that would intersect the falling bot's box), so the hand
    // needs the fastest turn the sensitivity slider allows — the same
    // setting the telebridge simulations run with.
    config.mouse.sensitivity = 1.0;
    let reason = rapidbot_client::run_with(config, controller).await;
    tracing::warn!("connection ended: {reason}");
}
