#!/usr/bin/env python3
"""Resolve the protocol and patched zksync-os build target for run_local.sh."""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import List, Optional, Tuple


ADDRESS_RE = re.compile(r"^0x[0-9a-fA-F]{40}$")
# The checked-in V7 app binaries were built with this patched source constant.
# Older runtime fixtures still compile V7 into the multi-version server and must
# use matching native source even though they do not execute that VM version.
PUBLISHED_PATCH_TARGET = "0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19"


class ResolutionError(Exception):
    pass


def scalar(value: str) -> str:
    value = value.split("#", 1)[0].strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def nested_scalar(path: Path, section: str, key: str) -> Optional[str]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as err:
        raise ResolutionError(f"failed to read {path}: {err}") from err

    section_indent: Optional[int] = None
    section_re = re.compile(rf"^(\s*){re.escape(section)}\s*:\s*(?:#.*)?$")
    key_re = re.compile(rf"^(\s+){re.escape(key)}\s*:\s*(.*?)\s*$")
    for line in lines:
        if section_indent is None:
            match = section_re.match(line)
            if match:
                section_indent = len(match.group(1))
            continue

        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if indent <= section_indent:
            break
        match = key_re.match(line)
        if match and len(match.group(1)) > section_indent:
            value = scalar(match.group(2))
            return value or None
    return None


def normalize_address(value: str, source: str) -> str:
    value = value.strip().lower()
    if not ADDRESS_RE.fullmatch(value):
        raise ResolutionError(f"{source} must be a 20-byte hex address, got {value!r}")
    if value == "0x" + "0" * 40:
        raise ResolutionError(f"{source} must be nonzero")
    return value


def protocol_version(config_dir: Path) -> str:
    versions = config_dir.parent / "versions.yaml"
    if not versions.is_file():
        directory_version = config_dir.parent.name
        return directory_version if re.fullmatch(r"v\d+\.\d+", directory_version) else ""
    protocol = nested_scalar(versions, "general", "protocol_version")
    if protocol:
        if not re.fullmatch(r"v\d+\.\d+", protocol):
            raise ResolutionError(
                f"invalid general.protocol_version in {versions}: {protocol!r}"
            )
        return protocol
    os_version = nested_scalar(versions, "general", "zksync_os_version")
    if os_version and os_version.startswith("0."):
        return "v" + os_version[2:]
    return ""


def selected_configs(config_dir: Path) -> List[Path]:
    single = config_dir / "config.yaml"
    if single.is_file():
        return [single]
    configs = sorted(config_dir.glob("chain_*.yaml"), key=lambda path: path.name)
    if not configs:
        raise ResolutionError(f"no config.yaml or chain_*.yaml files found in {config_dir}")
    return configs


def chain_id(config: Path) -> str:
    value = nested_scalar(config, "genesis", "chain_id")
    if value is None or not value.isdecimal():
        raise ResolutionError(f"missing or invalid genesis.chain_id in {config}")
    return str(int(value, 10))


def contracts_path(config_dir: Path, chain: str) -> Optional[Path]:
    candidates = (
        config_dir / f"contracts_{chain}.yaml",
        config_dir.parent / "multi_chain" / f"contracts_{chain}.yaml",
    )
    return next((path for path in candidates if path.is_file()), None)


def target_from_contracts(path: Path) -> str:
    values: List[Tuple[str, str]] = []
    for section in ("ecosystem_contracts", "l1"):
        value = nested_scalar(path, section, "validator_timelock_addr")
        if value is not None:
            label = f"{path}:{section}.validator_timelock_addr"
            values.append((label, normalize_address(value, label)))
    if not values:
        raise ResolutionError(f"missing validator_timelock_addr in {path}")
    unique = {value for _, value in values}
    if len(unique) != 1:
        details = ", ".join(f"{label}={value}" for label, value in values)
        raise ResolutionError(f"validator timelock addresses disagree: {details}")
    return values[0][1]


def explicit_target() -> Optional[str]:
    values: List[Tuple[str, str]] = []
    for name in (
        "SYSCOIN_EDGE_DA_COMMIT_TARGET",
        "ZKSYNC_OS_SYSCOIN_EDGE_DA_COMMIT_TARGET",
    ):
        value = os.environ.get(name, "").strip()
        if value:
            values.append((name, normalize_address(value, name)))
    if not values:
        return None
    unique = {value for _, value in values}
    if len(unique) != 1:
        details = ", ".join(f"{name}={value}" for name, value in values)
        raise ResolutionError(f"explicit validator timelock addresses disagree: {details}")
    return values[0][1]


def patched_target(config_dir: Path) -> str:
    explicit = explicit_target()
    derived: List[Tuple[Path, str]] = []
    missing: List[str] = []
    for config in selected_configs(config_dir):
        chain = chain_id(config)
        contracts = contracts_path(config_dir, chain)
        if contracts is None:
            missing.append(chain)
            continue
        derived.append((contracts, target_from_contracts(contracts)))

    unique = {value for _, value in derived}
    if len(unique) > 1:
        details = ", ".join(f"{path}={value}" for path, value in derived)
        raise ResolutionError(f"selected chains use different validator timelocks: {details}")
    if explicit is not None and unique and explicit not in unique:
        raise ResolutionError(
            "SYSCOIN_EDGE_DA_COMMIT_TARGET does not match the selected fixture "
            f"validator timelock {next(iter(unique))}"
        )
    if explicit is not None:
        return explicit
    if missing:
        chains = ", ".join(missing)
        raise ResolutionError(
            f"missing contracts_<chain-id>.yaml for v31 chain(s) {chains}; set "
            "SYSCOIN_EDGE_DA_COMMIT_TARGET to the deployed validator timelock"
        )
    if not unique:
        raise ResolutionError(
            "could not derive the v31 validator timelock; set "
            "SYSCOIN_EDGE_DA_COMMIT_TARGET explicitly"
        )
    return next(iter(unique))


def main() -> None:
    if len(sys.argv) != 2:
        raise ResolutionError(f"usage: {Path(sys.argv[0]).name} <config-directory>")
    config_dir = Path(sys.argv[1]).resolve()
    protocol = protocol_version(config_dir)
    if protocol.startswith("v31."):
        target = patched_target(config_dir)
    else:
        target = explicit_target() or PUBLISHED_PATCH_TARGET
    print(f"{protocol}\t{target}")


if __name__ == "__main__":
    try:
        main()
    except ResolutionError as err:
        raise SystemExit(f"error: {err}") from err
