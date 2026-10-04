#!/usr/bin/env python3
"""Attest a server-only Airbender Git overlay, preserving the legacy revision.

The source checkout supplied by SERVER_AIRBENDER_SOURCE_DIR is read-only: only
its immutable Git objects are cloned. Canonical server manifests are never
edited. Preparation follows the existing OS rewrite; verification runs on both
sides of Cargo. This record attests source identity, not a VK, proof or release.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from collections import Counter

try:
    import tomllib
except ImportError:
    import tomli as tomllib

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "scripts/patches/airbender-server-security.json"
RECORD = ".server-airbender-security.json"
UPSTREAM = "https://github.com/matter-labs/zksync-airbender"
COMMIT = "03454c7a41053a4b88bb421e97fb9efe893a92f5"
TREE = "3af54eb50c31d8e78575434c3f0ab4386891c131"
TAG = "v0.6.0-rc.2"
PATCHED_TREE = "e30d9332b55cbc6a5ea4cae71824e6a5a0858394"
PATCH_SHA = "b5c9e2d1cf89bcf7019bf9e822a61b95cd2d28b00f231a10d90ad7f23d4862e1"
SELECTED = "git+" + UPSTREAM + "?tag=" + TAG + "#" + COMMIT
LEGACY = ("git+" + UPSTREAM
          + "?rev=73d69b5#73d69b5346b3c2350fa104a56ec4df78840cea99")
SELECTED_CLOSURE = "2fefe8f11e55ae3a85a4656bd973dc700507db82434cc1728aa8aa3a9b605486"
LEGACY_CLOSURE = "30ca22d2f5c800b589c2ba2fe6912739a58e4a07e6d2cf568ce5b2a87301d56c"
DIRECT = {
    "execution_utils": {"git": UPSTREAM, "tag": TAG, "features": ["verifier_binaries"]},
    "riscv_transpiler": {"git": UPSTREAM, "tag": TAG},
    "verifier_common": {"git": UPSTREAM, "tag": TAG},
}
SELECTED_REFS = {"common_constants": 19, "field": 18, "blake2s_u32": 5,
                 "riscv_transpiler": 6, "worker": 6, "cs": 3}
LEGACY_REFS = {"common_constants": 3, "field": 2, "riscv_transpiler": 1,
               "blake2s_u32": 1, "cs": 1, "worker": 1}
INPUT_PATHS = (
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    "scripts/prepare-server-airbender.py",
    "scripts/_patched-zksync-os-workspace.sh",
    "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh",
    "scripts/patches/airbender-server-security.json",
    "scripts/patches/airbender-server-security.patch",
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), f"not a regular input: {path}")
    return path.read_bytes()


def sha(path):
    return digest(regular(path))


def object_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def document(path):
    return json.loads(regular(path), object_pairs_hook=object_pairs)


def semantic_sha(rows):
    return digest(json.dumps(rows, sort_keys=True, separators=(",", ":")).encode())


def load_pins(path=MANIFEST):
    pins = document(path)
    require(isinstance(pins, dict) and set(pins) == {
        "schema_version", "upstream_url", "upstream_commit", "upstream_tree",
        "upstream_lock_source", "selected_package_count", "selected_closure_sha256",
        "selected_qualified_reference_count", "legacy_lock_source",
        "legacy_package_count", "legacy_closure_sha256", "legacy_qualified_reference_count",
        "patch_file", "patch_sha256", "changed_files", "patched_tree", "purpose",
    }, "unknown server Airbender manifest fields")
    expected = {
        "schema_version": 1, "upstream_url": UPSTREAM, "upstream_commit": COMMIT,
        "upstream_tree": TREE, "upstream_lock_source": SELECTED,
        "selected_package_count": 44, "selected_closure_sha256": SELECTED_CLOSURE,
        "selected_qualified_reference_count": 57, "legacy_lock_source": LEGACY,
        "legacy_package_count": 6, "legacy_closure_sha256": LEGACY_CLOSURE,
        "legacy_qualified_reference_count": 9, "patch_file": "airbender-server-security.patch",
        "patch_sha256": PATCH_SHA, "patched_tree": PATCHED_TREE,
    }
    for key, value in expected.items():
        require(type(pins[key]) is type(value) and pins[key] == value,
                f"unreviewed server Airbender identity: {key}")
    require(isinstance(pins["purpose"], str) and pins["purpose"], "missing purpose")
    rows = pins["changed_files"]
    require(isinstance(rows, dict) and len(rows) == 31, "wrong changed-source inventory")
    for relative, row in rows.items():
        require(isinstance(relative, str) and re.fullmatch(r"[A-Za-z0-9_./-]+", relative)
                and not Path(relative).is_absolute() and ".." not in Path(relative).parts
                and str(Path(relative)) == relative, "noncanonical source path")
        require(isinstance(row, dict) and set(row) == {
            "preimage_sha256", "postimage_sha256", "postimage_size"
        }, "unknown source identity fields")
        is_new = relative in {"execution_utils/src/setup_summaries.rs",
                             "gpu_prover/src/execution/empty_inits_and_teardowns.rs"}
        require((row["preimage_sha256"] is None) == is_new, "invalid new-file preimage")
        for name in ("preimage_sha256", "postimage_sha256"):
            value = row[name]
            if name == "preimage_sha256" and is_new:
                continue
            require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value)
                    and value != "0" * 64, "invalid source digest")
        require(type(row["postimage_size"]) is int and row["postimage_size"] > 0,
                "invalid postimage size")
    require(sha(path.parent / pins["patch_file"]) == PATCH_SHA, "patch SHA-256 mismatch")
    return pins


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, dict):
        for key, item in value.items():
            yield from strings(key)
            yield from strings(item)


def validate_lock(lock, selected=SELECTED):
    selected_ref, legacy_ref = selected.split("#")[0], LEGACY.split("#")[0]
    rows = lock.get("package")
    require(isinstance(rows, list), "missing package graph")
    identities = [(r["name"], r["version"], r.get("source")) for r in rows]
    require(len(identities) == len(set(identities)), "duplicate package identity")
    current = [r for r in rows if r.get("source") == selected]
    legacy = [r for r in rows if r.get("source") == LEGACY]
    require(len(current) == 44 and len(legacy) == 6, "partial or mixed Airbender graph")
    restored = copy.deepcopy(current)
    for row in restored:
        row["source"] = SELECTED
        if "dependencies" in row:
            row["dependencies"] = [
                dep.replace(selected_ref, SELECTED.split("#")[0])
                for dep in row["dependencies"]
            ]
    require(semantic_sha(restored) == SELECTED_CLOSURE, "selected Airbender closure changed")
    require(semantic_sha(legacy) == LEGACY_CLOSURE, "legacy Airbender closure changed")
    names = {r["name"] for r in current}
    for row in rows:
        if row["name"] in names and row["version"] == "0.1.0":
            require(row.get("source") in {selected, LEGACY}, "aliased Airbender package source")
    refs = Counter(dep for row in rows for dep in row.get("dependencies", []))
    for origin, expected in ((selected_ref, SELECTED_REFS), (legacy_ref, LEGACY_REFS)):
        found = {dep: n for dep, n in refs.items() if origin in dep}
        require(found == {f"{name} 0.1.0 ({origin})": n for name, n in expected.items()},
                "Airbender qualified references changed")
    allowed = {selected, LEGACY} | {
        f"{name} 0.1.0 ({origin})"
        for origin, names in ((selected_ref, SELECTED_REFS), (legacy_ref, LEGACY_REFS))
        for name in names
    }
    for value in strings(lock):
        if "zksync-airbender" in value or selected_ref in value:
            require(value in allowed, "unknown or encoded Airbender source")


def validate_manifest(manifest, url=UPSTREAM):
    expected = copy.deepcopy(DIRECT)
    for row in expected.values():
        row["git"] = url
    deps = manifest.get("workspace", {}).get("dependencies", {})
    require(all(deps.get(name) == row for name, row in expected.items()),
            "direct Airbender dependencies changed")
    actual = [value for value in strings(manifest)
              if "zksync-airbender" in value or value == url]
    require(actual == [url] * 3, "additional or aliased direct Airbender source")


def rewrite(toml_text, lock_text, local_url, revision):
    require(re.fullmatch(r"[0-9a-f]{40}", revision), "invalid local commit")
    manifest, lock = tomllib.loads(toml_text), tomllib.loads(lock_text)
    validate_manifest(manifest)
    validate_lock(lock)
    source = f"git+{local_url}?tag={TAG}#{revision}"
    # Exact textual substitutions preserve all OS deltas and every legacy byte.
    before_ref, after_ref = SELECTED.split("#")[0], source.split("#")[0]
    new_toml = toml_text.replace(f'git = "{UPSTREAM}"', f'git = "{local_url}"')
    require(toml_text.count(f'git = "{UPSTREAM}"') == 3, "unexpected manifest encoding")
    require(lock_text.count(SELECTED) == 44 and lock_text.count(before_ref + ")") == 57,
            "unexpected lock source encoding")
    new_lock = lock_text.replace(SELECTED, source).replace(before_ref + ")", after_ref + ")")
    expected_manifest = copy.deepcopy(manifest)
    for name in DIRECT:
        expected_manifest["workspace"]["dependencies"][name]["git"] = local_url
    expected_lock = copy.deepcopy(lock)
    for row in expected_lock["package"]:
        if row.get("source") == SELECTED:
            row["source"] = source
        if "dependencies" in row:
            row["dependencies"] = [
                dep.replace(before_ref + ")", after_ref + ")") for dep in row["dependencies"]
            ]
    require(tomllib.loads(new_toml) == expected_manifest, "unexpected manifest semantic delta")
    require(tomllib.loads(new_lock) == expected_lock, "unexpected lock semantic delta")
    validate_manifest(tomllib.loads(new_toml), local_url)
    validate_lock(tomllib.loads(new_lock), source)
    return new_toml, new_lock


def git(repo, *args, binary=False, extra_env=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    env.update(extra_env or {})
    result = subprocess.run(
        ["git", "-c", "core.hooksPath=" + os.devnull, "-c", "core.autocrlf=false",
         "-c", "core.fsmonitor=false", "-C", str(repo), *args],
        env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    ).stdout
    return result if binary else result.decode().strip()


def source_images(repo, pins, post):
    for name, row in pins["changed_files"].items():
        path = repo / name
        if not post and row["preimage_sha256"] is None:
            require(not path.exists() and not path.is_symlink(), "new source already exists")
            continue
        require(sha(path) == row["postimage_sha256" if post else "preimage_sha256"],
                f"{'postimage' if post else 'preimage'} mismatch: {name}")
        if post:
            require(path.stat().st_size == row["postimage_size"], f"postimage size: {name}")


def clean(repo):
    require(not git(repo, "status", "--porcelain=v1", "--untracked-files=all",
                    "--ignored=matching"), "Airbender checkout or cache is dirty")
    flags = git(repo, "ls-files", "-v").splitlines()
    require(all(line.startswith("H ") for line in flags), "hidden Airbender index entries")


def verify_checkout(repo, pins, revision):
    require(repo.is_dir() and not repo.is_symlink() and (repo / ".git").is_dir(),
            "invalid Airbender checkout")
    require(git(repo, "rev-parse", "--show-toplevel") == str(repo), "aliased checkout root")
    require(git(repo, "rev-parse", "HEAD") == revision, "Airbender commit drift")
    require(git(repo, "rev-parse", "HEAD^") == COMMIT, "wrong Airbender parent")
    require(git(repo, "rev-parse", "HEAD^{tree}") == PATCHED_TREE, "Airbender tree drift")
    require(git(repo, "rev-parse", "refs/tags/" + TAG + "^{commit}") == revision,
            "local Airbender tag drift")
    clean(repo)
    source_images(repo, pins, True)


def prepare_checkout(parent, pins, patch, source=None):
    if source:
        require(source.is_absolute() and not source.is_symlink()
                and source.resolve() == source, "noncanonical source clone")
        require(git(source, "rev-parse", "--show-toplevel") == str(source),
                "source clone is not a Git root")
        require(git(source, "rev-parse", COMMIT + "^{tree}") == TREE,
                "wrong immutable source clone")
    # A fresh clone isolates simultaneous jobs and cannot trust a previous cache.
    repo = Path(tempfile.mkdtemp(prefix="server-airbender-", dir=parent)).resolve()
    git(parent, "clone", "--no-hardlinks", "--no-checkout", "--", str(source or UPSTREAM), str(repo))
    git(repo, "checkout", "--detach", COMMIT)
    require(git(repo, "rev-parse", "HEAD^{tree}") == TREE, "wrong upstream tree")
    clean(repo)
    source_images(repo, pins, False)
    numstat = git(repo, "apply", "--numstat", "-z", str(patch), binary=True)
    changed = [entry.split(b"\t", 2)[2].decode() for entry in numstat.split(b"\0") if entry]
    require(len(changed) == len(set(changed)) and set(changed) == set(pins["changed_files"]),
            "patch source inventory mismatch")
    git(repo, "apply", "--check", "--index", "--binary", "--whitespace=error-all", str(patch))
    git(repo, "apply", "--index", "--binary", "--whitespace=error-all", str(patch))
    source_images(repo, pins, True)
    require(git(repo, "write-tree") == PATCHED_TREE, "full staged Airbender tree mismatch")
    status = git(repo, "status", "--porcelain=v1", "--untracked-files=all",
                 "--ignored=matching").splitlines()
    require(len(status) == len(pins["changed_files"])
            and all(line[:2] in {"A ", "M "} for line in status)
            and {line[3:] for line in status} == set(pins["changed_files"]),
            "unstaged or unrelated source before local commit")
    base_date = git(repo, "show", "-s", "--format=%cI", COMMIT)
    git(repo, "-c", "user.name=server-airbender", "-c", "user.email=server-airbender@local",
        "commit", "--no-gpg-sign", "-m", "Reviewed server Airbender security overlay",
        extra_env={"GIT_AUTHOR_DATE": base_date, "GIT_COMMITTER_DATE": base_date})
    revision = git(repo, "rev-parse", "HEAD")
    git(repo, "tag", "-f", TAG, revision)
    verify_checkout(repo, pins, revision)
    return repo, revision


def inputs(server):
    return {name: sha(server / name) for name in INPUT_PATHS}


def canonical_directory(value):
    path = Path(value)
    require(path.is_absolute() and not path.is_symlink() and path.resolve() == path
            and path.is_dir(), f"noncanonical directory: {path}")
    require(not any(char in str(path) for char in ("\n", "\r", "\0")), "invalid path")
    return path


def verify(server, workspace):
    pins = load_pins()
    record = document(workspace / RECORD)
    require(set(record) == {"schema_version", "server", "workspace", "airbender",
                            "patched_commit", "inputs", "workspace_inputs"},
            "unknown source-attestation fields")
    require(type(record["schema_version"]) is int and record["schema_version"] == 1
            and record["server"] == str(server) and record["workspace"] == str(workspace),
            "source-attestation route mismatch")
    require(record["inputs"] == inputs(server), "source or tooling input drift")
    require(record["workspace_inputs"] == {
        name: sha(workspace / name) for name in ("Cargo.toml", "Cargo.lock")
    }, "rewritten workspace input drift")
    repo = canonical_directory(record["airbender"])
    require(repo.parent == workspace.parent and repo.name.startswith("server-airbender-"),
            "unrelated Airbender checkout")
    revision = record["patched_commit"]
    require(isinstance(revision, str) and re.fullmatch(r"[0-9a-f]{40}", revision),
            "invalid attested commit")
    verify_checkout(repo, pins, revision)
    validate_manifest(tomllib.loads(regular(server / "Cargo.toml").decode()))
    validate_lock(tomllib.loads(regular(server / "Cargo.lock").decode()))
    validate_manifest(tomllib.loads(regular(workspace / "Cargo.toml").decode()), repo.as_uri())
    validate_lock(tomllib.loads(regular(workspace / "Cargo.lock").decode()),
                  f"git+{repo.as_uri()}?tag={TAG}#{revision}")


def prepare(server, workspace):
    require(server == ROOT and server != workspace and workspace not in server.parents,
            "preparation requires a separate disposable run workspace")
    require(not (workspace / RECORD).exists(), "workspace already has an Airbender record")
    pins = load_pins()
    before = inputs(server)
    original = {name: regular(workspace / name).decode() for name in ("Cargo.toml", "Cargo.lock")}
    validate_manifest(tomllib.loads(regular(server / "Cargo.toml").decode()))
    validate_lock(tomllib.loads(regular(server / "Cargo.lock").decode()))
    validate_manifest(tomllib.loads(original["Cargo.toml"]))
    validate_lock(tomllib.loads(original["Cargo.lock"]))
    source = os.environ.get("SERVER_AIRBENDER_SOURCE_DIR")
    repo, revision = prepare_checkout(
        workspace.parent, pins, MANIFEST.parent / pins["patch_file"],
        canonical_directory(source) if source else None,
    )
    toml, lock = rewrite(original["Cargo.toml"], original["Cargo.lock"], repo.as_uri(), revision)
    require(before == inputs(server), "source or tooling drift during preparation")
    require(all(regular(workspace / name).decode() == value for name, value in original.items()),
            "workspace drift during preparation")
    for name, value in (("Cargo.toml", toml), ("Cargo.lock", lock)):
        (workspace / name).write_text(value, encoding="utf-8")
    record = {
        "schema_version": 1, "server": str(server), "workspace": str(workspace),
        "airbender": str(repo), "patched_commit": revision, "inputs": before,
        "workspace_inputs": {name: sha(workspace / name) for name in original},
    }
    with (workspace / RECORD).open("x", encoding="utf-8") as out:
        json.dump(record, out, indent=2)
        out.write("\n")
    verify(server, workspace)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    server, workspace = canonical_directory(args.server), canonical_directory(args.workspace)
    require(server == ROOT, "tooling must belong to the selected server checkout")
    (verify if args.verify else prepare)(server, workspace)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"server Airbender preparation failed: {error}", file=sys.stderr)
        sys.exit(1)
