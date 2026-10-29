#!/usr/bin/env python3
"""Apply source-reviewed advisory dispositions only to the pinned Linux server."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import subprocess
import sys
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
POLICY_PATH = "scripts/security/native-linux-review.json"
POLICY_FILES = {
    POLICY_PATH,
    "scripts/check-native-audit.py",
    "scripts/tests/test_native_audit.py",
}
LEGACY_AUDIT_IGNORES = {
    "RUSTSEC-2025-0055", "RUSTSEC-2023-0071", "RUSTSEC-2026-0235",
    "RUSTSEC-2026-0381", "RUSTSEC-2026-0383",
}
DATABASE_URL = "https://github.com/RustSec/advisory-db"
TOOL_VERSIONS = {"audit": "0.22.0", "deny": "0.20.2"}
LIBZ_ADVISORY = "RUSTSEC-2026-0418"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def run(command, root=ROOT):
    result = subprocess.run(command, cwd=root, text=True, capture_output=True)
    if result.returncode:
        raise ValueError(f"{shlex.join(map(str, command))} failed:\n{result.stderr}")
    return result.stdout


def source_fingerprint(root):
    # Bind callers and non-Rust build inputs as well as dependencies: an existing
    # re-export can expose a newly used vulnerable API without changing Cargo.lock.
    tracked = run(["git", "ls-files", "--stage", "-z"], root).split("\0")
    entries = {}
    for entry in filter(None, tracked):
        header, name = entry.split("\t", 1)
        mode, revision, stage = header.split()
        if stage != "0":
            raise ValueError("unmerged source cannot use advisory exceptions")
        entries[name] = (mode, revision)
    for name in filter(None, run(["git", "ls-files", "--others", "--exclude-standard", "-z"], root).split("\0")):
        entries[name] = ("untracked", "")
    result = hashlib.sha256()
    for name, (mode, revision) in sorted(entries.items()):
        if (name in POLICY_FILES or name.lower().endswith(".md")
                or "__pycache__" in Path(name).parts):
            continue
        path = root / name
        if mode == "160000":
            data = revision.encode()
        elif path.is_symlink():
            data = os.readlink(path).encode()
        else:
            data = path.read_bytes()
        result.update(name.encode() + b"\0" + mode.encode() + b"\0" + hashlib.sha256(data).digest())
    return result.hexdigest()


def graph_fingerprint(metadata, root):
    packages = {}
    for package in metadata["packages"]:
        source = package["source"]
        if source is None:
            try:
                relative = Path(package["manifest_path"]).relative_to(root).as_posix()
            except ValueError as error:
                raise ValueError("unreviewed dependency outside the workspace") from error
            source = "workspace:" + relative
        packages[package["id"]] = (package["name"], package["version"], source)
    nodes = []
    for node in metadata["resolve"]["nodes"]:
        dependencies = sorted(
            (dep["name"], packages[dep["pkg"]],
             sorted((kind.get("kind") or "normal", kind.get("target") or "")
                    for kind in dep["dep_kinds"]))
            for dep in node["deps"]
        )
        nodes.append((packages[node["id"]], sorted(node["features"]), dependencies))
    return digest(json.dumps(sorted(nodes), separators=(",", ":")).encode())


def validate_host(environ, system, machine):
    if system != "Linux" or machine not in {"x86_64", "AMD64"}:
        raise ValueError(f"advisory exceptions apply only to {TARGET}")
    for name, value in environ.items():
        if name == "RUSTFLAGS" and shlex.split(value) == ["--cfg", "tokio_unstable"]:
            continue
        if name == "CARGO_ENCODED_RUSTFLAGS" and value.split("\x1f") == ["--cfg", "tokio_unstable"]:
            continue
        if name == "CARGO_BUILD_TARGET" and value == TARGET:
            continue
        if (name in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET", "RUSTC",
                     "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS",
                     "ROCKSDB_LIB_DIR", "ROCKSDB_STATIC"}
                or name.startswith("ZSTD")
                or name.startswith("LIBZ_SYS_")
                or re.fullmatch(r"(?:HOST_|TARGET_)?(?:CC|CXX|CFLAGS|CXXFLAGS|AR|ARFLAGS)(?:_.*)?", name)
                or name.startswith("CARGO_PROFILE_")
                or re.fullmatch(r"CARGO_TARGET_.*_(?:RUSTFLAGS|LINKER)", name)):
            raise ValueError(f"unreviewed compiler/native-library override: {name}")


def validate_packages(lock, policy):
    for name, expected in policy["packages"].items():
        matches = [p for p in lock["package"] if p["name"] == name]
        if len(matches) != 1 or any(matches[0].get(k) != expected[k]
                                    for k in ("version", "source", "checksum")):
            raise ValueError(f"{name}: unreviewed package identity")


def validate_context(root, policy, metadata):
    if policy["schema"] != 1 or policy["target"] != TARGET:
        raise ValueError("unsupported review scope")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    validate_packages(lock, policy)
    validate_libz_scope(metadata, policy)
    actual = {
        "lock_sha256": digest((root / "Cargo.lock").read_bytes()),
        "source_sha256": source_fingerprint(root),
        "graph_sha256": graph_fingerprint(metadata, root),
    }
    for key, value in actual.items():
        if value != policy[key]:
            raise ValueError(f"{key}: review is stale (actual {value}); re-review before updating it")


def validate_libz_scope(metadata, policy):
    try:
        _validate_libz_scope(metadata, policy)
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError(f"{LIBZ_ADVISORY}: malformed dependency metadata") from error


def _validate_libz_scope(metadata, policy):
    if LIBZ_ADVISORY not in policy["advisories"]:
        return
    expected = {
        "libz-sys": ("1.1.22", "8b70e7a7df205e92a1a4cd9aaae7898dac0aa555503cc0a649494d0d60e7651d"),
        "cc": ("1.2.62", "a1dce859f0832a7d088c4f1119888ab94ef4b5d6795d1ce05afb7fe159d79f98"),
    }
    ids = {}
    for name, (version, checksum) in expected.items():
        if policy["packages"].get(name) != {"version": version, "source": REGISTRY, "checksum": checksum}:
            raise ValueError(f"{LIBZ_ADVISORY}: unreviewed {name} policy identity")
        packages = [p for p in metadata["packages"] if p["name"] == name]
        if len(packages) != 1 or packages[0]["version"] != version or packages[0]["source"] != REGISTRY:
            raise ValueError(f"{LIBZ_ADVISORY}: unreviewed {name} graph identity")
        ids[name] = packages[0]["id"]
    nodes = metadata["resolve"]["nodes"]
    libz_nodes = [n for n in nodes if n["id"] == ids["libz-sys"]]
    if len(libz_nodes) != 1 or sum(n["id"] == ids["cc"] for n in nodes) != 1:
        raise ValueError(f"{LIBZ_ADVISORY}: incomplete dependency graph")
    libz = libz_nodes[0]
    if "static" not in libz["features"]:
        raise ValueError(f"{LIBZ_ADVISORY}: static feature missing")
    cc_edges = [d for d in libz["deps"] if d["name"] == "cc"]
    if (len(cc_edges) != 1 or cc_edges[0]["pkg"] != ids["cc"]
            or cc_edges[0]["dep_kinds"] != [{"kind": "build", "target": None}]):
        raise ValueError(f"{LIBZ_ADVISORY}: unreviewed cc build dependency")


def validate_libz_server_tree(tree):
    lines = [line for line in tree.splitlines() if line.startswith("libz-sys ")]
    if not lines:
        raise ValueError(f"{LIBZ_ADVISORY}: server feature tree lacks libz-sys")
    for line in lines:
        match = re.fullmatch(r"libz-sys v1\.1\.22 \[([^]]*)\](?: \(\*\))?", line)
        if not match or set(filter(None, match.group(1).split(","))) != {"static"}:
            raise ValueError(f"{LIBZ_ADVISORY}: unreviewed server libz-sys features")


def validate_libz_server_builds(root, policy):
    if LIBZ_ADVISORY not in policy["advisories"]:
        return
    command = ["cargo", "tree", "--locked", "--target", TARGET, "--package", "zksync_os_server",
               "-e", "normal,build", "--prefix", "none", "--format", "{p} [{f}]"]
    for flags in ([], ["--features", "gcp"]):
        validate_libz_server_tree(run(command + flags, root))


def validate_advisories(database, policy):
    for advisory_id, review in policy["advisories"].items():
        path = database / "crates" / review["package"] / f"{advisory_id}.md"
        try:
            actual = digest(path.read_bytes())
        except OSError as error:
            raise ValueError(f"{advisory_id}: reviewed advisory is missing") from error
        if not review["rationale"] or actual != review["sha256"]:
            raise ValueError(f"{advisory_id}: advisory contents changed; re-review required")


def _filter_report(report, policy, scanner, legacy_ignores):
    settings = report["settings"]
    if not isinstance(settings["ignore"], list) or not all(isinstance(value, str) for value in settings["ignore"]):
        raise ValueError("malformed scanner settings")
    ignored = set(settings["ignore"])
    if ignored - legacy_ignores or ignored.intersection(policy["advisories"]):
        raise ValueError("unexpected global advisory ignore")
    vulnerabilities = report["vulnerabilities"]
    if scanner == "audit":
        findings = vulnerabilities["list"]
        if (not isinstance(findings, list)
                or type(vulnerabilities["found"]) is not bool
                or type(vulnerabilities["count"]) is not int
                or vulnerabilities["count"] != len(findings)
                or vulnerabilities["found"] != bool(findings)):
            raise ValueError("inconsistent audit report")
        findings = list(findings)
    elif scanner == "deny" and isinstance(vulnerabilities, list):
        findings = list(vulnerabilities)
    else:
        raise ValueError("unexpected scanner report")
    if not isinstance(findings, list) or not isinstance(report["warnings"], dict):
        raise ValueError("malformed scanner findings")
    for kind, warnings in report["warnings"].items():
        if (kind not in {"unsound", "unmaintained", "notice", "yanked"}
                or not isinstance(warnings, list)
                or not all(isinstance(warning, dict) for warning in warnings)):
            raise ValueError(f"unknown warning category: {kind}")
        if kind == "unsound":
            findings.extend(warnings)
    accepted, remaining = [], []
    for finding in findings:
        advisory = finding["advisory"]
        package = finding["package"]
        advisory_id = advisory["id"]
        if advisory_id in ignored:
            continue
        review = policy["advisories"].get(advisory_id)
        expected = policy["packages"].get(package["name"])
        if review is not None:
            if (review["package"] != package["name"] or expected is None
                    or advisory["package"] != package["name"]):
                raise ValueError(f"{advisory_id}: unexpected package")
            if any(package.get(k) != expected[k] for k in ("version", "source")):
                raise ValueError(f"{advisory_id}: unexpected package identity")
            checksum = package.get("checksum")
            if checksum != expected["checksum"] and not (scanner == "deny" and checksum is None):
                raise ValueError(f"{advisory_id}: unexpected package checksum")
            accepted.append(finding)
        else:
            remaining.append(finding)
    return accepted, remaining


def filter_report(report, policy, scanner, legacy_ignores):
    try:
        return _filter_report(report, policy, scanner, legacy_ignores)
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError("malformed scanner report") from error


def deny_diagnostics(stderr):
    for line in stderr.splitlines():
        item = json.loads(line)
        fields = item["fields"]
        if (fields.get("level") == "ERROR"
                or (fields.get("severity") == "error"
                    and fields.get("code") not in {"vulnerability", "unsound"})):
            raise ValueError(f"cargo-deny operational error: {fields}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scanner", choices=["audit", "deny", "fingerprints"])
    args = parser.parse_args()
    policy = json.loads((ROOT / POLICY_PATH).read_text())
    metadata = json.loads(run([
        "cargo", "metadata", "--locked", "--all-features", "--filter-platform", TARGET,
        "--format-version", "1",
    ]))
    if args.scanner == "fingerprints":
        print(json.dumps({
            "lock_sha256": digest((ROOT / "Cargo.lock").read_bytes()),
            "source_sha256": source_fingerprint(ROOT),
            "graph_sha256": graph_fingerprint(metadata, ROOT),
        }, indent=2))
        return
    validate_host(os.environ, platform.system(), platform.machine())
    validate_context(ROOT, policy, metadata)
    validate_libz_server_builds(ROOT, policy)
    compiler = run(["rustc", "--version", "--verbose"])
    if f"commit-hash: {policy['rustc_commit']}\n" not in compiler:
        raise ValueError("compiler differs from the reviewed toolchain")
    run([sys.executable, "scripts/check-root-compat.py"])
    base = tomllib.loads((ROOT / "deny.toml").read_text())
    legacy = LEGACY_AUDIT_IGNORES | set(base["advisories"]["ignore"])
    if legacy.intersection(policy["advisories"]):
        raise ValueError("reviewed native exceptions must not be global ignores")
    version = run(["cargo", args.scanner, "--version"])
    if not re.search(rf"\b{re.escape(TOOL_VERSIONS[args.scanner])}\s*$", version):
        raise ValueError(f"unreviewed cargo-{args.scanner} version: {version.strip()}")
    output = ROOT / "target/security-audit"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="zksys-native-audit-") as temporary:
        temporary = Path(temporary)
        metadata_path = temporary / "metadata.json"
        metadata_path.write_text(json.dumps(metadata))
        config = temporary / "deny.toml"
        config.write_text((ROOT / "deny.toml").read_text().replace(
            "[advisories]\n", f'[advisories]\ndb-path = "{temporary / "databases"}"\n', 1,
        ))
        if args.scanner == "audit":
            database = temporary / "advisory-db"
            run(["git", "clone", "--quiet", "--depth", "1", DATABASE_URL, str(database)])
        else:
            run(["cargo", "deny", "--config", str(config), "fetch", "db"])
            databases = list((temporary / "databases").glob("*/crates"))
            if len(databases) != 1:
                raise ValueError("expected exactly one advisory database")
            database = databases[0].parent
        validate_advisories(database, policy)
        revision = run(["git", "rev-parse", "HEAD"], database).strip()
        if args.scanner == "audit":
            command = ["cargo", "audit", "--json", "--file", "Cargo.lock", "--db", str(database),
                       "--no-fetch", "--target-arch", "x86_64", "--target-os", "linux"]
            for advisory_id in sorted(legacy):
                command.extend(["--ignore", advisory_id])
        else:
            common = ["cargo", "deny", "--format", "json", "--metadata-path", str(metadata_path),
                      "--config", str(config), "--target", TARGET, "--all-features", "--locked", "--offline"]
            # Advisory dispositions cannot suppress independent source, license or ban failures.
            run(common + ["check", "bans", "licenses", "sources", "--allow", "unmaintained"])
            command = common + ["check", "advisories", "--allow", "unmaintained", "--audit-compatible-output"]
        result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
        (output / f"{args.scanner}-raw.json").write_text(result.stdout)
        (output / f"{args.scanner}-stderr.log").write_text(result.stderr)
        if result.returncode not in {0, 1}:
            raise ValueError(f"scanner failed with status {result.returncode}: {result.stderr}")
        if args.scanner == "deny":
            deny_diagnostics(result.stderr)
        accepted, remaining = filter_report(json.loads(result.stdout), policy, args.scanner, legacy)
        if result.returncode and not (accepted or remaining):
            raise ValueError("scanner failed without a vulnerability/unsound finding")
        summary = {
            "target": TARGET,
            "source_commit": run(["git", "rev-parse", "HEAD"]).strip(),
            "source_sha256": policy["source_sha256"],
            "graph_sha256": policy["graph_sha256"],
            "advisory_db": revision,
            "accepted": [],
            "remaining": remaining,
        }
        for finding in accepted:
            advisory_id = finding["advisory"]["id"]
            entry = {"id": advisory_id, "package": finding["package"]["name"],
                     "version": finding["package"]["version"],
                     "rationale": policy["advisories"][advisory_id]["rationale"]}
            summary["accepted"].append(entry)
            print(f"REVIEWED {advisory_id}: {entry['package']} {entry['version']}: {entry['rationale']}")
        (output / f"{args.scanner}-review.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"Reviewed scope: {TARGET}; advisory database: {revision}")
        if remaining:
            raise ValueError(f"{len(remaining)} unreviewed vulnerability/unsound finding(s); see raw report")
        validate_context(ROOT, policy, metadata)
        validate_advisories(database, policy)
        print(f"PASS: {len(accepted)} scoped dispositions; no unreviewed vulnerability/unsound findings")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(f"Native Linux audit failed: {error}", file=sys.stderr)
        sys.exit(1)
