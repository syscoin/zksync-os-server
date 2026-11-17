#!/usr/bin/env python3
"""Verify the unregistered v32 OS candidate against the reviewed Syscoin source tree."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: verify.py <zksync-os repository containing the reviewed base tree>")
    repo = Path(sys.argv[1]).resolve()
    directory = Path(__file__).resolve().parent
    candidate = json.loads((directory / "source.json").read_text())
    patch = directory / "source.patch"
    if hashlib.sha256(patch.read_bytes()).hexdigest() != candidate["patch_sha256"]:
        sys.exit("candidate patch SHA-256 mismatch")
    with tempfile.TemporaryDirectory() as temporary:
        env = {**os.environ, "GIT_INDEX_FILE": str(Path(temporary) / "index")}
        git = ["git", "-C", str(repo)]
        subprocess.run(git + ["read-tree", candidate["base_tree"]], env=env, check=True)
        subprocess.run(git + ["apply", "--cached", "--unidiff-zero", str(patch)], env=env, check=True)
        tree = subprocess.check_output(git + ["write-tree"], env=env, text=True).strip()
    if tree != candidate["source_tree"]:
        sys.exit(f"candidate tree mismatch: {tree}")
    print(tree)


if __name__ == "__main__":
    main()
