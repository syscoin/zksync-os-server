#!/usr/bin/env python3
"""Exact generated Era release materialization; not fixture or deployment approval."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

SERVER = Path(__file__).resolve().parent.parent
BUNDLE = SERVER / "scripts/releases/era-v32"
CHECKER_SHA = "b1a861dce1d8e0a96d34d91f88671ef921b98bd5c23035be4675993deb84ad97"
MANIFEST_SHA = "454c3739b0d4091a4c661873832dd8fa29d3f0e8c9c0bf3fb4f40c8747d6c8b7"
VK = "0x8e6285da659dac3cfff56dbae3bf445e1524b9f08a9270f19d3361a97f8ce990"
PREIMAGES = {
    "AllContractsHashes.json": {"size": 160049,
        "sha256": "ac3feeba44c202ee082b855424007591de5f2031daeccb5991cb7f2e529d5368"},
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol": {"size": 95217,
        "sha256": "9926cf03b65cd404dfb1e5b2d6d9b487c60d2ead2d5b7998fcf0e3dd2249bb15"},
    "tools/verifier-gen/data/ZKsyncOSVerifierPlonk.sol": None,
    "tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json": {"size": 8077,
        "sha256": "368a53c438267cde0a21f49cb8fda8cc460e301462e205dfc87f5a01d7676bb9"},
}
APP_SOURCES = {
    "lib/types/src/protocol/proving_version.rs":
        "0441bd1ead3fc20fc783b48567b8a4311a7688c47125c4a62ca43e500243da31",
    "node/bin/src/prover_api/fri_proof_verifier.rs":
        "2c32e3ababc5419b35e195f3eaec17993cf5eec8ccef2c151b11bb99040c5682",
}
# SYSCOIN: This is source-controlled release data, never an operator flag or
# writable sidecar. It certifies the packaged fixture, not whether the exact
# generated contract sources can be materialized for a fresh deployment.
CANONICAL_BINDING = {'descriptor_sha256': '75cdda91c6d9a47aa390ad47697d210206a6e90c79d1abc6e8cdf9784a43b1b0',
 'identity_sha256': {'clean_boundary': '1175ed1a1afa24c3cbe0daba7f4d7e21cf33bc2308bedfa29a9a79c055918360',
                     'deployment_record': '430fe05606a015859366d120bea2f0780432599f513745d0030f1ca0409188a2',
                     'proof_identities': 'e72d889d99f880ff055dc74aa2c30850dfb67afada9cf0dd09c0ce657193ed55',
                     'snapshot_manifest': '34d875872dac09499a848ea28c73e8e5047372e093613537c36ca4b6de48f1ee',
                     'source_identities': 'dfa27d1fff8db912c7d977094478e8e1e9b57c4a78d8f92342f2be855712d137'},
 'versions_sha256': '33a1451fb1da5508eaa56d92a880b7f8e0f4743f4eec947b04b1790d92ecb1ba'}
IDENTITIES = {"source_identities", "proof_identities", "deployment_record",
              "clean_boundary", "snapshot_manifest"}
SNAPSHOTS = {"core_consensus", "nevm_database", "gateway_database",
             "edge6565_database", "edge6566_database"}
TOOLS = {"syscoind", "syscoin_cli", "syscoin_geth", "server", "fri_prover",
         "snark_prover", "restore_supervisor"}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def regular(path):
    path = Path(path)
    require(path.is_absolute() and path.resolve() == path,
            "path must be absolute with no symlink components")
    require(stat.S_ISREG(path.lstat().st_mode), "expected a regular file")
    return path


def digest(path):
    with regular(path).open("rb") as stream:
        result = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def exact(path, sha):
    require(re.fullmatch(r"[0-9a-f]{64}", sha or "") is not None,
            "invalid expected SHA-256")
    require(digest(path) == sha, "input SHA-256 mismatch: " + str(path))


def read_json(path):
    require(regular(path).stat().st_size <= 1024 * 1024, "JSON input too large")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    return json.loads(path.read_text(), object_pairs_hook=unique)


def file_identity(root, row):
    require(isinstance(row, dict) and set(row) == {"path", "size", "sha256"},
            "invalid file identity")
    rel = row["path"]
    require(isinstance(rel, str) and rel and not Path(rel).is_absolute()
            and all(x not in ("", ".", "..") for x in rel.split("/")),
            "unsafe fixture relative path")
    path = root / rel
    require(type(row["size"]) is int and row["size"] > 0
            and regular(path).stat().st_size == row["size"], "fixture size mismatch")
    exact(path, row["sha256"])
    return rel


def check_app_sources(server):
    for rel, sha in APP_SOURCES.items():
        exact(server / rel, sha)


def check_gateway_chain_id(requested, server):
    _, manifest = load_bundle()
    guest_gateway = manifest["v2_prover"]["gateway_chain_id"]
    require(type(guest_gateway) is int and 0 < guest_gateway < 2**32,
            "invalid Gateway chain ID in the release manifest")
    check_app_sources(server)
    os_patch = server / "scripts/patches/zksync-os-syscoin-v0.4.0.patch"
    exact(os_patch, manifest["native_os_source"]["patch_sha256"])
    for label, path, prefix in (
            ("node source", server / "lib/types/src/lib.rs", ""),
            ("OS patch", os_patch, "+")):
        pattern = r"(?m)^" + re.escape(prefix) + r"pub const SYSCOIN_GATEWAY_CHAIN_ID: u64 = ([0-9_]+);$"
        matches = re.findall(pattern, regular(path).read_text())
        require(len(matches) == 1 and int(matches[0].replace("_", "")) == guest_gateway,
                f"{label} Gateway chain ID disagrees with the release manifest")
    require(requested == guest_gateway,
            f"Gateway chain ID {requested} needs a matching guest and V8 key; "
            f"this release is bound to {guest_gateway}")
    return guest_gateway


def canonical_fixture_dir(server, fixture=None):
    source = server / "local-chains/v32.0"
    marker = "CANONICAL_V8_REGENERATION_REQUIRED"
    require(not os.path.lexists(source / marker), "canonical fixture regeneration marker is present")
    if fixture is None:
        return source
    require(fixture.is_absolute() and fixture.resolve() == fixture and fixture.is_dir(),
            "canonical fixture directory must be absolute and contain no symlinks")
    require(not os.path.lexists(fixture / marker), "canonical fixture regeneration marker is present")
    return fixture


def check_source_binding(server):
    canonical_fixture_dir(server)
    binding = CANONICAL_BINDING
    require(isinstance(binding, dict), "generated release is not activated: genuine fixture binding absent")
    require(set(binding) == {"descriptor_sha256", "versions_sha256", "identity_sha256"}
            and isinstance(binding["identity_sha256"], dict)
            and set(binding["identity_sha256"]) == IDENTITIES,
            "invalid source-controlled canonical binding")
    digests = [binding["descriptor_sha256"], binding["versions_sha256"],
               *binding["identity_sha256"].values()]
    require(all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value)
                for value in digests), "invalid source-controlled canonical binding hash")
    # The immutable Rust registry is the second source of the descriptor identity.
    registry = regular(server / "integration-tests/src/fixture_backend.rs").read_text()
    declarations = re.findall(
        r"const TRUSTED_DESCRIPTORS:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(.*?)\];",
        registry, re.S)
    require(len(declarations) == 1, "missing or ambiguous fixture registry")
    entry = re.compile(r'\(\s*"([^"\\\r\n]+)"\s*,\s*"([0-9a-f]{64})"\s*,?\s*\)')
    remaining = declarations[0].strip()
    entries = []
    while remaining:
        match = entry.match(remaining)
        require(match is not None, "nonliteral fixture registry")
        entries.append(match.groups())
        remaining = remaining[match.end():].lstrip()
        if remaining:
            require(remaining.startswith(","), "nonliteral fixture registry")
            remaining = remaining[1:].lstrip()
    require(sum(version == "v32.0" for version, _ in entries) == 1
            and ("v32.0", binding["descriptor_sha256"]) in entries,
            "canonical descriptor is not in the consumer registry")
    check_app_sources(server)
    return binding


def check_activation(server, fixture=None):
    fixture = canonical_fixture_dir(server, fixture)
    binding = check_source_binding(server)
    descriptor = fixture / "l1-backend.json"
    exact(descriptor, binding["descriptor_sha256"])
    exact(fixture / "versions.yaml", binding["versions_sha256"])
    data = read_json(descriptor)
    require(set(data) == {"schema_version", "protocol_version", "backend"}
            and type(data["schema_version"]) is int and data["schema_version"] == 1
            and data["protocol_version"] == "v32.0", "wrong fixture descriptor version")
    backend = data["backend"]
    require(set(backend) == {"kind", "inventory"}
            and backend["kind"] == "syscoin_core_nevm", "real Core/NEVM fixture required")
    inventory = backend["inventory"]
    require(set(inventory) == IDENTITIES | {"chain_ids", "finality", "snapshots", "tools"},
            "unexpected fixture inventory fields")
    require(inventory["chain_ids"] == {"root": 31337, "gateway": 57001, "edges": [6565, 6566]}
            and inventory["finality"] == {"kind": "confirmations", "required_confirmations": 5},
            "wrong real fixture topology or finality")
    paths = {"l1-backend.json", "versions.yaml"}
    rows = []
    for name in sorted(IDENTITIES):
        row = inventory[name]
        require(row["sha256"] == binding["identity_sha256"][name],
                "canonical identity record mismatch: " + name)
        rows.append(row)
    for name, expected in (("snapshots", SNAPSHOTS), ("tools", TOOLS)):
        values = inventory[name]
        require(isinstance(values, list) and len(values) == len(expected)
                and all(isinstance(v, dict) and set(v) == {"role", "file"} for v in values)
                and {v["role"] for v in values} == expected, "fixture role inventory mismatch")
        rows.extend(v["file"] for v in values)
    for row in rows:
        rel = file_identity(fixture, row)
        require(rel not in paths, "duplicate fixture file path")
        paths.add(rel)
    return binding["descriptor_sha256"]


def check_canonical_prover_assets(server, paths, fixture=None):
    fixture = canonical_fixture_dir(server, fixture)
    descriptor_sha = check_activation(server, fixture)
    descriptor = read_json(fixture / "l1-backend.json")
    identity = descriptor["backend"]["inventory"]["proof_identities"]
    file_identity(fixture, identity)
    proof = read_json(fixture / identity["path"])
    asset_fields = {"app_bin_sha256", "combined_prover_sha256", "compact_crs_sha256"}
    require(set(proof) == asset_fields | {
        "schema", "verification_key_hash", "program_commitment", "worker_commit",
    } and proof["schema"] == "syscoin-v32-canonical-proof-identities-v1",
            "invalid canonical proof identity record")
    _, manifest = load_bundle()
    worker = manifest["v2_prover"]
    require(proof["verification_key_hash"] == manifest["verification_key_hash"]
            and all(proof[name] == worker[name] for name in (
                "program_commitment", "worker_commit", "app_bin_sha256")),
            "fixture proof identities differ from the registered application")
    require(set(paths) == asset_fields, "missing canonical prover assets")
    for name, path in paths.items():
        require(path, "missing asset path for " + name)
        exact(Path(path), proof[name])
    # The combined worker loads the executable segment from this sibling file.
    exact(Path(paths["app_bin_sha256"]).with_suffix(".text"), worker["app_text_sha256"])
    return descriptor_sha


def load_bundle():
    checker = BUNDLE / "check-release-overlay.py"
    exact(checker, CHECKER_SHA)
    exact(BUNDLE / "generated-verifier-manifest.json", MANIFEST_SHA)
    spec = importlib.util.spec_from_file_location("reviewed_era_overlay", checker)
    module = importlib.util.module_from_spec(spec)
    # No .pyc writes into the checked source bundle.
    exec(compile(checker.read_bytes(), str(checker), "exec"), module.__dict__)
    manifest = read_json(BUNDLE / "generated-verifier-manifest.json")
    require(manifest["verification_key_hash"] == VK
            and manifest["candidate_tree"] == module.CANDIDATE
            and manifest["reviewed_source_tree"] == module.SOURCE
            and set(manifest["paths"]) == set(PREIMAGES) == module.PATHS, "release manifest drift")
    return module, manifest


def git(repo, env, *args):
    return subprocess.check_output(["git", "-c", "diff.autoRefreshIndex=false", "-C", str(repo), *args], env=env,
                                   stderr=subprocess.PIPE)


def inspect_worktree(repo, overlay):
    require(repo.is_absolute() and repo.resolve() == repo, "noncanonical Era repository path")
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_OPTIONAL_LOCKS"] = "0"
    require(Path(git(repo, env, "rev-parse", "--show-toplevel").decode().strip()).resolve() == repo,
            "expected repository root")
    require(git(repo, env, "rev-parse", "HEAD").decode().strip() == overlay.BASE,
            "unexpected Era base commit")
    require(not git(repo, env, "diff", "--cached", "--name-only"), "staged Era changes forbidden")
    require(not git(repo, env, "ls-files", "--unmerged"), "unmerged Era index")
    paths = set(git(repo, env, "apply", "--numstat", "--unidiff-zero",
                    str(SERVER / "scripts/patches/era-contracts-syscoin.patch")).decode().splitlines())
    paths = {row.split("\t")[2] for row in paths} | overlay.PATHS
    untracked = set(git(repo, env, "ls-files", "--others", "--exclude-standard").decode().splitlines())
    require(untracked <= paths, "unrelated Era worktree changes")
    objects = Path(git(repo, env, "rev-parse", "--git-path", "objects").decode().strip())
    if not objects.is_absolute():
        objects = repo / objects
    with tempfile.TemporaryDirectory(prefix="era-generated-tree-") as tmp:
        tmp = Path(tmp)
        (tmp / "objects").mkdir()
        private = dict(env, GIT_INDEX_FILE=str(tmp / "index"),
                       GIT_OBJECT_DIRECTORY=str(tmp / "objects"),
                       GIT_ALTERNATE_OBJECT_DIRECTORIES=str(objects.resolve()))
        git(repo, private, "read-tree", "HEAD")
        git(repo, private, "add", "-u", "--", ".")
        changed = set(git(repo, private, "diff", "--cached", "--name-only", "HEAD").decode().splitlines())
        require(changed <= paths, "unrelated Era worktree changes")
        for rel in sorted(paths):
            path = repo / rel
            if os.path.lexists(path):
                regular(path)
                git(repo, private, "add", "-f", "--", rel)
        tree = git(repo, private, "write-tree").decode().strip()
    base_tree = git(repo, env, "rev-parse", "HEAD^{tree}").decode().strip()
    require(tree in {base_tree, overlay.SOURCE, overlay.CANDIDATE},
            "partial, modified, or unreviewed Era postimage")
    return tree, base_tree, env


def require_dependencies(repo, env):
    # No cloning, repair or recursive updates here: materialization consumes an
    # already populated exact graph. Dirty or missing submodules fail closed.
    status = git(repo, env, "submodule", "status", "--recursive").decode().splitlines()
    require(status and all(line.startswith(" ") for line in status), "submodule graph is incomplete or drifted")
    dirty = git(repo, env, "submodule", "foreach", "--quiet", "--recursive",
                "git status --porcelain --untracked-files=all")
    require(not dirty, "dirty submodule graph")


def apply_patches(repo, env, patches):
    # Even worktree-only git apply can refresh stat data in the real index.
    # Use a disposable index/object store for its internal bookkeeping too.
    objects = Path(git(repo, env, "rev-parse", "--git-path", "objects").decode().strip())
    if not objects.is_absolute():
        objects = repo / objects
    with tempfile.TemporaryDirectory(prefix="era-generated-apply-") as tmp:
        tmp = Path(tmp)
        (tmp / "objects").mkdir()
        private = dict(env, GIT_INDEX_FILE=str(tmp / "index"),
                       GIT_OBJECT_DIRECTORY=str(tmp / "objects"),
                       GIT_ALTERNATE_OBJECT_DIRECTORIES=str(objects.resolve()))
        git(repo, private, "read-tree", "HEAD")
        for path in patches:
            git(repo, private, "apply", "--unidiff-zero", "--whitespace=error-all", str(path))


def run(repo, assert_applied=False, check_bundle_only=False):
    # Source identity is required before mutation; deployment cannot depend on
    # a fixture that can only be captured after that deployment has completed.
    check_app_sources(SERVER)
    overlay, manifest = load_bundle()
    source_patch = SERVER / "scripts/patches/era-contracts-syscoin.patch"
    source_applicator = SERVER / "scripts/apply-era-contracts-syscoin-patch.sh"
    patch = BUNDLE / "generated-verifier-overlay.patch"
    overlay.check_bundle(repo, source_patch, source_applicator, patch)
    tree, base_tree, env = inspect_worktree(repo, overlay)
    if check_bundle_only:
        return {"status": "bundle_and_worktree_checked", "tree": tree,
                "target_mutated": False, "canonical_launch_authorized": False,
                "canonical_fixture_authorized": False}
    require_dependencies(repo, env)
    require(not assert_applied or tree == overlay.CANDIDATE,
            "assert-applied refuses source or upstream tree")
    if tree != overlay.CANDIDATE:
        # Earlier private-index reconstruction checked both exact transformations.
        # Keep source application separate; no source-only exception is invoked.
        if tree == base_tree:
            apply_patches(repo, env, [source_patch])
        require(inspect_worktree(repo, overlay)[0] == overlay.SOURCE, "source preimage tree mismatch")
        for rel, row in PREIMAGES.items():
            if row is None:
                require(not os.path.lexists(repo / rel), "generated-only preimage must be absent")
            else:
                file_identity(repo, dict(row, path=rel))
        apply_patches(repo, env, [patch])
    require(inspect_worktree(repo, overlay)[0] == overlay.CANDIDATE, "release postimage mismatch")
    for rel, row in manifest["paths"].items():
        file_identity(repo, dict(row, path=rel))
    require_dependencies(repo, env)
    check_app_sources(SERVER)
    return {"status": "exact_generated_release_postimage", "tree": overlay.CANDIDATE,
            "canonical_fixture_authorized": False, "target_mutated": tree != overlay.CANDIDATE,
            "deployed": False, "fixture_restore_executed": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--assert-applied", action="store_true")
    mode.add_argument("--check-bundle", action="store_true", help="read-only; never launch authorization")
    mode.add_argument("--check-canonical-fixture", action="store_true",
                      help="read-only certification of the separately accepted packaged fixture")
    mode.add_argument("--check-canonical-fixture-binding", action="store_true",
                      help="check immutable source binding without opening private fixture data")
    mode.add_argument("--check-canonical-prover-assets", action="store_true",
                      help="verify the registered app, combined worker and compact CRS")
    mode.add_argument("--check-gateway-chain-id", type=int,
                      help="check the Gateway ID before funding or broadcasting")
    parser.add_argument("era_root", type=Path, nargs="?")
    args = parser.parse_args()
    try:
        fixture_value = os.environ.get("SYSCOIN_V32_CANONICAL_FIXTURE_DIR")
        fixture = Path(fixture_value) if fixture_value is not None else None
        if args.check_canonical_fixture_binding:
            require(args.era_root is None, "source fixture binding check does not consume an Era repository")
            result = {"status": "canonical_fixture_binding_checked", "fixture_mode": "private",
                      "descriptor_sha256": check_source_binding(SERVER)["descriptor_sha256"],
                      "canonical_fixture_authorized": False, "target_mutated": False}
        elif args.check_canonical_fixture:
            require(args.era_root is None, "fixture check does not consume an Era repository")
            result = {"status": "canonical_fixture_checked", "canonical_fixture_authorized": True,
                      "descriptor_sha256": check_activation(SERVER, fixture), "target_mutated": False}
        elif args.check_canonical_prover_assets:
            require(args.era_root is None, "prover asset check does not consume an Era repository")
            paths = {
                "app_bin_sha256": os.environ.get("SYSCOIN_V8_APP_BIN"),
                "combined_prover_sha256": os.environ.get("SYSCOIN_V8_PROVER_BIN"),
                "compact_crs_sha256": os.environ.get("COMPACT_CRS_FILE"),
            }
            result = {"status": "canonical_prover_assets_checked",
                      "descriptor_sha256": check_canonical_prover_assets(SERVER, paths, fixture)}
        elif args.check_gateway_chain_id is not None:
            require(args.era_root is None, "Gateway identity check does not consume an Era repository")
            result = {"gateway_chain_id": check_gateway_chain_id(args.check_gateway_chain_id, SERVER)}
        else:
            require(args.era_root is not None, "Era repository path is required")
            result = run(args.era_root, args.assert_applied, args.check_bundle)
        print(json.dumps(result, sort_keys=True))
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(1, "error: " + str(error) + "\n")


if __name__ == "__main__":
    main()
