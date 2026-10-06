# Developer guide

How to build a bot on rapidbot. For the internals see
[architecture.md](architecture.md).

## Add it to a project

```toml
[dependencies]
rapidbot = { git = "https://github.com/creator1116/rapidbot" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Build and run with `--release`: tick timing is part of what a server sees.

## A first bot

```rust
use rapidbot::prelude::*;

struct MyLogic { walker: Walker }

impl Controller for MyLogic {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        if self.walker.arrived() {
            self.walker.go_to(Vec3::new(100.0, 64.0, -20.0));
        }
        self.walker.tick(ctx);
    }
}

#[tokio::main]
async fn main() {
    let config = ClientConfig::new("localhost:25565", Account::Offline { name: "Steve".into() });
    let mut bot = Bot::spawn(config, MyLogic { walker: Walker::new(1) });
    while let Some(event) = bot.next_event().await {
        match event {
            BotEvent::Loaded => bot.chat("hello"),
            BotEvent::Chat { name, text, .. } => println!("<{name:?}> {text}"),
            BotEvent::Disconnected(reason) => println!("left: {reason}"),
            _ => {}
        }
    }
}
```

`crates/rapidbot/examples/embed.rs` is a complete program. Several bots can
run in one process.

## The two halves

**`Bot`** is the handle your async code holds: `next_event()`, `chat(..)`,
`disconnect()`, `wait()`.

**`Controller`** is your logic inside the game. Its `tick` is called once
per client tick (20 per second) at the point where the real client reads the
keyboard and mouse. It gets a `TickContext`.

## What a controller can see

| Field | Meaning |
|---|---|
| `ctx.player` | Position, velocity, rotation, on-ground, sprinting, pose |
| `ctx.world` | Loaded chunks: `block_state_or_air(x, y, z)` |
| `ctx.entities` | Other entities where the client draws them, with size, pose and health |
| `ctx.players` | Other players within 32 blocks, nearest first |
| `ctx.inventory` | Items carried and worn, the selected slot, any open container |
| `ctx.crosshair` | The block under the crosshair, or a miss |
| `ctx.target` | The entity under the crosshair, within reach |
| `ctx.breaking` | The block being dug and its progress |
| `ctx.attack_strength` | The attack charge, 0 to 1 |
| `ctx.chat_open` | True while a chat line is being typed |

## What a controller can do

A controller states intent. The input model turns it into timed mouse,
keyboard, and click events; the vanilla simulation processes those events.

| Call | Effect |
|---|---|
| `ctx.aim(yaw, pitch, tolerance)` / `ctx.aim_at(point, tolerance)` | The mouse turns the view there over the next frames |
| `ctx.stop_aiming()` | The hand rests |
| `ctx.set_keys(Keys { forward: true, .. })` | Movement keys to hold; fingers follow within tens of milliseconds |
| `ctx.select_slot(n)` | Presses hotbar key `n` |
| `ctx.hold_attack(bool)` | Holds or releases the left mouse button: digs the block under the crosshair |
| `ctx.click_attack()` | One left click: attacks the entity under the crosshair |
| `ctx.chat(line)` | Types a message or `/command` at typing speed |
| `ctx.close_container()` | Closes a screen the server opened |

Things act on the crosshair, as in the game, so aim first. Helpers find
where to look: `interact::aim_point` for a block,
`ctx.entity_aim_point(id, height)` for an entity.

`ctx.snap_look` sets the view instantly and bypasses the modeled mouse-input
path; reserve it for tests.

## Ready-made behaviour

- `nav::Walker`: `go_to(point)`, then `tick(ctx)` every tick. It plans a
  route (around walls, up steps, down drops, clear of lava) and follows it
  using modeled input. Check `arrived()` and `gave_up()`.
- `combat::Fighter`: `set_target(Some(entity_id))`, then `tick(ctx)`. It
  approaches, keeps the crosshair on the target and clicks when the attack
  charge is back.

## Accounts

`Account::Offline { name }` works on offline-mode servers. For online-mode
servers sign in with Microsoft once; the tokens are cached in a file:

```sh
cargo run -p rapidbot-auth --example login -- accounts/main.json
cargo run --release -p rapidbot-client --example join -- play.example.net --microsoft accounts/main.json
```

Keep that file private (`accounts/` is gitignored).

## Configuration

`ClientConfig` fields worth knowing:

- `resource_packs`: `Decline` (default) or `Accept`. Accepted packs are
  really downloaded, with the request the Java client sends.
- `human_seed`: seeds input-model randomness. One seed per account keeps
  its generated input profile consistent between sessions.
- `mouse_profile`: a profile fitted from your own play by
  `rapidbot-recorder`.
- `mouse`, `display`, `information`: sensitivity, frame rate and the client
  settings the server is told about.
- `auto_respawn`: click "Respawn" after dying.

## Player and entity data

`ctx.players` contains tracked player entities within
`entities::NEARBY_PLAYER_RADIUS` (32 blocks), nearest
first. Each `NearbyPlayer` includes its entity ID, UUID, interpolated
position, distance, and optional `PlayerInfo` received from the server's
player list (name, listed state, game mode, and latency). Missing player
information means the server has not provided a current player-list entry;
it is not a role or identity classification.

For a different radius or players outside the default range, use
`ctx.entities.players_near(ctx.player.pos, radius, ctx.own_id)`. Use
`ctx.entities.players()` to iterate every player entity currently tracked by
the client. The framework exposes state; application-specific meaning and
policy are left to the package user.

## What is not there yet

Placing blocks, using items (eating, bows, shields), clicking inside
containers, vehicles, swimming and ladders in the route planner. See the
issue tracker.

## Use

Run bots only on servers where you are allowed to.
