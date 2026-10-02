//! Digs the blocks given on the command line, in order, from where the bot
//! stands: aims at each, holds the attack button until it is gone.
//!
//!     cargo run --release -p rapidbot-client --example dig -- localhost:25565 Walker 94 100 100  94 101 100
//!
//! `RAPIDBOT_SLOT=0` selects that hotbar slot first (a pickaxe, say).

use rapidbot_client::interact::aim_point;
use rapidbot_client::world::Registry;
use rapidbot_client::{Account, ClientConfig, Controller, TickContext};

struct Digger {
    targets: Vec<(i32, i32, i32)>,
    slot: Option<u8>,
    started: Option<u64>,
    done: bool,
}

impl Controller for Digger {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        for event in ctx.events {
            tracing::warn!(severity = event.severity, "check: {:?}", event.kind);
        }
        if ctx.tick < 40 {
            return;
        }
        if let Some(slot) = self.slot {
            ctx.select_slot(slot);
            if ctx.inventory.selected != slot {
                return;
            }
        }
        let registry = Registry::get();
        let solid = |p: &(i32, i32, i32)| !registry.state(ctx.world.block_state_or_air(p.0, p.1, p.2)).is_air;
        let Some(target) = self.targets.iter().copied().find(solid) else {
            ctx.hold_attack(false);
            ctx.stop_aiming();
            if !self.done {
                self.done = true;
                tracing::info!("all blocks dug");
            }
            return;
        };
        let Some(point) = aim_point(ctx.player, ctx.world, target) else {
            ctx.hold_attack(false);
            if ctx.tick % 40 == 0 {
                let p = ctx.player.pos;
                tracing::warn!(?target, x = p.x, y = p.y, z = p.z, crosshair = ?ctx.crosshair, "cannot see the block from here");
            }
            return;
        };
        ctx.aim_at(point, 1.0);
        let on_target = !ctx.crosshair.miss && ctx.crosshair.pos == target;
        ctx.hold_attack(on_target);
        match (ctx.breaking, self.started) {
            (Some((pos, _)), None) if pos == target => {
                self.started = Some(ctx.tick);
                let held = ctx.inventory.held().map_or("nothing", |s| s.name());
                tracing::info!(?target, held, "digging");
            }
            (None, Some(since)) => {
                self.started = None;
                tracing::info!(ticks = ctx.tick - since, "block broken");
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "localhost".into());
    let name = args.next().unwrap_or_else(|| "Walker".into());
    let numbers: Vec<i32> = args.filter_map(|v| v.parse().ok()).collect();
    let controller = Digger {
        targets: numbers.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect(),
        slot: std::env::var("RAPIDBOT_SLOT").ok().and_then(|v| v.parse().ok()),
        started: None,
        done: false,
    };
    let reason = rapidbot_client::run_with(ClientConfig::new(address, Account::Offline { name }), controller).await;
    tracing::warn!("connection ended: {reason}");
}
