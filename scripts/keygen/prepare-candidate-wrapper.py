#!/usr/bin/env python3
"""Prepare the candidate wrapper from the prover's reviewed source closure."""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
try:
    import tomllib
except ImportError:
    import tomli as tomllib


PROVER_TOOLING_COMMIT = "3033d6c1ffbce5080d74d7a2eeb11b922ca78834"
PROVER_TOOLING_TREE = "6fa29f5c5f16f7811b23ccd76a2ef754932fb6d0"
WRAPPER_COMMIT = "585595f145cb53a09a130706ca36f80ddcac3961"
UPSTREAM = "https://github.com/matter-labs/zksync-airbender"
UPSTREAM_COMMIT = "03454c7a41053a4b88bb421e97fb9efe893a92f5"
FORK = "https://github.com/syscoin/zksync-airbender"
FORK_COMMIT = "e8f380777b7e90388c7323d6fd80a3b58c589f17"
FORK_TREE = "799e018339df40d0e456ba3fa0fc768ce0691853"
OLD_SOURCE = f"git+{UPSTREAM}?tag=v0.6.0-rc.2#{UPSTREAM_COMMIT}"
NEW_SOURCE = f"git+{FORK}?rev={FORK_COMMIT}#{FORK_COMMIT}"
AIRBENDER_DEPS = {
    "unified_reduced_machine_verifier", "blake_verifier", "execution_utils",
    "setups", "mersenne_field", "zkos_verifier_generator", "prover",
}
WRAPPER_CRYPTO_PACKAGES = {
    "boojum", "fflonk", "franklin-crypto", "rescue_poseidon", "snark_wrapper",
    "zksync_bellman", "zksync_cs_derive", "zksync_ff", "zksync_ff_derive",
    "zksync_pairing",
}


def git(directory: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(directory), *args], text=True).strip()


def reviewed_preparer(tooling: Path):
    tooling = tooling.resolve(strict=True)
    if git(tooling, "rev-parse", "--show-toplevel") != str(tooling):
        raise ValueError("prover tooling checkout must be its Git root")
    if (git(tooling, "rev-parse", "HEAD") != PROVER_TOOLING_COMMIT
            or git(tooling, "rev-parse", "HEAD^{tree}") != PROVER_TOOLING_TREE
            or git(tooling, "status", "--porcelain", "--untracked-files=all")):
        raise ValueError("unexpected prover tooling source")
    script = tooling / "scripts/prepare-patched-airbender.py"
    spec = importlib.util.spec_from_file_location("reviewed_prover_preparer", script)
    if spec is None or spec.loader is None:
        raise ValueError("missing reviewed prover preparer")
    module = importlib.util.module_from_spec(spec)
    old_dont_write_bytecode = sys.dont_write_bytecode
    try:
        sys.dont_write_bytecode = True
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = old_dont_write_bytecode
    return module


def pin(wrapper: Path, crypto: Path, crypto_pins: dict, crypto_paths: dict,
        wrapper_tree: str, crypto_tree: str) -> None:
    wrapper = wrapper.resolve(strict=True)
    if git(wrapper, "rev-parse", "HEAD") != WRAPPER_COMMIT:
        raise ValueError("unexpected wrapper revision")
    if git(wrapper, "write-tree") != wrapper_tree or git(crypto, "write-tree") != crypto_tree:
        raise ValueError("reviewed wrapper or crypto patch tree changed")

    manifest_path, lock_path = wrapper / "Cargo.toml", wrapper / "Cargo.lock"
    manifest, lock = manifest_path.read_text(), lock_path.read_text()
    parsed_manifest = tomllib.loads(manifest)
    if "patch" in parsed_manifest or "replace" in parsed_manifest:
        raise ValueError("unexpected wrapper dependency override")
    dependencies = parsed_manifest["workspace"]["dependencies"]
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

    canonical = tomllib.loads(lock)
    old_air = [row for row in canonical["package"] if row.get("source") == OLD_SOURCE]
    crypto_source = crypto_pins["upstream_lock_source"]
    old_crypto = [row for row in canonical["package"] if row.get("source") == crypto_source]
    crypto_names = {row["name"] for row in old_crypto}
    if (len(old_air) != 44 or lock.count(OLD_SOURCE) != 44
            or len(old_crypto) != len(WRAPPER_CRYPTO_PACKAGES)
            or crypto_names != WRAPPER_CRYPTO_PACKAGES
            or any(row["version"] != crypto_pins["upstream_packages"][row["name"]]
                   for row in old_crypto)
            or set(crypto_paths) != set(crypto_pins["upstream_packages"])):
        raise ValueError("unexpected wrapper proving lock graph")
    if any("github.com/matter-labs/zksync-crypto" in row.get("source", "")
           and "zksync-crypto-gpu" not in row.get("source", "")
           and row.get("source") != crypto_source for row in canonical["package"]):
        raise ValueError("mixed common crypto sources in wrapper lock")

    new_manifest = manifest.replace(old_manifest_url, f'git = "{FORK}"')
    new_manifest = new_manifest.replace(old_manifest_tag, f'rev = "{FORK_COMMIT}"')
    # The CPU wrapper uses ten common-crypto packages; bind precisely those to
    # the prover's verified patched tree without changing the stock-control graph.
    new_manifest += '\n[patch.' + json.dumps(crypto_pins["upstream_url"]) + ']\n'
    for name in sorted(crypto_names):
        new_manifest += (json.dumps(name) + " = { path = "
                         + json.dumps(str(crypto / crypto_paths[name])) + " }\n")

    blocks = re.split(r"(?m)(?=^\[\[package\]\]\r?$)", lock)
    if len(blocks) != len(canonical["package"]) + 1:
        raise ValueError("unexpected wrapper lock layout")
    for index, row in enumerate(canonical["package"], 1):
        source = row.get("source")
        if source == OLD_SOURCE:
            old_line = "source = " + json.dumps(OLD_SOURCE)
            new_line = "source = " + json.dumps(NEW_SOURCE)
            if blocks[index].count(old_line) != 1:
                raise ValueError("unexpected Airbender source row")
            blocks[index] = blocks[index].replace(old_line, new_line)
        elif source == crypto_source:
            source_line = "source = " + json.dumps(crypto_source) + "\n"
            if blocks[index].count(source_line) != 1:
                raise ValueError("unexpected common crypto source row")
            blocks[index] = blocks[index].replace(source_line, "")
    new_lock = "".join(blocks)
    updated = tomllib.loads(new_manifest)
    if any(updated["workspace"]["dependencies"][name].get("git") != FORK
           or updated["workspace"]["dependencies"][name].get("rev") != FORK_COMMIT
           for name in selected):
        raise ValueError("wrapper manifest pin failed")
    new_packages = tomllib.loads(new_lock)["package"]
    if (sum(row.get("source") == NEW_SOURCE for row in new_packages) != 44
            or {row["name"] for row in new_packages if row["name"] in crypto_names
                and "source" not in row} != crypto_names
            or any("zksync-airbender" in row.get("source", "")
                   and row.get("source") != NEW_SOURCE for row in new_packages)):
        raise ValueError("wrapper lock pin failed")
    manifest_path.write_text(new_manifest)
    lock_path.write_text(new_lock)


def prepare(tooling: Path, airbender: Path, build: Path) -> Path:
    preparer = reviewed_preparer(tooling)
    air_pins = preparer.load_airbender_pins()
    wrapper_pins = preparer.load_wrapper_pins()
    crypto_pins = preparer.load_crypto_pins()
    if (air_pins["fork_url"], air_pins["fork_commit"], air_pins["fork_tree"]) != (FORK, FORK_COMMIT, FORK_TREE):
        raise ValueError("candidate Airbender source differs from prover")
    if wrapper_pins["upstream_commit"] != WRAPPER_COMMIT:
        raise ValueError("candidate wrapper source differs from prover")
    preparer.verify_airbender_source(airbender.resolve(strict=True), air_pins)
    build = build.resolve()
    build.mkdir(parents=True, exist_ok=False)
    wrapper, _, _ = preparer.prepare_wrapper(build, wrapper_pins)
    crypto, _, crypto_paths = preparer.prepare_crypto(build, crypto_pins)
    pin(wrapper, crypto, crypto_pins, crypto_paths,
        wrapper_pins["patched_tree"], crypto_pins["patched_tree"])
    return wrapper


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("prover_tooling", type=Path)
    parser.add_argument("airbender", type=Path)
    parser.add_argument("build", type=Path)
    args = parser.parse_args()
    print(prepare(args.prover_tooling, args.airbender, args.build))
