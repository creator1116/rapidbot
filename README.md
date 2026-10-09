# rapidbot

A headless Minecraft Java Edition bot framework in Rust, built from scratch to
behave exactly like the vanilla client: byte-exact on the wire, tick-exact in
physics and timing.

Targets **Minecraft 26.3** (protocol 777).

## Layout

| Crate | Purpose |
|---|---|
| `rapidbot-buf` | Wire types (VarInt, strings, UUIDs), `Encode`/`Decode` traits |
| `rapidbot-buf-derive` | `#[derive(Encode, Decode)]` |
| `rapidbot-nbt` | Network NBT with Java's modified UTF-8 |
| `rapidbot-protocol` | Framing, compression, encryption, packet IDs, packets, async connection |
| `rapidbot-world` | Block data extracted from the jar, chunk storage, vanilla-exact block collision |
| `rapidbot-physics` | Player movement ported from vanilla: input, travel, collision, step-up, ground state |
| `rapidbot-human` | Input models: mouse strokes, response delays, fatigue |
| `rapidbot-auth` | Microsoft login (device code), Xbox/XSTS, Minecraft token and profile, chat keys, session join |
| `rapidbot` | The crate to depend on: re-exports the public API and a prelude |
| `rapidbot-recorder` | Records your own mouse and game-key input while you play, and fits a mouse profile to it |
| `rapidbot-client` | Address resolution, login, then a vanilla-style main thread (frames, ticks, packet queue) running configuration and play; `Controller` trait, route finding, modeled input, and player/entity state |

## Documentation

- [Developer guide](docs/guide.md): building a bot on the framework.
- [Architecture](docs/architecture.md): how the crates and threads fit together.
- [Porting from vanilla](docs/porting.md): how behaviour is ported and verified.
- [Testing](docs/testing.md): unit tests, generated vectors, live tests.
- [Contributing](CONTRIBUTING.md).

## Using it from another project

Depend on the `rapidbot` crate (path or git) and drive a bot from your own
code:

```rust
use rapidbot::prelude::*;

struct MyLogic { walker: Walker }

impl Controller for MyLogic {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        // Called once per client tick. Say where to look and what to hold;
        // input models and vanilla physics do the rest.
        if self.walker.arrived() {
            self.walker.go_to(Vec3::new(100.0, 64.0, -20.0));
        }
        self.walker.tick(ctx);
    }
}

# async fn demo() {
let config = ClientConfig::new("play.example.net", Account::Offline { name: "Steve".into() });
let mut bot = Bot::spawn(config, MyLogic { walker: Walker::new(1) });
while let Some(event) = bot.next_event().await {
    match event {
        BotEvent::Loaded => bot.chat("/home"), // uses modeled keystroke timing
        BotEvent::Chat { name, text, .. } => println!("<{name:?}> {text}"),
        _ => {}
    }
}
# }
```

The walker plans a route (around walls, up steps, down drops, clear of
lava) and follows it using modeled input. `ctx.chat(..)` types from inside a
controller. `ctx.inventory` is what the player carries and wears (a jump in
mid-air opens a worn elytra), `ctx.select_slot(n)` presses a hotbar key and
`ctx.close_container()` closes a screen the server opened. `ctx.crosshair` is
what the player is looking at; `ctx.hold_attack(true)` holds the left mouse
button, which digs that block at exactly the vanilla speed for the held tool
(`interact::aim_point` finds where to look to hit a given block).
`ctx.entities` is every entity where the client shows it, `ctx.target` the
one under the crosshair, and `ctx.click_attack()` clicks on it;
`combat::Fighter` provides aim, approach, and attack-timing behavior (see
the `fight` example).

Server resource packs are declined by default. Set
`config.resource_packs = ResourcePackPolicy::Accept` to answer the prompt
with "Yes": the pack is then really downloaded, with the same HTTP request
the Java client sends, because servers can check their pack host's log.

`crates/rapidbot/examples/embed.rs` is a complete example. Several bots can
run in one process; each has its own main thread.

## Development

```sh
python tools/fetch_vanilla.py 26.3   # vanilla jars, data reports, decompiled source → vanilla/ (gitignored)
python tools/gen_packet_ids.py 26.3  # regenerate crates/protocol/src/ids.rs
python tools/extract_blocks.py 26.3  # regenerate block data, attributes and Mth tables (needs javac)
python tools/extract_items.py 26.3   # regenerate item components and decoder test vectors
python tools/gen_clip_vectors.py 26.3  # regenerate ray cast test vectors
cargo test --workspace
cargo run -p rapidbot-protocol --example ping -- localhost 25565
cargo run --release -p rapidbot-client --example join -- localhost:25565 Steve   # offline-mode server, idles
cargo run --release -p rapidbot-client --example walk -- localhost:25565 Walker  # uses route finding
cargo run --release -p rapidbot-client --example chat -- localhost:25565 Walker "hello" "/msg Walker hi"  # types lines into chat
cargo run --release -p rapidbot-client --example course -- localhost:25565 Walker 130  # walks +x through water, ladders; glides when wearing an elytra
cargo run --release -p rapidbot-client --example dig -- localhost:25565 Walker 62 150 60  # digs the listed blocks
cargo run --release -p rapidbot-client --example fight -- localhost:25565 Walker minecraft:husk  # fights the nearest husk
cargo run -p rapidbot-auth --example login -- accounts/main.json            # Microsoft sign-in, cached (accounts/ is gitignored)
cargo run --release -p rapidbot-client --example join -- play.example.net --microsoft accounts/main.json
```

Run bots with `--release`: tick timing is part of what servers see, and
debug builds are too slow to keep it regular.

Mojang's code is unobfuscated but **not** open source. Use it as a reference,
write our own implementation, and never commit anything under `vanilla/`.

## Use and licence

Run bots only on servers where you are allowed to. See the
[Code of Conduct](CODE_OF_CONDUCT.md). MIT licensed: see [LICENSE](LICENSE).
