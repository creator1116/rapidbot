//! Stops the build with the command to run when `src/ids.rs` is missing. It is
//! generated from the game's own packet report, so like the rest of the
//! game-derived files it is not committed or published: see `docs/data.md`.

use std::path::Path;

fn main() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ids.rs");
    println!("cargo:rerun-if-changed={}", path.display());

    if !path.exists() {
        panic!(
            "src/ids.rs is missing.\n\
             Run `python tools/fetch_vanilla.py 26.3 --no-decompile` once, then \
             `python tools/gen_packet_ids.py 26.3`, which writes it. The table is \
             generated from the game: see docs/data.md."
        );
    }
}
