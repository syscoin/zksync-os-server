#!/usr/bin/env python3
"""Offline bundle validation only; no checkout mutation or launch authorization."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

BASE = "8fb7c29a4e3174335c6480b23f57822e054f9d5f"
SOURCE = "264d98e758c3a032942dfb08ee7d87a3f46288b4"
CANDIDATE = "117b5f2d1ad82de6a073142d45bd46a5218f493e"
SOURCE_PATCH_SHA = "0cac53c803b502e67c4a14ccf8eef4f0f576a0430fa0ee007276becbf124e2c6"
SOURCE_APPLICATOR_SHA = "503341ef094f5a5b6b7766906a53a6eda99615cdb0437f98aa563cb8b7fa029d"
OVERLAY_SHA = "76e356ccc14b4912863526e000d5ed712a59649c1d22d6cf27cee8658fb2b1ba"
PATHS = {
    "AllContractsHashes.json",
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol",
    "tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol",
    "tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json",
}
ROWS = {"l1-contracts/ZKsyncOSVerifierPlonk", "l1-contracts/GatewayCTMDeployerVerifiersZKsyncOS"}
FIELDS = ("zkBytecodeHash", "evmBytecodeHash", "evmDeployedBytecodeHash", "evmDeployedBytecodeBlakeHash")


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def checked_file(path, expected_sha):
    path = Path(path)
    require(path.is_absolute() and path.is_file() and path.resolve() == path,
            "input must be an absolute regular non-symlink path")
    contents = path.read_bytes()
    require(hashlib.sha256(contents).hexdigest() == expected_sha, "input digest mismatch")
    return contents


def inventory_projection(rows):
    require(isinstance(rows, list) and len(rows) == 278, "inventory count drift")
    identities = [(r["contractName"], r["zkBytecodePath"], r["evmBytecodePath"]) for r in rows]
    require(len(set(identities)) == 278, "duplicate inventory identity")
    closure = [r for r in rows if r["contractName"] in ROWS]
    require(len(closure) == 2 and {r["contractName"] for r in closure} == ROWS, "inventory closure drift")
    projected = copy.deepcopy(rows)
    for row in projected:
        if row["contractName"] not in ROWS:
            continue
        for field in FIELDS:
            value = row.pop(field)
            require(isinstance(value, str) and len(value) == 66 and value.startswith("0x")
                    and all(c in "0123456789abcdef" for c in value[2:]) and int(value, 16) > 0,
                    "invalid generated artifact hash")
        size = row.pop("evmDeployedBytecodeLength")
        require(type(size) is int and size > 0, "invalid deployed bytecode length")
    return projected


def check_inventory(before, after):
    require(inventory_projection(before) == inventory_projection(after), "unexpected inventory edit")


def git(repo, env, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], env=env)


def check_bundle(repo, source_patch, source_applicator, overlay):
    checked_file(source_patch, SOURCE_PATCH_SHA)
    checked_file(source_applicator, SOURCE_APPLICATOR_SHA)
    checked_file(overlay, OVERLAY_SHA)
    repo = Path(repo)
    require(repo.is_absolute() and repo.resolve() == repo, "repository path must be canonical")
    clean_env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    require(git(repo, clean_env, "rev-parse", "HEAD").decode().strip() == BASE, "unexpected upstream HEAD")
    objects = Path(git(repo, clean_env, "rev-parse", "--git-path", "objects").decode().strip())
    if not objects.is_absolute():
        objects = repo / objects
    with tempfile.TemporaryDirectory(prefix="era-release-overlay-") as temp:
        temp = Path(temp)
        (temp / "objects").mkdir()
        env = dict(clean_env, GIT_INDEX_FILE=str(temp / "index"),
                   GIT_OBJECT_DIRECTORY=str(temp / "objects"),
                   GIT_ALTERNATE_OBJECT_DIRECTORIES=str(objects.resolve()))
        git(repo, env, "read-tree", BASE)
        git(repo, env, "apply", "--cached", "--unidiff-zero", "--whitespace=error-all", str(source_patch))
        require(git(repo, env, "write-tree").decode().strip() == SOURCE, "reviewed source tree mismatch")
        before = json.loads(git(repo, env, "show", SOURCE + ":AllContractsHashes.json"))
        git(repo, env, "apply", "--cached", "--unidiff-zero", "--whitespace=error-all", str(overlay))
        actual = git(repo, env, "write-tree").decode().strip()
        require(actual == CANDIDATE, "candidate tree mismatch")
        changes = git(repo, env, "diff-tree", "--no-commit-id", "--name-only", "-r", SOURCE, actual)
        require(set(changes.decode().splitlines()) == PATHS, "generated overlay scope mismatch")
        after = json.loads(git(repo, env, "show", actual + ":AllContractsHashes.json"))
        check_inventory(before, after)
        first = git(repo, env, "show", actual + ":l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol")
        second = git(repo, env, "show", actual + ":tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol")
        require(first == second, "generated/deployed PLONK mismatch")
        for path in ("l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol",
                     "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json"):
            require(git(repo, env, "show", BASE + ":" + path) == git(repo, env, "show", actual + ":" + path),
                    "retained FFLONK drift")
    return {"status": "exact_overlay_bundle_validated", "source_tree": SOURCE, "candidate_tree": CANDIDATE,
            "inventory_rows": 278, "generated_paths": sorted(PATHS), "checkout_mutated": False,
            "launcher_wired": False, "canonical_fixture_activated": False, "deployed": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--era-root", type=Path, required=True)
    parser.add_argument("--source-patch", type=Path, required=True)
    parser.add_argument("--source-applicator", type=Path, required=True)
    parser.add_argument("--overlay", type=Path, default=Path(__file__).resolve().with_name("generated-verifier-overlay.patch"))
    args = parser.parse_args()
    print(json.dumps(check_bundle(args.era_root, args.source_patch, args.source_applicator, args.overlay), indent=2))


if __name__ == "__main__":
    main()
