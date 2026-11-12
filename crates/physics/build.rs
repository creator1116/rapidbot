//! Stops the build with the command to run when `src/mth_tables.rs` is
//! missing. It is generated from Minecraft's own `Mth`, so like the rest of
//! the game-derived data it is not committed or published: see `docs/data.md`.

use std::path::Path;

fn main() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mth_tables.rs");
    println!("cargo:rerun-if-changed={}", path.display());

    if !path.exists() {
        panic!(
            "src/mth_tables.rs is missing.\n\
             Run `python tools/fetch_vanilla.py 26.3 --no-decompile` once, then \
             `python tools/extract_blocks.py 26.3`, which writes it. The table is \
             generated from the game: see docs/data.md."
        );
    }
}
