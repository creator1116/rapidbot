# Publishing

Which crates go to crates.io, in what order, and why the rest do not.

## The data boundary

Everything the extractors produce from the game is gitignored and excluded
from the packages ([data.md](data.md)). Four crates need that data to build —
`rapidbot-protocol`, `rapidbot-world`, `rapidbot-physics` and
`rapidbot-client` — so their packages cannot verify and they are marked
`publish = false`. `rapidbot` pulls in `rapidbot-client`, so it is marked
too. This is deliberate: that data is Mojang's, not ours to redistribute,
and a crate version can never be taken back.

Consumers use the git dependency instead, and generate the data themselves:

```toml
[dependencies]
rapidbot = { git = "https://github.com/creator1116/rapidbot" }
```

```sh
python tools/fetch_vanilla.py 26.3 --no-decompile
python tools/extract_blocks.py 26.3
python tools/extract_items.py 26.3
python tools/extract_lang.py 26.3
python tools/gen_clip_vectors.py 26.3
python tools/gen_packet_ids.py 26.3
```

## What is published

The data-free crates, in dependency order:

| Order | Crate | Depends on |
|---|---|---|
| 1 | `rapidbot-buf-derive` | — |
| 2 | `rapidbot-buf` | `rapidbot-buf-derive` |
| 3 | `rapidbot-nbt` | — |
| 4 | `rapidbot-auth` | `rapidbot-buf` |

Each publishes only after the crates before it are live, because the
packaged manifests reference them by version. Verify one locally without
touching the registry:

```sh
cargo package --offline --allow-dirty -p rapidbot-buf-derive
```

`--allow-dirty` only while the publishing changes themselves are
uncommitted. Then, with the crates.io token (`cargo login`):

```sh
cargo publish -p rapidbot-buf-derive
cargo publish -p rapidbot-buf
cargo publish -p rapidbot-nbt
cargo publish -p rapidbot-auth
```

Published versions are immutable: check `cargo package --list` and the
 dry run of everything you care about before the real one.
