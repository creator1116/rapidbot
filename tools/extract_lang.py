#!/usr/bin/env python3
"""Copy the vanilla en_us language file into the client's embedded data.

Writes crates/client/data/en_us.json.gz. Run tools/fetch_vanilla.py first: the
file is read straight out of the downloaded client jar, so decompiling the
client is not needed.

    python tools/extract_lang.py 26.3
"""

import gzip
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSET = "assets/minecraft/lang/en_us.json"


def language_bytes(version_dir: Path) -> bytes:
    jar = version_dir / "client.jar"
    if jar.exists():
        with zipfile.ZipFile(jar) as archive:
            try:
                return archive.read(ASSET)
            except KeyError:
                pass
    # A decompiled tree carries the jar's assets alongside the sources.
    source = version_dir / "src" / ASSET
    if source.exists():
        return source.read_bytes()
    raise SystemExit(
        f"no {ASSET} in {jar} or {source}: run "
        f"`python tools/fetch_vanilla.py {version_dir.name}` first"
    )


def main() -> None:
    version = sys.argv[1] if len(sys.argv) > 1 else "26.3"
    data = language_bytes(ROOT / "vanilla" / version)
    dest = ROOT / "crates" / "client" / "data" / "en_us.json.gz"
    dest.parent.mkdir(parents=True, exist_ok=True)
    with gzip.GzipFile(dest, "wb", mtime=0) as target:
        target.write(data)
    print(f"wrote {dest} ({dest.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
