#!/usr/bin/env python3
"""Resume release asset publication without replacing an existing asset."""

import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def publish(tag, paths):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", tag):
        raise ValueError("invalid release tag")
    paths = [Path(path) for path in paths]
    if not paths or len({p.name for p in paths}) != len(paths):
        raise ValueError("missing or duplicate local release asset")
    for path in paths:
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz", path.name):
            raise ValueError("invalid release asset name")
        if path.is_symlink() or not path.is_file():
            raise ValueError("release asset must be a regular file")

    response = subprocess.run(["gh", "release", "view", tag, "--json", "assets"],
                              check=True, capture_output=True, text=True)
    assets = json.loads(response.stdout)["assets"]
    names = [asset["name"] for asset in assets]
    if len(set(names)) != len(names):
        raise ValueError("duplicate remote release asset names")
    existing = set(names)
    # Verify every collision before uploading anything. An existing filename is
    # never overwritten, including if another publisher wins an upload race.
    with tempfile.TemporaryDirectory(prefix="release-asset-check-") as temporary:
        for path in paths:
            if path.name in existing:
                subprocess.run(["gh", "release", "download", tag, "--pattern", path.name,
                                "--dir", temporary], check=True)
                downloaded = Path(temporary) / path.name
                if downloaded.is_symlink() or not downloaded.is_file() or digest(downloaded) != digest(path):
                    raise ValueError("existing release asset differs: " + path.name)
        for path in paths:
            if path.name not in existing:
                subprocess.run(["gh", "release", "upload", tag, str(path)], check=True)


def main():
    try:
        if len(sys.argv) < 3:
            raise ValueError("usage: publish-release-assets.py TAG FILE...")
        publish(sys.argv[1], sys.argv[2:])
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError,
            subprocess.CalledProcessError) as error:
        print("Release asset publication failed: " + str(error), file=sys.stderr)
        return 1
    print("Release assets published or already matched")
    return 0


if __name__ == "__main__":
    sys.exit(main())
