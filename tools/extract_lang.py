#!/usr/bin/env python3
"""Copy the vanilla en_us language file into the client's embedded data.

Writes crates/client/data/en_us.json.gz. Run tools/fetch_vanilla.py first.

    python tools/extract_lang.py 26.3
"""

import gzip
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    version = sys.argv[1] if len(sys.argv) > 1 else "26.3"
    src = ROOT / "vanilla" / version / "src" / "assets" / "minecraft" / "lang" / "en_us.json"
    dest = ROOT / "crates" / "client" / "data" / "en_us.json.gz"
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(src, "rb") as source, gzip.GzipFile(dest, "wb", mtime=0) as target:
        shutil.copyfileobj(source, target)
    print(f"wrote {dest} ({dest.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
