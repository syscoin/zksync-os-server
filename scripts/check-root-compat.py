#!/usr/bin/env python3
"""Fence the two legacy-root advisory exceptions to authenticated forwarding crates."""

import argparse
from pathlib import Path
import re
import tomllib


REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
# These immutable crates contain only a re-export of their same-named v1 parent.
FORWARDERS = {
    "webpki-roots": "521bc38abb08001b01866da9f51eb7c5d647a19260e00054a8c7fd5f9e57f7a9",
    "webpki-root-certs": "75c7f0ef91146ebfb530314f5f1d24528d7f0767efbfd31dce919275413e393e",
}


def patched(version):
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    return match is not None and tuple(map(int, match.groups())) >= (1, 0, 10)


def check_lock(lock):
    packages = lock["package"]
    for package in packages:
        name = package["name"]
        if name not in FORWARDERS:
            continue
        if package.get("source") != REGISTRY:
            raise ValueError(f"{name}: root advisory exception requires crates.io sources")
        if patched(package["version"]):
            continue
        if (package["version"] != "0.26.11"
                or package.get("checksum") != FORWARDERS[name]):
            raise ValueError(f"{name}: unsupported vulnerable root package")
        dependencies = package.get("dependencies", [])
        if len(dependencies) != 1:
            raise ValueError(f"{name}: forwarding package must have exactly one parent")
        parent = dependencies[0].split()
        if len(parent) != 2 or parent[0] != name or not patched(parent[1]):
            raise ValueError(f"{name}: forwarding parent must be a patched stable release")
        matches = [candidate for candidate in packages
                   if candidate["name"] == name and candidate["version"] == parent[1]]
        if len(matches) != 1 or matches[0].get("source") != REGISTRY:
            raise ValueError(f"{name}: forwarding parent is absent or ambiguous")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lockfile", type=Path, default=Path("Cargo.lock"))
    args = parser.parse_args()
    with args.lockfile.open("rb") as source:
        check_lock(tomllib.load(source))
    print("Legacy root advisory exceptions resolve only to patched root stores")


if __name__ == "__main__":
    main()
