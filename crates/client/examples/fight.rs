//! Fights the nearest entity of a kind (or the nearest other player):
//! walks up, keeps the crosshair on it and clicks when the attack charge
//! is back.
//!
//!     cargo run --release -p rapidbot-client --example fight -- localhost:25565 Walker minecraft:zombie
//!     cargo run --release -p rapidbot-client --example fight -- localhost:25565 Walker player
//!
//! `RAPIDBOT_SLOT=0` selects that hotbar slot first (a sword, say).

use rapidbot_client::combat::{FightStatus, Fighter};
use rapidbot_client::{Account, ClientConfig, Controller, TickContext};

struct Brawler {
    kind: String,
    slot: Option<u8>,
    fighter: Fighter,
    swings: u32,
}

impl Controller for Brawler {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        if ctx.tick < 40 {
            return;
        }
        if let Some(slot) = self.slot {
            ctx.select_slot(slot);
            if ctx.inventory.selected != slot {
                return;
            }
        }
        if self.fighter.target().is_none() {
            let me = ctx.player.pos;
            let kind = if self.kind == "player" {
                "minecraft:player"
            } else {
                self.kind.as_str()
            };
            let nearest = ctx
                .entities
                .iter()
                .filter(|e| e.kind == kind && e.health.is_none_or(|h| h > 0.0))
                .map(|e| {
                    (
                        e.id,
                        (e.pos.x - me.x).powi(2)
                            + (e.pos.y - me.y).powi(2)
                            + (e.pos.z - me.z).powi(2),
                    )
                })
                .filter(|(_, d)| *d < 24.0 * 24.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((id, distance_sq)) = nearest {
                tracing::info!(id, distance = distance_sq.sqrt(), "target");
                self.fighter.set_target(Some(id));
            }
        }
        // A dead mob stays in the world for a second: stop hitting it.
        if self
            .fighter
            .target()
            .and_then(|id| ctx.entities.get(id))
            .is_some_and(|e| e.health.is_some_and(|h| h <= 0.0))
        {
            tracing::info!("target died");
            self.fighter.set_target(None);
        }
        let sprinting = ctx.player.sprinting;
        let speed = ctx.player.delta_movement.horizontal_distance_sqr().sqrt();
        if self.fighter.tick(ctx) == FightStatus::Swung {
            self.swings += 1;
            tracing::info!(
                swings = self.swings,
                charge = ctx.attack_strength,
                sprinting,
                speed,
                "swing"
            );
        }
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
    let name = args.next().unwrap_or_else(|| "Walker".into());
    let kind = args.next().unwrap_or_else(|| "minecraft:zombie".into());
    let controller = Brawler {
        kind,
        slot: std::env::var("RAPIDBOT_SLOT")
            .ok()
            .and_then(|v| v.parse().ok()),
        fighter: Fighter::new(name.bytes().map(u64::from).sum()),
        swings: 0,
    };
    let reason = rapidbot_client::run_with(
        ClientConfig::new(address, Account::Offline { name }),
        controller,
    )
    .await;
    tracing::warn!("connection ended: {reason}");
}
