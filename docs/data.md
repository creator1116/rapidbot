# Game data

Rapidbot needs a few tables that only the game has: block shapes and movement
properties, item component defaults, the English strings, the `Mth` lookup
tables, and the vectors its ports are tested against. They are generated from
**your own** copy of Minecraft and are deliberately not in this repository and
not in the published crates: that content is Mojang's, not ours to
redistribute, and a published crate version can never be taken back.

## Generating them

Java 26 is needed for the extractors (`java` and `javac` on `PATH`).

```sh
python tools/fetch_vanilla.py 26.3 --no-decompile   # the jar and its libraries
python tools/extract_blocks.py 26.3                 # crates/world/data/blocks.json.gz, crates/physics/src/mth_tables.rs
python tools/extract_items.py 26.3                  # crates/world/data/items.json.gz, the item vectors
python tools/extract_lang.py 26.3                   # crates/client/data/en_us.json.gz
python tools/gen_clip_vectors.py 26.3               # the ray cast vectors
```

The files land exactly where the crates expect them and are gitignored, so
`cargo build` works and `git status` stays clean. Continuous integration runs
these same steps before it builds anything.

## What happens without them

Each crate that needs game data has a build script that stops with the command
to run, instead of leaving you with an `include_bytes!` error that names a path
and nothing else. The vector files are only needed by the tests: the build
warns about them, and the tests that read them say what to run.

## Generated but kept in the tree

`crates/protocol/src/ids.rs` is also produced from the game's own report, but
it is a table of facts — packet IDs, directions and names — that the protocol
crate needs to compile, and `tools/gen_packet_ids.py` refreshes it on a
version bump. It stays in the repository.
