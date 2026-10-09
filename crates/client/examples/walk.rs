//! Demonstrates route finding and timed input while wandering around spawn.
//!
//!     cargo run --release -p rapidbot-client --example walk -- localhost:25565 Walker [radius]
//!
//! Use on a local or explicitly authorized server to inspect movement and
//! route-finding behavior. Watch server logs for movement corrections.

use rapidbot_client::human::Noise;
use rapidbot_client::math::Vec3;
use rapidbot_client::nav::Walker;
use rapidbot_client::{Account, ClientConfig, Controller, TickContext};

struct Wander {
    walker: Walker,
    noise: Noise,
    home: Option<Vec3>,
    /// Ticks to stand around before the next walk.
    rest: u32,
    /// How far from the starting point to roam, in blocks.
    radius: f64,
}

impl Controller for Wander {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        if self.walker.arrived() {
            if self.rest > 0 {
                self.rest -= 1;
            } else {
                // Home is wherever the first walk starts from.
                let home = *self.home.get_or_insert(ctx.player.pos);
                let goal = Vec3::new(
                    home.x + self.noise.uniform(-self.radius, self.radius),
                    home.y,
                    home.z + self.noise.uniform(-self.radius, self.radius),
                );
                tracing::info!(x = goal.x, z = goal.z, "walking");
                self.walker.go_to(goal);
                // Next pause: usually a few seconds, sometimes much longer.
                self.rest = (self.noise.lognormal(4.0, 0.8) * 20.0) as u32;
            }
        }
        self.walker.tick(ctx);

        if ctx.tick % 100 == 0 && std::env::var_os("RAPIDBOT_DEBUG_BLOCKS").is_some() {
            // What the bot thinks is around it, for diagnosing stuck spots.
            let registry = rapidbot_client::world::Registry::get();
            let (bx, by, bz) = (
                ctx.player.pos.x.floor() as i32,
                ctx.player.pos.y.floor() as i32,
                ctx.player.pos.z.floor() as i32,
            );
            for y in (by - 1..=by + 2).rev() {
                let mut row = String::new();
                for z in bz - 1..=bz + 1 {
                    for x in bx - 1..=bx + 1 {
                        let state = ctx.world.block_state_or_air(x, y, z);
                        row.push_str(
                            registry
                                .block_of(state)
                                .name
                                .trim_start_matches("minecraft:"),
                        );
                        row.push(' ');
                    }
                    row.push_str("| ");
                }
                tracing::info!("y={y}: {row}");
            }
            let p = &ctx.player;
            tracing::info!(delta = ?p.delta_movement, hc = p.horizontal_collision, vc = p.vertical_collision, support = ?p.main_supporting_block, keys = ?p.held_keys, "state");
        }
        if ctx.tick % 100 == 0 {
            let p = &ctx.player;
            tracing::info!(
                x = p.pos.x,
                y = p.pos.y,
                z = p.pos.z,
                yaw = p.y_rot,
                pitch = p.x_rot,
                on_ground = p.on_ground,
                "position"
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
    let radius = args.next().and_then(|r| r.parse().ok()).unwrap_or(8.0);
    let mut walker = Walker::new(1);
    // RAPIDBOT_SPRINT_JUMP=1 forces the sprint-jumping habit, for testing.
    if std::env::var_os("RAPIDBOT_SPRINT_JUMP").is_some() {
        walker.sprint_jumps = true;
    }
    let controller = Wander {
        walker,
        noise: Noise::new(2),
        home: None,
        rest: 100,
        radius,
    };
    let reason = rapidbot_client::run_with(
        ClientConfig::new(address, Account::Offline { name }),
        controller,
    )
    .await;
    tracing::warn!("connection ended: {reason}");
}
