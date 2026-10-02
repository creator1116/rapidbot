#!/usr/bin/env python3
"""Download a vanilla Minecraft version, run its data generator and decompile the client.

Everything lands in `vanilla/<version>/` which is gitignored: Mojang's code is
unobfuscated but not open source, so it is reference material only and must
never be committed. Port logic into our own code instead of copying it.

    python tools/fetch_vanilla.py            # latest release
    python tools/fetch_vanilla.py 26.3       # specific version
    python tools/fetch_vanilla.py --no-decompile
"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
VINEFLOWER_METADATA = "https://repo1.maven.org/maven2/org/vineflower/vineflower/maven-metadata.xml"
ROOT = Path(__file__).resolve().parent.parent / "vanilla"


def fetch_json(url: str):
    with urllib.request.urlopen(url) as r:
        return json.load(r)


def download(url: str, dest: Path, sha1: str | None = None) -> Path:
    if dest.exists() and (sha1 is None or sha1_of(dest) == sha1):
        print(f"  cached {dest.name}")
        return dest
    print(f"  downloading {url}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
        shutil.copyfileobj(r, f)
    if sha1 is not None and sha1_of(tmp) != sha1:
        tmp.unlink()
        sys.exit(f"sha1 mismatch for {url}")
    tmp.replace(dest)
    return dest


def sha1_of(path: Path) -> str:
    h = hashlib.sha1()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run_data_generator(server_jar: Path, out: Path) -> None:
    if (out / "reports" / "packets.json").exists():
        print("  cached data generator output")
        return
    print("  running data generator (reports + all)")
    subprocess.run(
        ["java", "-DbundlerMainClass=net.minecraft.data.Main", "-jar", str(server_jar),
         "--reports", "--server", "--output", str(out)],
        cwd=server_jar.parent, check=True,
    )


def decompile(client_jar: Path, out: Path) -> None:
    if out.exists() and any(out.iterdir()):
        print("  cached decompiled source")
        return
    with urllib.request.urlopen(VINEFLOWER_METADATA) as r:
        latest = re.search(r"<release>([^<]+)</release>", r.read().decode()).group(1)
    jar = download(
        f"https://repo1.maven.org/maven2/org/vineflower/vineflower/{latest}/vineflower-{latest}.jar",
        ROOT / "tools" / f"vineflower-{latest}.jar",
    )
    print("  decompiling client (takes a few minutes)")
    out.mkdir(parents=True, exist_ok=True)
    # Only decompile Mojang's own classes; the jar also bundles assets.
    subprocess.run(
        ["java", "-Xmx4G", "-jar", str(jar), "--only=net/minecraft", "--only=com/mojang",
         "-log=WARN", str(client_jar), str(out)],
        check=True,
    )


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("version", nargs="?", help="defaults to latest release")
    p.add_argument("--no-decompile", action="store_true")
    args = p.parse_args()

    manifest = fetch_json(MANIFEST_URL)
    version = args.version or manifest["latest"]["release"]
    entry = next((v for v in manifest["versions"] if v["id"] == version), None)
    if entry is None:
        sys.exit(f"unknown version {version}")

    print(f"vanilla {version}")
    base = ROOT / version
    meta = fetch_json(entry["url"])
    (base).mkdir(parents=True, exist_ok=True)
    (base / "version.json").write_text(json.dumps(meta, indent=2))

    dl = meta["downloads"]
    client = download(dl["client"]["url"], base / "client.jar", dl["client"]["sha1"])
    server = download(dl["server"]["url"], base / "server.jar", dl["server"]["sha1"])

    run_data_generator(server, base / "generated")
    if not args.no_decompile:
        decompile(client, base / "src")
    print(f"done: {base}")


if __name__ == "__main__":
    main()
