#!/usr/bin/env python3
"""SYSCOIN: Preserve the generated f638 edge context, changing only runner mode.

DATED Tanenbaum mock-testnet reference; never copy these inputs to mainnet.
Stage recipe only; execute after fresh canonical configs and normal edge
build-prebuilt/exec-prebuilt --help qualification. This neither installs nor
starts a service and never writes a build stamp or the canonical start script.
"""

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess


SOURCE = Path("/home/ubuntu/zksync-os-server-v32-public-20261006")
RUNTIME = Path("/home/ubuntu/gateway_v32_public_20261006")
DEST = Path("/home/ubuntu/v32-services-prep-20261006/generated")
COMMIT = "f638db92e5c5ea31507e089b61f122fd95cf9083"
SOURCE_HASHES = {
    "scripts/gateway-launch/generate-os-server-configs.sh":
        "2d577a78adbae9812b52f1103765bbe2a5468bfac75a7c844415f3ab2781746e",
    "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh":
        "34bba80585b9fdc26cc6b6165a493655400bf4018f4f1345148d7b5a052ab28d",
    "scripts/gateway-launch/_execute_operator_lock.sh":
        "7c426d363ca0aaa03c843e6779493628e6fed7f394460f14ecb1ab1f30ac3b89",
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def regular_owned(path, private=False):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_nlink != 1 or info.st_mode & 0o022):
        raise ValueError(f"unsafe owned regular file: {path}")
    if private and info.st_mode & 0o077:
        raise ValueError(f"private config is accessible by others: {path}")
    return path.read_bytes()


def adapt_edge(original, source=SOURCE, runtime=RUNTIME):
    # SYSCOIN: The replacement covers the exact terminal argv, not a broad
    # text substitution. Every preceding byte (including cookie + FD9 lock)
    # must survive unchanged. No config, context, app or stamp bypass is added.
    config = runtime / "os-server-configs/zksys/config.yaml"
    runner = source / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
    old = f'exec bash "{runner}" "zksys" -- run --release -- --config "{config}"\n'.encode()
    new = f'exec bash "{runner}" "zksys" -- exec-prebuilt -- --config "{config}"\n'.encode()
    anchors = (
        b"#!/usr/bin/env bash\nset -euo pipefail\n",
        f'cd "{source}"\n'.encode(),
        f'export GATEWAY_DIR="{runtime}"\n'.encode(),
        b'export GATEWAY_CHAIN_NAME="gateway"\n',
        b'export EDGE_CHAIN_NAME="zksys"\n',
        b'export EDGE_CHAIN_ID="57057"\n',
        b'export PROTOCOL_VERSION="v32.0"\n',
        b"resolve_syscoin_cookie_file() {\n",
        f'source "{source}/scripts/gateway-launch/_execute_operator_lock.sh"\n'.encode(),
        f'gateway_acquire_execute_operator_lock "zksys" "{config}"\n'.encode(),
    )
    if not original.startswith(anchors[0]):
        raise ValueError("generated edge script header changed")
    for anchor in anchors:
        if original.count(anchor) != 1:
            raise ValueError("generated edge script context/guard missing or ambiguous")
    if original.count(old) != 1 or not original.endswith(old):
        raise ValueError("exact canonical edge terminal argv changed")
    result = original[:-len(old)] + new
    if result[:-len(new)] != original[:-len(old)]:
        raise ValueError("edge adapter modified canonical prefix")
    return result


def exclusive_write(path, data, mode):
    # SYSCOIN: Never overwrite prior reviewed generation or follow a symlink.
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    with os.fdopen(fd, "wb") as handle:
        handle.write(data)


def main():
    if subprocess.check_output(["git", "-C", str(SOURCE), "rev-parse", "HEAD"], text=True).strip() != COMMIT:
        raise ValueError("server source is not the approved f638 deployment pin")
    for args in (["diff", "--quiet"], ["diff", "--cached", "--quiet"]):
        subprocess.run(["git", "-C", str(SOURCE), *args], check=True)
    for name, expected in SOURCE_HASHES.items():
        if digest((SOURCE / name).read_bytes()) != expected:
            raise ValueError(f"canonical source hash changed: {name}")
    original_path = RUNTIME / "os-server-configs/zksys/start-node.sh"
    config = RUNTIME / "os-server-configs/zksys/config.yaml"
    original = regular_owned(original_path)
    regular_owned(config, private=True)
    for name in ("contracts.yaml", "wallets.yaml", "genesis.json"):
        regular_owned(config.parent / name, private=name == "wallets.yaml")
    for directory in (DEST.parent, DEST):
        if not directory.exists():
            directory.mkdir(mode=0o700)
        info = directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
            raise ValueError(f"unsafe private output directory: {directory}")
    adapter = adapt_edge(original)
    subprocess.run(["bash", "-n"], input=adapter, check=True)
    output = DEST / "zksys-start-node-prebuilt.sh"
    manifest = {
        "status": "generated-not-installed-not-started",
        "source_commit": COMMIT,
        "canonical_script_path": str(original_path),
        "canonical_script_sha256": digest(original),
        "adapter_path": str(output),
        "adapter_sha256": digest(adapter),
        "generator_recipe_sha256": digest(Path(__file__).read_bytes()),
        "canonical_source_sha256": SOURCE_HASHES,
        "config_path": str(config),
        "change": "exact final runner mode only: run --release -- to exec-prebuilt --",
        "guards": "canonical prefix retained byte-identically; normal source/binary/app stamp validation and execute-operator lock retained",
        "required_prior_gate": "normal config-bound edge build-prebuilt and exec-prebuilt -- --help qualification; parent reviews receipt/config identity before installation",
    }
    exclusive_write(DEST / "zksys-start-node-canonical.sh", original, 0o700)
    exclusive_write(output, adapter, 0o700)
    exclusive_write(DEST / "edge-adapter-generation.json",
                    (json.dumps(manifest, indent=2) + "\n").encode(), 0o600)
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
