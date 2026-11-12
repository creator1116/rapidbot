//! Stops the build with the command to run when the game data is missing: see
//! `docs/data.md`. `data/en_us.json.gz` is Mojang's language file, generated
//! from your own copy of the game, and is not committed or published.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=data");

    if !root.join("data/en_us.json.gz").exists() {
        panic!(
            "data/en_us.json.gz is missing.\n\
             Run `python tools/fetch_vanilla.py 26.3 --no-decompile` once, then \
             `python tools/extract_lang.py 26.3`. The file is Mojang's, taken from your \
             own copy of the game: see docs/data.md for why it is not in the repository."
        );
    }
}
