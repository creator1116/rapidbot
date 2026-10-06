# Architecture

rapidbot is a headless Minecraft Java Edition client (26.3, protocol 777)
split into small crates. The `rapidbot` crate re-exports what users need;
everything else is an implementation layer.

## Crates

| Crate | Purpose |
|---|---|
| `rapidbot` | Facade: `prelude`, `Bot`, `Controller`. The public API lives here and should stay stable and documented |
| `rapidbot-client` | Address resolution, login, then the main thread: frame loop, ticks, configuration and play packet handlers |
| `rapidbot-protocol` | Framing, compression, encryption, generated packet IDs (`ids.rs`), packets per connection state |
| `rapidbot-buf`, `rapidbot-buf-derive` | Wire types, `Encode`/`Decode`, `#[derive]` (`#[var]` marks VarInt fields) |
| `rapidbot-nbt` | Network NBT with Java's modified UTF-8 |
| `rapidbot-world` | Block and item data extracted from the game, chunks, tags, collision shapes, item stacks and components, block ray cast |
| `rapidbot-physics` | Player movement ported from vanilla, attributes, `Mth`/`Vec3` |
| `rapidbot-human` | Input models: mouse strokes, response delays, key timing, typing, fatigue; seeded per account |
| `rapidbot-auth` | Microsoft device code → Xbox → XSTS → Minecraft token, chat keys, session join |
| `rapidbot-recorder` | Windows raw-input recorder (game keys only, only while Minecraft is focused) and an analyser that fits a mouse profile |

## Two layers

**Vanilla-exact.** What the real client does, ported from its source and
pinned in tests: packet bytes, tick order, movement maths, dig speed, the
crosshair ray cast, entity interpolation.

**Input model.** How intent is translated into mouse, keyboard, and click
events over time, including reaction delays and movement along a route. This
lives in `crates/human` and in `nav.rs` / `combat.rs` of the client.

Bot logic never writes rotation or packets directly. It states intent ("look
there", "hold forward", "click"), the input model turns that into events over
the following frames, and the vanilla layer turns input into what the server
sees.

## Threads and timing

Like the vanilla client, a bot has network tasks and one main thread.

- `net.rs` reader and writer tasks decode frames and queue packets. A few
  packets are answered on the network side, as vanilla does.
- The main thread (`game/mod.rs`) runs frames. Each frame it drains the
  packet queue, polls input (the input models run here, per frame), and runs
  as many 50 ms ticks as are due.
- A tick follows `Minecraft.tick`: connection tick, crosshair pick, key
  bindings (the `Controller` runs here), player tick, position packets,
  other entities, end-of-tick packet.

Tick regularity is visible to servers, so bots are run in release builds.

## Inside `rapidbot-client`

| File | Role |
|---|---|
| `game/mod.rs` | Frame loop and tick order |
| `game/configuration.rs`, `game/play.rs` | Packet handlers for the two states |
| `game/player.rs` | `LocalPlayer`: when and which movement packets are sent |
| `controller.rs` | `Controller` trait and `TickContext` (aim, keys, clicks, chat) |
| `mouse.rs` | Vanilla sensitivity maths: mouse counts to rotation |
| `entities.rs` | Other entities: interpolation, sizes, entity data, tab list |
| `inventory.rs` | Inventory and server-opened containers |
| `interact.rs` | Crosshair pick, digging, block-change predictions, attacking |
| `chat.rs`, `commands.rs` | Chat sending and signing, last-seen tracking, the server's command tree |
| `packs.rs` | Resource pack prompt, download and status packets |
| `path.rs`, `nav.rs` | A* route finding; movement along routes using modeled input |
| `combat.rs` | Target approach, aiming, and attack timing |
| `entities.rs` | Tracks server-provided entity and player state; exposes snapshots, player-info metadata, and distance queries without interpreting roles |

## Generated data

Facts about the game are extracted, not typed in:

- `tools/gen_packet_ids.py` → `crates/protocol/src/ids.rs` from the vanilla
  data reports.
- `tools/extract_blocks.py` → `crates/world/data/blocks.json.gz`: block
  states, shapes, attributes, entity types, and the `Mth` tables.
- `tools/extract_items.py` → `crates/world/data/items.json.gz`: item
  components.

The extractors compile small Java programs (`tools/extractor/`) against the
vanilla server jar and run them, so the data comes from the game's own code.
