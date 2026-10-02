//! Walks a straight obstacle course in the +x direction: through water (it
//! holds jump to stay afloat), up ladders, and opening an elytra when it
//! finds itself falling while wearing one. For checking fluid,
//! climbing and gliding physics against a server and its anticheat.
//!
//!     cargo run --release -p rapidbot-client --example course -- localhost:25565 Walker 130
//!
//! The last argument is the x coordinate to stop at. `RAPIDBOT_SPRINT=1`
//! holds sprint: in water it then looks down and dives instead of floating,
//! and swims once its head is under. `RAPIDBOT_SLOT=3` selects that hotbar
//! slot two seconds in.

use rapidbot_client::Keys;
use rapidbot_client::{Account, ClientConfig, Controller, TickContext};

struct Course {
    stop_x: f64,
    sprint: bool,
    slot: Option<u8>,
    /// Ticks left to hold jump for opening the elytra.
    open_glider: u32,
    pitch: f32,
}

impl Controller for Course {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        for event in ctx.events {
            tracing::warn!(severity = event.severity, "check: {:?}", event.kind);
        }
        if ctx.tick == 40 {
            if let Some(slot) = self.slot {
                ctx.select_slot(slot);
            }
        }
        if ctx.inventory.container.is_some() {
            ctx.close_container();
        }
        let p = &*ctx.player;
        if ctx.tick % 100 == 0 {
            let held = ctx.inventory.held().map_or("nothing", |s| s.name());
            tracing::info!(slot = ctx.inventory.selected, held, can_glide = ctx.inventory.can_glide(), "inventory");
        }
        if ctx.tick % 20 == 0 {
            tracing::info!(
                x = p.pos.x,
                y = p.pos.y,
                z = p.pos.z,
                vy = p.delta_movement.y,
                water = p.in_water,
                swimming = p.swimming,
                gliding = p.fall_flying,
                on_ground = p.on_ground,
                "position"
            );
        }
        if p.pos.x >= self.stop_x {
            ctx.set_keys(Keys::default());
            ctx.stop_aiming();
            return;
        }
        let mut keys = Keys { forward: true, sprint: self.sprint, ..Keys::default() };
        // Afloat: a person holds space in water (unless diving to swim).
        // A swimmer pressed against the pool's edge needs it too.
        if p.in_water && (!self.sprint || p.horizontal_collision) {
            keys.jump = true;
        }
        // Falling with an elytra on: tap jump to open it.
        if ctx.inventory.can_glide() && !p.on_ground && !p.fall_flying && !p.in_water && p.fall_distance > 3.0 && self.open_glider == 0
        {
            self.open_glider = 4;
        }
        if self.open_glider > 0 {
            self.open_glider -= 1;
            keys.jump = self.open_glider > 0;
        }
        let pitch = if p.fall_flying {
            self.pitch
        } else if p.swimming {
            // Swim back up towards the surface.
            -8.0
        } else if p.in_water && self.sprint {
            35.0
        } else {
            5.0
        };
        ctx.set_keys(keys);
        ctx.aim(-90.0, pitch, 2.0);
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
    let stop_x = args.next().and_then(|v| v.parse().ok()).unwrap_or(130.0);
    let controller = Course {
        stop_x,
        sprint: std::env::var_os("RAPIDBOT_SPRINT").is_some(),
        slot: std::env::var("RAPIDBOT_SLOT").ok().and_then(|v| v.parse().ok()),
        open_glider: 0,
        pitch: std::env::var("RAPIDBOT_GLIDE_PITCH").ok().and_then(|v| v.parse().ok()).unwrap_or(12.0),
    };
    let reason = rapidbot_client::run_with(ClientConfig::new(address, Account::Offline { name }), controller).await;
    tracing::warn!("connection ended: {reason}");
}
