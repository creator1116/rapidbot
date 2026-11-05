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
| `rapidbot-world` | Block and item data extracted from the game, chunks and block entity NBT, tags, collision shapes, item stacks and components, block ray cast |
| `rapidbot-physics` | Player movement ported from vanilla, attributes, `Mth`/`Vec3` |
| `rapidbot-human` | Input models: mouse strokes, response delays, key timing, typing, fatigue; seeded per account |
| `rapidbot-auth` | Microsoft device code and silent token renewal → Xbox → XSTS → Minecraft token, encrypted per-alias token cache, chat keys, session join; `refresh_if_needed` for background renewal |
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
sees. `InputMode::Direct` is the explicit test/debug exception: it applies the
same intent in the current tick without human timing while keeping vanilla
packet order and gating.

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
- While the local player is a passenger, `positionRider` runs before the
  controller and the normal physics tick is skipped. `sendChanges` sends
  `MovePlayerRot` instead of a player position packet, plus `MoveVehicle`
  when the local player controls the root vehicle.

Tick regularity is visible to servers, so bots are run in release builds.

## Embedding: world snapshots

`World` hands out immutable views of its chunks, for an application that
renders or analyses the world from its own thread instead of reading every
column through `block_state` on the tick thread:

- `generation()` identifies the world. Every `World` takes a fresh identity in
  the process, and one is built whenever the level is replaced — a dimension
  change, or a reconfiguration, which drops the play state entirely. A
  same-dimension respawn keeps the level, as vanilla does; the chunks
  themselves still move their revisions as the server reloads them.
- `observation_id()` is a revision that moves whenever anything a consumer can
  see changes: a chunk loaded or unloaded, a block update, a block entity.
  Polling it is the cheap "has anything changed?" test.
- `chunk_snapshot(x, z)` returns `(revision, Arc<Chunk>)` for a loaded chunk,
  and `None` when it is not loaded, so an unloaded chunk stays
  distinguishable from one full of air. The `Arc` is immutable and needs no
  lock: the next update to that chunk copies it out from under the snapshot
  (`Arc::make_mut`), so a retained snapshot keeps the contents it was taken
  with and the tick thread is never blocked. A bot that takes no snapshots
  pays nothing for the copy-on-write storage.
- `chunk_snapshots_in(from, to)` returns the loaded chunks of a bounded chunk
  range without walking or copying the rest of the world.

Rendering, serialising and shipping that data are the application's job; the
world only answers what it has.

## Embedding: observing the wire

`ClientConfig::packet_observer` takes a `PacketObserver`, and every packet the
connection decodes or queues is reported once with its direction, protocol
state, id, generated name, body and stage. The hooks sit where the traffic
is: the login handshake runs on the un-split `Connection`, so its packets are
reported by the login path, while configuration and play — and a
reconfiguration, which moves the reader back a state — are reported by
`net.rs`'s reader and by `PacketSender::send`. Inbound packets are reported
once they are decoded and before anything acts on them; outbound ones when
they are queued, which is before the writer flushes them. With no observer
set, nothing is looked up and nothing is allocated.

## The configuration phase

Between login and play the server runs a configuration phase of its own: it
selects the known packs the client shares with it, sends every synchronized
registry (entries the selected packs already cover arrive without their data)
and the tags, and may push resource packs, transfer the client, or show a code
of conduct. `crates/client/src/game/configuration.rs` collects registries one
phase at a time and replaces a registry's entries if the same registry arrives
again, because vanilla builds a fresh collector for every phase. A
server-initiated transfer starts a fresh login and keeps only the cookie jar
(the one thing the server remembers between the two); a reconfiguration runs
the phase again on the same connection, keeping the state vanilla hands the
new listener (the cookie jar and the account) while collecting the registries
from scratch.

## Inside `rapidbot-client`

| File | Role |
|---|---|
| `game/mod.rs` | Frame loop and tick order |
| `game/configuration.rs`, `game/play.rs` | Packet handlers for the two states |
| `game/player.rs` | `LocalPlayer`: when and which movement packets are sent, and passenger/vehicle state |
| `controller.rs` | `Controller` trait and `TickContext` (aim, keys, clicks, chat) |
| `mouse.rs` | Vanilla sensitivity maths: mouse counts to rotation |
| `entities.rs` | Other entities: interpolation, sizes, entity data, tab list |
| `inventory.rs` | Inventory and server-opened containers; menu clicks and predictions |
| `interact.rs` | Crosshair pick, digging, block-change predictions, block placement and entity interaction, attacking |
| `chat.rs`, `commands.rs` | Chat sending and signing, last-seen tracking, the server's command tree |
| `packs.rs` | Resource pack prompt, download and status packets; best-effort `pack.mcmeta` validation, selected assets and `en_us` translations |
| `status.rs` | Server list ping: raw handshake address, SRV-resolved socket |
| `path.rs`, `nav.rs` | A* route finding; movement along routes using modeled input |
| `combat.rs` | Target approach, aiming, strafing/retreating, and attack timing |
| `entities.rs` | Tracks server-provided entity and player state; exposes snapshots, player-info metadata, and distance queries without interpreting roles |

`inventory.rs` models pickup, quick-move, swap and throw clicks. A
`HashedStack` prediction is only sent for stacks without a component patch;
patched stacks use the empty form, so the server sends authoritative slot
updates instead of suppressing them.

`packs.rs` reports `SUCCESSFULLY_LOADED` after a best-effort check of the
downloaded zip, its root `pack.mcmeta`, the `pack` object, the component
shape of `description`, and the top-level `features` and `overlays` sections.
It then keeps `assets/` and `data/` from the root and from the overlays whose
format ranges contain the client resource format (`97.1`); later overlays and
later packs override earlier ones. `assets/minecraft/lang/en_us.json` is
merged in that order and overrides the built-in `en_us` map embedded from the
client jar; both are used when rendering system chat and title components.
It does not compile selector or NBT paths, inspect every object/click/hover
payload, or interpret textures, models and sounds, so a pack that only fails
vanilla's real resource reload can still be reported as loaded. Download
batches are per-pack here; vanilla discards the rest of a download batch when
one request fails.

## Generated data

Facts about the game are extracted, not typed in:

- `tools/gen_packet_ids.py` → `crates/protocol/src/ids.rs` from the vanilla
  data reports.
- `tools/extract_blocks.py` → `crates/world/data/blocks.json.gz`: block
  states, shapes, attributes, entity types, and the `Mth` tables.
- `tools/extract_items.py` → `crates/world/data/items.json.gz`: item
  components.
- `tools/extract_lang.py` → `crates/client/data/en_us.json.gz`: the built-in
  language strings used for chat, titles and disconnect messages.

The block and item extractors compile small Java programs (`tools/extractor/`)
against the vanilla server jar and run them, so the data comes from the game's
own code; the language file is copied from the extracted client assets.
