#!/usr/bin/env python3
"""Compile and run tools/extractor/Items.java against the vanilla server jar.

Writes crates/world/data/items.json.gz (data component types, every item's
default components in vanilla's network encoding) and
crates/world/tests/data/item_vectors.txt (encoded item stacks the decoder is
tested against). Run tools/fetch_vanilla.py first.

    python tools/extract_items.py 26.3
"""

import gzip
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    version = sys.argv[1] if len(sys.argv) > 1 else "26.3"
    base = ROOT / "vanilla" / version
    jars = [base / "versions" / version / f"server-{version}.jar"]
    jars += sorted((base / "libraries").rglob("*.jar"))
    classpath = os.pathsep.join(str(j) for j in jars)

    build = base / "extractor-items"
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    subprocess.run(
        ["javac", "-nowarn", "-cp", classpath, "-d", str(build), str(ROOT / "tools" / "extractor" / "Items.java")],
        check=True,
    )

    out_json = build / "items.json"
    vectors = ROOT / "crates" / "world" / "tests" / "data" / "item_vectors.txt"
    vectors.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["java", "-cp", str(build) + os.pathsep + classpath, "Items", str(out_json), str(vectors)],
        cwd=build,
        check=True,
    )

    dest = ROOT / "crates" / "world" / "data" / "items.json.gz"
    with open(out_json, "rb") as src, gzip.GzipFile(dest, "wb", mtime=0) as dst:
        shutil.copyfileobj(src, dst)
    print(f"wrote {dest} ({dest.stat().st_size} bytes), {vectors}")


if __name__ == "__main__":
    main()
