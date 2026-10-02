#!/usr/bin/env python3
"""Compile and run tools/extractor/Extract.java against the vanilla server jar.

Writes crates/world/data/blocks.json.gz (collision shapes and movement
properties for every block state, attributes) and
crates/physics/src/mth_tables.rs (Mth lookup tables). Run tools/fetch_vanilla.py first.

    python tools/extract_blocks.py 26.3
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

    build = base / "extractor"
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    subprocess.run(
        ["javac", "-nowarn", "-cp", classpath, "-d", str(build), str(ROOT / "tools" / "extractor" / "Extract.java")],
        check=True,
    )

    out_json = build / "blocks.json"
    # Bootstrap writes logs and may create files; keep them inside the build dir.
    subprocess.run(
        ["java", "-cp", str(build) + os.pathsep + classpath, "Extract", str(out_json),
         str(ROOT / "crates" / "physics" / "src" / "mth_tables.rs")],
        cwd=build,
        check=True,
    )

    dest = ROOT / "crates" / "world" / "data" / "blocks.json.gz"
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(out_json, "rb") as src, gzip.GzipFile(dest, "wb", mtime=0) as dst:
        shutil.copyfileobj(src, dst)
    print(f"wrote {dest} ({dest.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
