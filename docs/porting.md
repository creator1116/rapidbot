# Porting from vanilla

Everything a server can observe is ported from the vanilla 26.3 client. This
page is how to do that well.

## Get the source

```sh
python tools/fetch_vanilla.py 26.3
```

This fills `vanilla/26.3/` with the jars, the data reports and the
decompiled client under `vanilla/26.3/src/`. The directory is gitignored and
must stay that way: Mojang's code is readable, not open source.

Searching the whole tree from a shell is slow (assets are in there). Search
under `vanilla/26.3/src/net/minecraft/...`.

## Find all of the behaviour

A feature is rarely in one method. For "left click on an entity" the path is
`Minecraft.handleKeybinds` → `startAttack` → `MultiPlayerGameMode.attack` →
`Player.attack`, plus `Player.tick` for the attack charge. Follow it end to
end and note:

- which packets go out, in what order, on which tick;
- what client state changes (it may change movement on the same tick);
- what runs only on the server (`level() instanceof ServerLevel`) and so
  must *not* be ported;
- overrides in subclasses (`LocalPlayer`, `RemotePlayer`).

## Keep Java's arithmetic

Results must match bit for bit, because physics errors accumulate and
servers predict movement.

- `float` and `double` are different: do float maths in `f32` and widen
  where Java widens. `0.42F` as a double is `0.41999998688697815`.
- `Mth.sin`/`cos` use a lookup table: use `rapidbot_physics::math`.
- `Math.min`/`max` treat `-0.0` and `NaN` specially: use `rapidbot_world::aabb::java_min` and
  friends.
- `(int)` and `(long)` casts truncate and saturate; `Mth.floor` floors.
- Keep the order of operations; do not simplify expressions.

## Keep the timing

Note where the vanilla code runs: on the network thread or the main thread,
per frame or per tick, and where in `Minecraft.tick`. A packet sent one tick
early is as visible as a wrong byte.

## Cite it

Doc comments name what is mirrored:

```rust
/// `Player.getAttackStrengthScale`: the attack charge, 0 to 1.
```

Where we knowingly differ or leave something out, say so in a comment at
that spot.

## Checking against the JVM

The strongest test runs the vanilla code and compares outputs. The pattern,
used by `tools/extract_blocks.py`, `tools/extract_items.py` and
`tools/gen_clip_vectors.py`:

1. Write a small Java class in `tools/extractor/` that calls the vanilla
   code with many inputs and prints inputs and outputs.
2. A Python script compiles it against
   `vanilla/26.3/versions/26.3/server-26.3.jar` plus `libraries/`, runs it,
   and writes the result under `crates/*/tests/data/`.
3. A Rust test reads the vectors and requires identical results.

Examples: the block ray cast matches the JVM on 4,000 random rays
(`crates/world/tests/raycast.rs`); item stacks encoded by vanilla decode
exactly (`crates/world/tests/items.rs`).

When that is not practical, pin hand-derived values in a unit test and say
where they come from.

## Input modeling is separate

If what you are writing describes modeled input (mouse movement, key timing,
or navigation) rather than vanilla client behavior, it belongs in
`crates/human` or the client's `nav.rs` / `combat.rs`. It is not cited to
vanilla, and should take its randomness from the bot's seeded `Noise` so
each account behaves consistently.
