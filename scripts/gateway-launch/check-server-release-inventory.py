#!/usr/bin/env python3
"""Read-only fleet inventory: a sequencer update is not an EN update."""

import argparse
import ast
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


SCHEMA = "syscoin-server-release-inventory-v1"
CLASSES = {"main_node", "public_external_node", "debug_external_node"}
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
UNIT = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.@:-]*\.service\Z")
MAX_INPUT = 2 * 1024 * 1024


def require(condition, code):
    if not condition:
        raise ValueError(code)


def file_digest(path, max_size=512 * 1024 * 1024):
    require(Path(path).stat().st_size <= max_size, "file_digest_bound")
    result = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def source_fingerprint(root):
    # SYSCOIN: Inspect the stock helper's literal input lists without executing
    # source from the host being audited. Preserve old helper input closures.
    root = Path(root)
    runner = root / "scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
    require(runner.stat().st_size <= 64 * 1024, "stock_helper_bound")
    text = runner.read_text()
    match = re.search(r"prebuilt_source_digest\(\) \{.*?<<'PY'\n(.*?)\nPY\n\}", text, re.S)
    require(match is not None, "unsupported_stock_source_digest")
    assignments = {}
    for node in ast.parse(match.group(1)).body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id in {"rust_roots", "build_inputs", "excluded"}:
                assignments[target.id] = ast.literal_eval(node.value)
    require(set(assignments) == {"rust_roots", "build_inputs", "excluded"}, "missing_stock_inputs")
    require(assignments["rust_roots"] == ("lib", "node"), "unsupported_stock_roots")
    require(assignments["excluded"] == {".git", "target", ".cursor", ".gateway-launch"}, "unsupported_stock_exclusions")
    inputs = assignments["build_inputs"]
    require(isinstance(inputs, tuple) and len(inputs) == len(set(inputs)), "invalid_stock_inputs")
    require(all(isinstance(x, str) and not Path(x).is_absolute() and ".." not in Path(x).parts for x in inputs), "unsafe_stock_input")
    result = hashlib.sha256()
    count = size = 0

    def add(path):
        nonlocal count, size
        require(path.is_file() and not path.is_symlink(), "unsupported_source_file")
        count += 1
        size += path.stat().st_size
        require(count <= 4096 and size <= 128 * 1024 * 1024, "source_snapshot_bound")
        result.update(path.relative_to(root).as_posix().encode() + b"\0" + bytes.fromhex(file_digest(path)))

    for name in sorted(assignments["rust_roots"]):
        tree = root / name
        require(tree.is_dir() and not tree.is_symlink(), "unsupported_source_root")
        for parent, dirs, files in os.walk(tree):
            dirs[:] = sorted(x for x in dirs if x not in assignments["excluded"])
            require(not any((Path(parent) / x).is_symlink() for x in dirs), "symlinked_source_directory")
            for name in sorted(files):
                if name not in assignments["excluded"] and Path(name).suffix in {".rs", ".toml", ".lock"}:
                    add(Path(parent) / name)
    for name in sorted(inputs):
        add(root / name)
    return result.hexdigest()


def stock_context(binary, workspace, protocol, edge_target, gas_tank, source):
    return hashlib.sha256(b"".join(x.encode() + b"\0" for x in (binary, workspace, protocol, edge_target, gas_tank, source))).hexdigest()


def unit_state(unit):
    require(UNIT.fullmatch(unit) is not None, "invalid_unit")
    result = subprocess.run(["systemctl", "show", unit, "-p", "MainPID", "-p", "ActiveState", "-p", "SubState"], capture_output=True, text=True, timeout=5, check=True)
    return dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)


def capture_unit(unit, proc=Path("/proc"), get_state=unit_state):
    before = get_state(unit)
    require(before.get("ActiveState") == "active" and before.get("SubState") == "running", "unit_not_running")
    pid = before.get("MainPID", "")
    require(pid.isdigit() and int(pid) > 0, "unit_has_no_pid")
    process = proc / pid
    def start_time():
        # SYSCOIN: CPU counters change while reading a live process. Only its
        # immutable start time detects PID reuse, not the entire stat record.
        fields = (process / "stat").read_bytes().rsplit(b")", 1)[-1].split()
        require(len(fields) >= 20 and fields[19].isdigit(), "invalid_process_start")
        return fields[19]

    start = start_time()
    environment = dict(x.split(b"=", 1) for x in (process / "environ").read_bytes().split(b"\0") if b"=" in x)

    def env(name):
        return environment.get(name.encode(), b"").decode()

    source_root = Path(env("ZKSYNC_OS_SERVER_PATH"))
    require(source_root.is_absolute(), "missing_source_binding")
    gateway = Path(env("GATEWAY_DIR"))
    require(gateway.is_absolute(), "missing_workspace_binding")
    gateway = gateway.resolve(strict=True)
    exe = (process / "exe").resolve(strict=True)
    require(exe.name == "zksync-os-server" and exe.parent.name == "release", "unexpected_server_binary")
    workspace = exe.parent.parent.name
    require(exe == gateway / ".gateway-launch/target" / workspace / "release/zksync-os-server", "unexpected_workspace_binary")
    running = file_digest(process / "exe")
    on_disk = file_digest(exe)
    source = source_fingerprint(source_root)
    context = stock_context(on_disk, workspace, env("PROTOCOL_VERSION"), env("SYSCOIN_EDGE_DA_COMMIT_TARGET"), env("SYSCOIN_GAS_TANK_ADDRESS"), source)
    stamp = Path(str(exe) + ".sha256").read_text().strip()
    require(DIGEST.fullmatch(stamp) is not None, "malformed_stock_stamp")
    args = [x.decode() for x in (process / "cmdline").read_bytes().split(b"\0") if x]
    configs = []
    for i, arg in enumerate(args):
        if arg == "--config":
            require(i + 1 < len(args), "missing_config_argument")
            configs.extend(args[i + 1].split(":"))
        elif arg.startswith("--config="):
            configs.extend(arg.split("=", 1)[1].split(":"))
    require(configs and all(Path(p).is_absolute() for p in configs), "missing_explicit_config")
    config_hashes = [file_digest(p, MAX_INPUT) for p in configs]
    require(get_state(unit) == before and start_time() == start, "process_changed_during_capture")
    require(file_digest(process / "exe") == running and file_digest(exe) == on_disk, "binary_changed_during_capture")
    require(source_fingerprint(source_root) == source and [file_digest(p, MAX_INPUT) for p in configs] == config_hashes, "inputs_changed_during_capture")
    return {"unit": unit, "pid": int(pid), "active": True, "source_sha256": source, "running_elf_sha256": running, "on_disk_elf_sha256": on_disk, "stock_stamp_matches": context == stamp, "config_sha256": config_hashes}


def verify_inventory(manifest, snapshots, now=None, max_age=300):
    require(manifest.get("schema") == SCHEMA and DIGEST.fullmatch(manifest.get("source_sha256", "")), "invalid_release_manifest")
    coverage = manifest.get("role_classes", {})
    require(set(coverage) == CLASSES, "declare_all_role_classes")
    for value in coverage.values():
        require(isinstance(value, dict) and value.get("status") in {"deployed", "not_deployed"}, "invalid_role_class")
        require(value["status"] == "deployed" or bool(value.get("reason", "").strip()), "undeployed_role_requires_reason")
    required = manifest.get("instances", [])
    require(isinstance(required, list) and required, "empty_release_inventory")
    expected = {}
    for row in required:
        require(row.get("role_class") in CLASSES and coverage[row["role_class"]]["status"] == "deployed", "undeclared_instance_role")
        require(isinstance(row.get("host"), str) and row["host"] and UNIT.fullmatch(row.get("unit", "")), "invalid_instance")
        key = row["host"], row["unit"]
        require(key not in expected and DIGEST.fullmatch(row.get("binary_sha256", "")), "duplicate_or_unpinned_instance")
        configs = row.get("config_sha256", [])
        require(isinstance(configs, list) and configs and all(DIGEST.fullmatch(x) for x in configs), "unpinned_instance_config")
        expected[key] = row
    require({x["role_class"] for x in required} == {k for k, v in coverage.items() if v["status"] == "deployed"}, "deployed_role_has_no_instance")
    now = now or datetime.now(timezone.utc)
    observed = {}
    for snapshot in snapshots:
        require(snapshot.get("schema") == SCHEMA, "invalid_snapshot")
        at = datetime.fromisoformat(snapshot.get("observed_at", "").replace("Z", "+00:00"))
        require(at.tzinfo is not None and 0 <= (now - at).total_seconds() <= max_age, "stale_snapshot")
        require(not snapshot.get("errors"), "snapshot_capture_failed")
        for row in snapshot.get("instances", []):
            key = snapshot.get("host"), row.get("unit")
            require(key in expected, "unexpected_observed_instance")
            require(key not in observed, "duplicate_observation")
            observed[key] = row
    for key, target in expected.items():
        require(key in observed, "missing_required_instance")
        row = observed[key]
        require(row.get("active") is True and type(row.get("pid")) is int and row["pid"] > 0, "instance_not_active")
        require(row.get("source_sha256") == manifest["source_sha256"], "stale_instance_source")
        require(row.get("running_elf_sha256") == row.get("on_disk_elf_sha256") == target["binary_sha256"], "instance_binary_mismatch")
        require(row.get("stock_stamp_matches") is True, "instance_stock_stamp_mismatch")
        require(row.get("config_sha256") == target["config_sha256"], "instance_config_mismatch")
    return {"schema": SCHEMA, "release_inventory_matches": True, "verified_instances": len(expected), "semantic_acceptance": False}


def read_json(path):
    with Path(path).open("rb") as source:
        raw = source.read(MAX_INPUT + 1)
    require(len(raw) <= MAX_INPUT, "json_input_bound")
    return json.loads(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    capture = modes.add_parser("capture", help="read local running units; no SSH, RPC, restart or writes")
    capture.add_argument("--host", required=True, help="operator's stable fleet host label")
    capture.add_argument("--unit", required=True, action="append")
    verify = modes.add_parser("verify", help="verify every declared fleet instance, including ENs")
    verify.add_argument("--manifest", required=True)
    verify.add_argument("--snapshot", required=True, action="append")
    verify.add_argument("--max-age-seconds", type=int, default=300)
    args = parser.parse_args()
    try:
        if args.mode == "capture":
            require(len(args.unit) <= 32 and len(args.unit) == len(set(args.unit)), "invalid_capture_units")
            result = {"schema": SCHEMA, "observed_at": datetime.now(timezone.utc).isoformat(), "host": args.host, "instances": [capture_unit(unit) for unit in args.unit]}
        else:
            require(0 < args.max_age_seconds <= 3600 and len(args.snapshot) <= 32, "invalid_capture_bounds")
            result = verify_inventory(read_json(args.manifest), [read_json(x) for x in args.snapshot], max_age=args.max_age_seconds)
        print(json.dumps(result, sort_keys=True))
    except (ValueError, TypeError, AttributeError, KeyError, OSError, subprocess.SubprocessError) as error:
        # SYSCOIN: Files and process environments may contain credentials. Do
        # not forward exception text, subprocess output or arbitrary config.
        print(json.dumps({"schema": SCHEMA, "release_inventory_matches": False, "error": str(error) if type(error) is ValueError and re.fullmatch(r"[a-z_]+", str(error)) else type(error).__name__}))
        raise SystemExit(1)


if __name__ == "__main__":
    main()
