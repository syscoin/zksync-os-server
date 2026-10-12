#!/usr/bin/env python3
"""Pin the disposable candidate wrapper to the reviewed Airbender fork commit."""

import argparse
from pathlib import Path
import subprocess
try:
    import tomllib
except ImportError:
    import tomli as tomllib


WRAPPER_COMMIT = "585595f145cb53a09a130706ca36f80ddcac3961"
UPSTREAM = "https://github.com/matter-labs/zksync-airbender"
UPSTREAM_COMMIT = "03454c7a41053a4b88bb421e97fb9efe893a92f5"
FORK = "https://github.com/syscoin/zksync-airbender"
FORK_COMMIT = "e8f380777b7e90388c7323d6fd80a3b58c589f17"
OLD_SOURCE = f"git+{UPSTREAM}?tag=v0.6.0-rc.2#{UPSTREAM_COMMIT}"
NEW_SOURCE = f"git+{FORK}?rev={FORK_COMMIT}#{FORK_COMMIT}"
AIRBENDER_DEPS = {
    "unified_reduced_machine_verifier", "blake_verifier", "execution_utils",
    "setups", "mersenne_field", "zkos_verifier_generator", "prover",
}


def git(directory: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(directory), *args], text=True).strip()


def pin(directory: Path) -> None:
    directory = directory.resolve(strict=True)
    if git(directory, "rev-parse", "--show-toplevel") != str(directory):
        raise ValueError("wrapper checkout must be its Git root")
    if git(directory, "rev-parse", "HEAD") != WRAPPER_COMMIT:
        raise ValueError("unexpected wrapper revision")
    if git(directory, "status", "--porcelain"):
        raise ValueError("wrapper checkout must be clean before pinning")

    manifest_path, lock_path = directory / "Cargo.toml", directory / "Cargo.lock"
    manifest, lock = manifest_path.read_text(), lock_path.read_text()
    dependencies = tomllib.loads(manifest)["workspace"]["dependencies"]
    selected = {name for name, row in dependencies.items()
                if isinstance(row, dict) and row.get("git") == UPSTREAM}
    airbender = {name for name, row in dependencies.items()
                 if isinstance(row, dict) and "zksync-airbender" in row.get("git", "")}
    if selected != AIRBENDER_DEPS or airbender != selected or any(
        dependencies[name].get("tag") != "v0.6.0-rc.2" for name in selected
    ):
        raise ValueError("unexpected wrapper Airbender dependencies")
    old_manifest_url = f'git = "{UPSTREAM}"'
    old_manifest_tag = 'tag = "v0.6.0-rc.2"'
    if manifest.count(old_manifest_url) != 7 or manifest.count(old_manifest_tag) != 7:
        raise ValueError("unexpected wrapper manifest source encoding")
    old_rows = [row for row in tomllib.loads(lock)["package"]
                if row.get("source") == OLD_SOURCE]
    if len(old_rows) != 44 or lock.count(OLD_SOURCE) != 44:
        raise ValueError("unexpected wrapper Airbender lock graph")

    new_manifest = manifest.replace(old_manifest_url, f'git = "{FORK}"')
    new_manifest = new_manifest.replace(old_manifest_tag, f'rev = "{FORK_COMMIT}"')
    new_lock = lock.replace(OLD_SOURCE, NEW_SOURCE)
    updated = tomllib.loads(new_manifest)["workspace"]["dependencies"]
    if any(updated[name].get("git") != FORK or updated[name].get("rev") != FORK_COMMIT
           for name in selected):
        raise ValueError("wrapper manifest pin failed")
    new_packages = tomllib.loads(new_lock)["package"]
    if sum(row.get("source") == NEW_SOURCE for row in new_packages) != 44 or any(
        "zksync-airbender" in row.get("source", "") and row.get("source") != NEW_SOURCE
        for row in new_packages
    ):
        raise ValueError("wrapper lock pin failed")
    manifest_path.write_text(new_manifest)
    lock_path.write_text(new_lock)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wrapper", type=Path)
    pin(parser.parse_args().wrapper)
