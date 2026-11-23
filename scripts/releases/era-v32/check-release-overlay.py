#!/usr/bin/env python3
"""Offline bundle validation only; no checkout mutation or launch authorization."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

BASE = "8fb7c29a4e3174335c6480b23f57822e054f9d5f"
SOURCE = "bdb50a9f012bdcd1761149b5729d0cecb89a9eb2"
CANDIDATE = "1dc3f74ad1f73d076beb506719e5ef9fe698e51f"
SOURCE_PATCH_SHA = "7d74ea2dc6521e36f204c71bfb0a8e826e5f786d034767920d33b9f4aa979f7e"
SOURCE_APPLICATOR_SHA = "20d58338e9aa3bec04730e335eabc6bf0ec569eb76ef7f62dee20145a9c007bf"
OVERLAY_SHA = "ade3f2a00e599a79f179b5500001936eba3789b7fb842b1fb6c3a927cadb120e"
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


def check_source_postimages(applicator, expected_paths, read_source):
    # Tree identity alone does not prove that the shell applicator accepts that tree.
    match = re.search(rb"done <<'SYSCOIN_POSTIMAGE_MANIFEST'\n(.*?)\nSYSCOIN_POSTIMAGE_MANIFEST\n",
                      applicator, re.S)
    require(match is not None, "source postimage manifest missing")
    paths = []
    for line in match[1].decode().splitlines():
        fields = line.split()
        require(len(fields) == 3, "invalid source postimage row")
        size, digest, path = fields
        require(size.isdecimal() and re.fullmatch(r"[0-9a-f]{64}", digest),
                "invalid source postimage identity")
        require(path in expected_paths and path not in paths, "unexpected source postimage path")
        raw = read_source(path)
        require(len(raw) == int(size) and hashlib.sha256(raw).hexdigest() == digest,
                "source postimage mismatch: " + path)
        paths.append(path)
    require(paths == sorted(expected_paths), "source postimage path set mismatch")


def check_bundle(repo, source_patch, source_applicator, overlay):
    checked_file(source_patch, SOURCE_PATCH_SHA)
    applicator = checked_file(source_applicator, SOURCE_APPLICATOR_SHA)
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
        source_paths = git(repo, env, "diff-tree", "--no-commit-id", "--name-only", "-r", BASE, SOURCE)
        check_source_postimages(applicator, set(source_paths.decode().splitlines()),
                                lambda path: git(repo, env, "show", SOURCE + ":" + path))
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
