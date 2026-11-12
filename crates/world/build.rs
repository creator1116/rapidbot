//! Stops the build with the command to run when the game data is missing.
//!
//! `data/blocks.json.gz` and `data/items.json.gz` are generated from your own
//! copy of Minecraft and are deliberately not committed or published: see
//! `docs/data.md`. Without this the failure is a `include_bytes!` error that
//! names a path and says nothing about what to do about it.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=data");

    let mut missing = Vec::new();
    for (file, generator) in [
        ("data/blocks.json.gz", "python tools/extract_blocks.py 26.3"),
        ("data/items.json.gz", "python tools/extract_items.py 26.3"),
    ] {
        if !root.join(file).exists() {
            missing.push(format!("  {file} — {generator}"));
        }
    }
    for file in ["tests/data/clip_vectors.txt", "tests/data/item_vectors.txt"] {
        if !root.join(file).exists() {
            println!(
                "cargo:warning={file} is missing, so the tests that read it will fail: \
                 run `python tools/gen_clip_vectors.py 26.3` and \
                 `python tools/extract_items.py 26.3`"
            );
        }
    }
    if !missing.is_empty() {
        panic!(
            "Mojang-derived data is missing:\n{}\n\
             Run `python tools/fetch_vanilla.py 26.3 --no-decompile` once, then the \
             generators above. The data comes from your own copy of the game: see \
             docs/data.md for why it is not in the repository.",
            missing.join("\n")
        );
    }
}
