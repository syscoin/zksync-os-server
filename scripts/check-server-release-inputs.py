#!/usr/bin/env python3
"""Reject unreviewed source or incomplete workspace versions before releasing."""

import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("native_audit", ROOT / "scripts/check-native-audit.py")
NATIVE_AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE_AUDIT)


def validate(root):
    root = Path(root)
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    version = workspace["package"]["version"]
    if version != json.loads((root / ".github/release-please/manifest.json").read_text())["."]:
        raise ValueError("release manifest and workspace version differ")
    packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    seen = set()
    for member in workspace["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text())["package"]
        name = package["name"]
        if name in seen:
            raise ValueError("duplicate workspace package: " + name)
        seen.add(name)
        expected = version if package["version"] == {"workspace": True} else package["version"]
        matches = [p for p in packages if p["name"] == name and "source" not in p]
        if len(matches) != 1 or matches[0]["version"] != expected:
            raise ValueError("Cargo.lock workspace package version differs for " + name)
    policy = json.loads((root / NATIVE_AUDIT.POLICY_PATH).read_text())
    if policy.get("schema") != 1 or policy.get("target") != NATIVE_AUDIT.TARGET:
        raise ValueError("unrecognized native audit review scope")
    lock_sha = hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest()
    if policy.get("lock_sha256") != lock_sha:
        raise ValueError("native audit lock review is stale")
    if policy.get("source_sha256") != NATIVE_AUDIT.source_fingerprint(root):
        raise ValueError("native audit source review is stale")


def fixture_ready(root):
    fixture = Path(root) / "local-chains/v32.0"
    marker = fixture / "CANONICAL_V8_REGENERATION_REQUIRED"
    if fixture.is_symlink() or (fixture / "default").is_symlink() or marker.exists() or marker.is_symlink():
        return False
    required = (fixture / "l1-state.json.gz", fixture / "default/config.yaml",
                fixture / "default/genesis.json")
    return all(path.is_file() and not path.is_symlink() for path in required)


def main():
    try:
        if sys.argv[1:] == ["--fixture-ready"]:
            print("true" if fixture_ready(ROOT) else "false")
            return 0
        if sys.argv[1:]:
            raise ValueError("usage: check-server-release-inputs.py [--fixture-ready]")
        validate(ROOT)
    except (OSError, ValueError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        print("Release inputs invalid: " + str(error), file=sys.stderr)
        return 1
    print("Release versions and reviewed native audit inputs agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
