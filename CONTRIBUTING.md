# Contributing to rapidbot

Thanks for helping. rapidbot is a Minecraft Java Edition client written from
scratch in Rust; its one hard requirement is that it behaves like the vanilla
client. Most of the review effort goes into checking that.

By participating, you agree to follow the project's
[Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- Rust (stable, edition 2024), Python 3, and a JDK with `javac` for the data
  extractors.
- Run `python tools/fetch_vanilla.py 26.3` once. It downloads the vanilla
  jars and decompiles the client into `vanilla/` (gitignored). You will read
  that source constantly.
- Read [docs/architecture.md](docs/architecture.md) for how the crates fit
  together and [docs/porting.md](docs/porting.md) for how behaviour is ported
  and verified.

## The rules

1. **Port from vanilla source, don't guess.** Minecraft 26.3 is unobfuscated.
   Before implementing anything a server can observe, read the vanilla class
   that does it. Doc comments name the class or method being mirrored
   (`/// `MultiPlayerGameMode.attack`.`).
2. **Never commit Mojang's code.** Nothing under `vanilla/` goes into the
   repository, and ported code is our own implementation, not a paste.
   Generated facts (packet IDs, block data, test vectors) are fine.
3. **Keep Java's numbers.** `float` vs `double`, the `Mth` sine table,
   `Math.min`/`max` on `-0.0` and `NaN`, `(long)` casts. `0.42F` is
   `0.41999998688697815` as a double. Pin such values in tests.
4. **Match where and when, not only what.** Network thread vs main thread,
   per-frame vs per-tick work, the order of steps in `Minecraft.tick`, one
   TCP write per vanilla flush.
5. **Two layers, kept apart.** *Vanilla-exact* code is what the client does:
   ported, cited, pinned. *Input modeling* code (`crates/human`, `nav.rs`,
   `combat.rs`) converts intent into timed input. Bot logic uses that layer:
   aim with `TickContext::aim`, press keys with `set_keys`.
6. **No Azalea or mineflayer code.** The project is from scratch on purpose.
7. **It is a framework.** Anything a user needs must be reachable from the
   `rapidbot` crate. Examples demonstrate; they do not hold logic.

## Making a change

```sh
cargo test --workspace
cargo build --release --workspace --examples
```

- Add tests with the change. For ported behaviour the best test compares
  against the real thing: see "Checking against the JVM" in
  [docs/porting.md](docs/porting.md).
- Match the surrounding code: its naming, comment density and idiom.
- Keep commits focused; say in the message which vanilla behaviour changed.
- If you add or change something a server can observe, say in the pull
  request which vanilla source it follows and how you checked it.
- Live-test movement and interaction changes on a server you operate or have
  permission to test ([docs/testing.md](docs/testing.md)).

## Reporting bugs

The most useful report says what the vanilla client does, what rapidbot does
instead, and how you saw the difference (a packet capture, a server log, or
an alert from a server-side movement validator). Include the Minecraft and
server versions.

## Use

Run bots only on servers where you are allowed to. Do not open issues asking
for help bypassing a server's rules or enforcement.

## Licence

Contributions are accepted under the project's MIT licence.
