#!/usr/bin/env python3
"""Compile and run tools/extractor/ClipVectors.java against the vanilla server jar.

Writes crates/world/tests/data/clip_vectors.txt: rays and what vanilla's
BlockGetter.clip returns for them. Run tools/fetch_vanilla.py first.

    python tools/gen_clip_vectors.py 26.3
"""

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

    build = base / "extractor-clip"
    shutil.rmtree(build, ignore_errors=True)
    build.mkdir(parents=True)
    subprocess.run(
        ["javac", "-nowarn", "-cp", classpath, "-d", str(build), str(ROOT / "tools" / "extractor" / "ClipVectors.java")],
        check=True,
    )
    out = ROOT / "crates" / "world" / "tests" / "data" / "clip_vectors.txt"
    subprocess.run(["java", "-cp", str(build) + os.pathsep + classpath, "ClipVectors", str(out)], cwd=build, check=True)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
