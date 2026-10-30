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

`ctx.player.abilities.flying` is true when the player is in creative flight
mode.

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

### Direct input mode

To bypass the human input layer entirely, set `ClientConfig::input_mode` to
`InputMode::Direct`:

```rust
let mut config = ClientConfig::new("localhost:25565", account);
config.input_mode = InputMode::Direct;
```

Controller calls then take effect in the same tick: views snap, keys are
already held, clicks and hotbar changes are immediate, chat is sent without
typing, and containers close at once. Vanilla packet order and the screen,
container, death, loading, and chat-state gates are still applied. This mode is
for tests and debugging; it is deliberately observable as non-human input.
`InputMode::Human` is the default.

## Creative flight

Creative flight is supported when the server grants the `may_fly` ability
(in creative or spectator mode). The flight mechanics follow vanilla
Minecraft 1.26.3 exactly.

**Toggling flight:**

- Double-tap the jump key within a 7-tick window (0.35 seconds) to start
  flying.
- Double-tap again to land and return to walking mode.
- Spectators are automatically set to flying if they have the `may_fly`
  permission.

**Movement while flying:**

- Hold the jump key to climb upward.
- Hold the shift key to descend.
- Movement keys (forward, back, left, right) work as usual for horizontal
  movement.
- Sprint while flying to move faster horizontally.

**Landing:**

- Flight ends automatically when the player touches the ground (for
  non-spectators).
- The bot sends a `PlayerAbilities` packet to the server whenever flight
  state changes.

**Using flight in a bot:**

```rust
impl Controller for MyLogic {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        // Check if flying
        if ctx.player.abilities.flying {
            // Climb by holding jump
            ctx.set_keys(Keys { jump: true, forward: true, ..Default::default() });
        } else if ctx.player.abilities.may_fly {
            // Double-tap jump to start flying
            // (set jump: true for two consecutive ticks, then false)
        }
    }
}
```

The `crates/client/examples/fly.rs` example demonstrates a complete flight
sequence: double-tap to take off, climb, hover, sprint forward, descend, and
land.

## Ready-made behaviour

- `nav::Walker`: `go_to(point)`, then `tick(ctx)` every tick. It plans a
  route (around walls, up steps, down drops, clear of lava) and follows it
  using modeled input. Check `arrived()` and `gave_up()`.
- `combat::Fighter`: `set_target(Some(entity_id))`, then `tick(ctx)`. It
  approaches, keeps the crosshair on the target and clicks when the attack
  charge is back.

## Accounts

`Account::Offline { name }` works on offline-mode servers. For online-mode
servers sign in with Microsoft once; tokens are kept per alias in an
encrypted file, with its random key in the OS keyring:

```sh
cargo run -p rapidbot-auth -- login main
cargo run --release -p rapidbot-client --example join -- play.example.net --microsoft main
```

`rapidbot-auth refresh main` renews all cached tokens from the stored refresh
token without a device-code prompt; it is safe to run from a timer or another
background task. `rapidbot-auth status` shows the aliases and cached token
state, and `rapidbot-auth logout main` forgets one. `RAPIDBOT_ACCOUNTS` chooses
the directory (default `accounts`, which is gitignored).

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

## Chat verification

Player chat packets are checked against the sender's chat session from the
player list, including the RSA/SHA-256 signature, message index, and the
per-sender chain. A message that fails validation is not emitted as chat and
breaks that sender's chain until a new session is announced. Servers without
secure profiles can still use unsigned chat. The Mojang signature over the
profile key itself is not verified yet, so the client trusts the key the
server supplied for that player.

## Riding

`SetPassengers` is tracked for the local player. While mounted, the rider
follows the vehicle's interpolated position and seat attachment, and sends
`MovePlayerRot` instead of normal player position packets. If the local player
is the controlling passenger (for example the first passenger in a boat), the
client also echoes `MoveVehicle` in the vanilla order. Vehicle motion itself
remains server-driven: the framework does not simulate boat, horse, or other
vehicle physics locally.

## What is not there yet

Client-side vehicle physics, firework boosts, and riptide movement. See the
issue tracker.

## Use

Run bots only on servers where you are allowed to.
