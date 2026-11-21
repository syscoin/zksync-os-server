#!/usr/bin/env python3
"""Restore the authenticated V32 fixture into an owned Linux runtime."""

import argparse
import base64
from contextlib import ExitStack, contextmanager
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import re
import select
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tarfile
import time


CHAIN_ROLES = ("gateway", "edge-6565", "edge-6566")
SNAPSHOTS = {
    "core_consensus": "core",
    "nevm_database": "nevm",
    "gateway_database": "gateway",
    "edge6565_database": "edge-6565",
    "edge6566_database": "edge-6566",
}
TOOLS = {"syscoind", "syscoin_cli", "syscoin_geth", "server", "fri_prover", "snark_prover",
         "restore_supervisor"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def exact(row, keys):
    require(type(row) is dict and set(row) == set(keys.split()), "unexpected record fields")
    return row


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def decode(raw):
    return json.loads(raw, object_pairs_hook=unique)


def relative(value):
    require(isinstance(value, str) and bool(value), "missing relative artifact path")
    path = PurePosixPath(value)
    require(not path.is_absolute() and path.as_posix() == value
            and bool(path.parts) and ".." not in path.parts and "\\" not in value,
            "unsafe artifact path")
    return path


def sha(value):
    require(isinstance(value, str) and re.fullmatch("[a-f0-9]{64}", value), "invalid digest")
    return value


def hash32(value):
    require(isinstance(value, str) and re.fullmatch("0x[a-f0-9]{64}", value), "invalid chain hash")
    return value


def number(value, minimum=0):
    require(type(value) is int and minimum <= value <= 2**63 - 1, "invalid nonnegative integer")
    return value


def directory(path, private=False):
    info = path.lstat()
    require(path.is_absolute() and path.resolve() == path and stat.S_ISDIR(info.st_mode),
            "directory must have an absolute, canonical path")
    if private:
        require(info.st_uid == os.getuid() and not info.st_mode & 0o077,
                "directory must be owner-only")


@contextmanager
def opened(root, name, private=False):
    path = root / relative(name)
    require(path.resolve() == path, "artifact path traverses a symlink")
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode), "artifact is not a regular file")
        if private:
            require(info.st_uid == os.getuid() and not info.st_mode & 0o077,
                    "private config must be owner-only")
        yield stream


@contextmanager
def verified(root, identity, private=False):
    exact(identity, "path size sha256")
    with opened(root, identity["path"], private) as stream:
        info = os.fstat(stream.fileno())
        require(info.st_size == number(identity["size"]) and
                hashlib.file_digest(stream, "sha256").hexdigest() == sha(identity["sha256"]),
                "artifact size or digest differs")
        stream.seek(0)
        yield stream


def verified_json(root, identity):
    with verified(root, identity) as stream:
        raw = stream.read(1 << 20)
        require(not stream.read(1), "record is too large")
        return decode(raw)


def by_role(rows, roles):
    result = {}
    for row in rows:
        exact(row, "role file")
        require(row["role"] in roles and row["role"] not in result, "invalid or duplicate role")
        result[row["role"]] = row["file"]
    require(result.keys() == roles, "missing artifact role")
    return result


def extract(root, identity, target, limits, role):
    exact(limits, "files extracted_bytes")
    expected_files = number(limits["files"], 1)
    expected_bytes = number(limits["extracted_bytes"], 1)
    target.mkdir(mode=0o700)
    seen = set()
    files = total = 0
    with verified(root, identity) as stream, tarfile.open(fileobj=stream, mode="r|*") as archive:
        for entry in archive:
            name = relative(entry.name)
            require(name not in seen and len(seen) < expected_files * 2 + 64, "invalid archive inventory")
            seen.add(name)
            require(entry.isdir() or (entry.isfile() and not entry.issparse()),
                    "archive contains a link, device, or unsupported entry")
            require(not {"LOCK", ".cookie", "geth.ipc", "syscoin.conf", "settings.json",
                         "config.json", "genesis.json"}.intersection(name.parts),
                    "archive contains runtime or configuration state")
            if role in CHAIN_ROLES:
                require(name.parts[0] in {"rocksdb", "proof_storage", "replay_archive"},
                        "rollup archive contains an unexpected directory")
            path = target / name
            if entry.isdir():
                path.mkdir(mode=0o700, parents=True, exist_ok=True)
                continue
            files += 1
            total += entry.size
            require(files <= expected_files and 0 <= total <= expected_bytes, "archive exceeds manifest")
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with archive.extractfile(entry) as source, path.open("xb") as out:
                shutil.copyfileobj(source, out)
    require((files, total) == (expected_files, expected_bytes), "archive differs from extraction manifest")
    if role in CHAIN_ROLES:
        require((target / "rocksdb").is_dir(), "rollup RocksDB is missing")


def stage_configs(private, receipt, run):
    exact(receipt, "schema core_wallet_name nevm_miner roles")
    require(receipt["schema"] == "syscoin-v32-canonical-private-configs-v1",
            "wrong deployment record schema")
    require(re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", receipt["core_wallet_name"]), "invalid DA wallet name")
    require(re.fullmatch("0x[a-fA-F0-9]{40}", receipt["nevm_miner"]), "invalid NEVM miner")
    exact(receipt["roles"], " ".join(CHAIN_ROLES))
    for role in CHAIN_ROLES:
        for kind, output, name in (("config", "config.json", f"{role}.json"),
                                   ("genesis", "genesis.json", f"{role}.genesis.json")):
            identity = exact(receipt["roles"][role], "config genesis")[kind]
            require(identity["path"] == name, "unexpected private config filename")
            with verified(private, identity, private=True) as stream:
                raw = stream.read(64 << 20)
                require(not stream.read(1) and type(decode(raw)) is dict, "invalid private config")
            with (run / role / output).open("xb") as out:
                out.write(raw)


def process_start(pid):
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def listener_owned(pid, port):
    matches = []
    for name in ("tcp", "tcp6"):
        for row in Path(f"/proc/net/{name}").read_text().splitlines()[1:]:
            fields = row.split()
            if fields[3] == "0A" and fields[1].endswith(f":{port:04X}"):
                matches.append((fields[1], fields[9]))
    if len(matches) != 1 or matches[0][0] != f"0100007F:{port:04X}":
        return False
    socket_link = f"socket:[{matches[0][1]}]"
    for fd in Path(f"/proc/{pid}/fd").iterdir():
        try:
            if os.readlink(fd) == socket_link:
                return True
        except FileNotFoundError:
            continue
    return False


class Child:
    def __init__(self, name, binary, argv, ports, run):
        self.name, self.binary, self.ports = name, binary, ports
        with (run / f"{name}.log").open("xb") as log:
            self.process = subprocess.Popen(
                [binary, *argv], stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                cwd=run, start_new_session=True, env={"HOME": str(run), "PATH": "/usr/bin:/bin",
                                                     "LC_ALL": "C"},
            )
        self.start = process_start(self.process.pid)

    def owned(self, listeners=True):
        pid = self.process.pid
        require(self.process.poll() is None and process_start(pid) == self.start
                and os.readlink(f"/proc/{pid}/exe") == str(self.binary),
                f"owned {self.name} process identity changed")
        return not listeners or all(listener_owned(pid, port) for port in self.ports)

    def stop(self):
        if self.process.poll() is None:
            self.owned(listeners=False)
            self.process.send_signal(signal.SIGINT)
            try:
                self.process.wait(timeout=40)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                try:
                    self.process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
        require(self.process.returncode == 0, f"owned {self.name} failed clean shutdown")


class RpcError(ValueError):
    pass


def rpc(child, method, params, auth=None):
    require(child.owned(), f"owned {child.name} listener missing")
    connection = http.client.HTTPConnection("127.0.0.1", child.ports[0], timeout=3)
    headers = {"Content-Type": "application/json"}
    if auth is not None:
        headers["Authorization"] = auth
    try:
        connection.request("POST", "/", json.dumps({"jsonrpc": "1.0" if auth else "2.0",
                                                     "id": 1, "method": method,
                                                     "params": params}), headers)
        response = connection.getresponse()
        if response.status != 200:
            raise RpcError(f"{child.name} RPC {method} failed")
        raw = response.read(16 << 20)
        require(not response.read(1), "RPC response exceeded limit")
        data = decode(raw)
        if data.get("id") != 1 or data.get("error") or "result" not in data:
            raise RpcError(f"{child.name} RPC {method} rejected")
    finally:
        connection.close()
    require(child.owned(), f"owned {child.name} listener changed")
    return data["result"]


def port(stack):
    reservation = stack.enter_context(socket.socket())
    reservation.bind(("127.0.0.1", 0))
    return reservation, reservation.getsockname()[1]


def wait_listener(child, cookie=None):
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        if child.owned() and (cookie is None or cookie.exists()):
            return
        time.sleep(0.1)
    raise TimeoutError(f"owned {child.name} did not acquire readiness listeners")


def restore_pair(root, run, tools, deployment, boundary, stack):
    for name in ("syscoind", "syscoin_geth"):
        with verified(root, tools[name]):
            pass
    core_socket, core_port = port(stack)
    nevm_socket, nevm_port = port(stack)
    zmq_socket, zmq_port = port(stack)
    endpoint = f"tcp://127.0.0.1:{zmq_port}"
    nevm_socket.close()
    zmq_socket.close()
    nevm = Child("nevm", root / tools["syscoin_geth"]["path"], [
        "--datadir", str(run / "nevm"), "--state.scheme", "hash", "--gcmode", "archive",
        "--networkid", "31337", "--syncmode", "full", "--nodiscover", "--maxpeers", "0",
        "--port", "0", "--nat", "none", "--syscoin.statebootstrap.disable", "--nevmpub", endpoint,
        "--http", "--http.addr", "127.0.0.1", "--http.port", str(nevm_port),
        "--http.api", "eth,net,web3", "--http.vhosts", "localhost,127.0.0.1",
        "--ipcdisable", "--miner.etherbase", deployment["nevm_miner"],
        "--miner.gaslimit", "120000000", "--cache", "512",
    ], (nevm_port, zmq_port), run)
    stack.callback(nevm.stop)
    wait_listener(nevm)
    core_socket.close()
    core = Child("core", root / tools["syscoind"]["path"], [
        f"-datadir={run / 'core'}", "-regtest", "-server=1", "-daemon=0", "-listen=0",
        "-connect=0", "-dnsseed=0", "-discover=0", "-listenonion=0", "-upnp=0", "-natpmp=0",
        "-nevmstartheight=1", "-txindex=1", "-par=2", "-rpcbind=127.0.0.1",
        "-rpcallowip=127.0.0.1", f"-rpcport={core_port}", f"-zmqpubnevm={endpoint}",
        "-printtoconsole=1",
    ], (core_port,), run)
    stack.callback(core.stop)
    cookie = run / "core/regtest/.cookie"
    wait_listener(core, cookie)
    with opened(run, "core/regtest/.cookie", private=True) as source:
        cookie_value = source.read(1024).rstrip(b"\r\n")
    require(cookie_value.startswith(b"__cookie__:") and b"\n" not in cookie_value,
            "owned Core cookie invalid")
    auth = "Basic " + base64.b64encode(cookie_value).decode()

    def core_rpc(method, *params):
        return rpc(core, method, params, auth)

    def nevm_rpc(method, *params):
        return rpc(nevm, method, params)

    deadline = time.monotonic() + 45
    while True:
        try:
            verify_pair(core_rpc, nevm_rpc, boundary)
            break
        except (RpcError, ConnectionError, http.client.HTTPException):
            if time.monotonic() >= deadline:
                raise TimeoutError("paired JSON-RPC did not become ready") from None
            time.sleep(0.15)
    wallet = deployment["core_wallet_name"]
    if wallet not in core_rpc("listwallets"):
        core_rpc("loadwallet", wallet)
    require(wallet in core_rpc("listwallets"), "private DA wallet did not load")
    latest = verify_pair(core_rpc, nevm_rpc, boundary)
    return core, nevm, int(latest["timestamp"], 16)


def core_hash(value):
    return hash32("0x" + value.removeprefix("0x"))


def verify_pair(core_rpc, nevm_rpc, boundary):
    info = core_rpc("getblockchaininfo")
    require(info["chain"] == "regtest" and core_rpc("getnetworkinfo")["connections"] == 0
            and int(nevm_rpc("eth_chainId"), 16) == 31337
            and int(nevm_rpc("net_peerCount"), 16) == 0, "different chain or unexpected peers")
    paired = core_rpc("getnevmblockchaininfo")
    latest = nevm_rpc("eth_getBlockByNumber", "latest", False)
    core_height = number(boundary["core_height"], 1)
    nevm_height = number(boundary["nevm_height"], 1)
    require(core_height - nevm_height in {0, 1} and info["blocks"] == core_height
            and core_hash(info["bestblockhash"]) == hash32(boundary["core_hash"])
            and core_hash(core_rpc("getblockhash", 0)) == hash32(boundary["core_genesis_hash"])
            and nevm_rpc("eth_getBlockByNumber", "0x0", False)["hash"] ==
            hash32(boundary["nevm_genesis_hash"]), "restored genesis or Core frontier differs")
    require(paired["status"] == "online" and paired["height"] == core_height
            and paired["bestblockhash"] == hash32(boundary["core_nevm_hash"]),
            "restored Core NEVM pairing differs")
    accepted = {(nevm_height, hash32(boundary["nevm_hash"])),
                (core_height, boundary["core_nevm_hash"])}
    require((int(latest["number"], 16), latest["hash"]) in accepted
            and nevm_rpc("eth_getBlockByNumber", hex(nevm_height), False)["hash"] ==
            boundary["nevm_hash"], "restored NEVM frontier differs")
    # Core-at-H+1 is valid only if its exact parent committed to the saved Geth block.
    anchor = core_rpc("getblockhash", nevm_height)
    anchor_block = core_rpc("getblock", anchor, 1)
    if core_height != nevm_height:
        tip_block = core_rpc("getblock", info["bestblockhash"], 1)
        require(tip_block["previousblockhash"] == anchor, "Core anchor is not the exact parent")
    proof = core_rpc("syscoingetspvproof", anchor_block["tx"][0], anchor)
    require(proof["blockhash"] == anchor and type(proof["index"]) is int and proof["index"] == 0
            and core_hash(proof["nevm_blockhash"]) == boundary["nevm_hash"],
            "Core coinbase does not commit to the saved NEVM anchor")
    return latest


def run(args):
    require(sys.platform == "linux", "canonical restore requires Linux")
    directory(args.fixture_dir)
    directory(args.run_dir, private=True)
    directory(args.private_config_dir, private=True)
    require(not any(args.run_dir.iterdir()) and not os.isatty(sys.stdin.fileno()),
            "restore requires an empty run directory and an owned lifetime pipe")
    require(stat.S_ISFIFO(os.fstat(sys.stdin.fileno()).st_mode), "restore lifetime must be a pipe")
    require(not os.path.lexists(args.fixture_dir / "CANONICAL_V8_REGENERATION_REQUIRED"),
            "canonical fixture regeneration marker is present")
    with opened(args.fixture_dir, "l1-backend.json") as file:
        raw = file.read(1 << 20)
        require(not file.read(1) and hashlib.sha256(raw).hexdigest() == sha(args.descriptor_sha256),
                "canonical descriptor differs from caller trust root")
    descriptor = exact(decode(raw), "schema_version protocol_version backend")
    require(descriptor["schema_version"] == 1 and descriptor["protocol_version"] == "v32.0",
            "wrong canonical descriptor version")
    backend = exact(descriptor["backend"], "kind inventory")
    require(backend["kind"] == "syscoin_core_nevm", "wrong canonical backend")
    inventory = exact(backend["inventory"],
                      "chain_ids finality source_identities proof_identities deployment_record "
                      "clean_boundary snapshot_manifest snapshots tools")
    require(inventory["chain_ids"] == {"root": 31337, "gateway": 57001, "edges": [6565, 6566]}
            and inventory["finality"] == {"kind": "confirmations", "required_confirmations": 5},
            "wrong topology or finality")
    snapshots = by_role(inventory["snapshots"], SNAPSHOTS.keys())
    tools = by_role(inventory["tools"], TOOLS)
    with verified(args.fixture_dir, tools["restore_supervisor"]):
        require(Path(__file__).resolve() == args.fixture_dir / tools["restore_supervisor"]["path"],
                "executing supervisor is outside the authenticated role")
    deployment = verified_json(args.fixture_dir, inventory["deployment_record"])
    boundary = exact(verified_json(args.fixture_dir, inventory["clean_boundary"]),
                     "schema core_height core_hash core_genesis_hash core_nevm_hash "
                     "nevm_height nevm_hash nevm_genesis_hash roles")
    require(boundary["schema"] == "syscoin-v32-canonical-clean-boundary-v1", "wrong boundary schema")
    roles = exact(boundary["roles"], " ".join(CHAIN_ROLES))
    for record in roles.values():
        exact(record, "block_number block_hash batch_number")
        number(record["block_number"])
        hash32(record["block_hash"])
        number(record["batch_number"])
    manifest = exact(verified_json(args.fixture_dir, inventory["snapshot_manifest"]), "schema roles")
    require(manifest["schema"] == "syscoin-v32-canonical-snapshot-layout-v1", "wrong snapshot schema")
    limits = exact(manifest["roles"], " ".join(SNAPSHOTS))
    for role, target in SNAPSHOTS.items():
        extract(args.fixture_dir, snapshots[role], args.run_dir / target, limits[role], target)
    stage_configs(args.private_config_dir, deployment, args.run_dir)

    stopping = False

    def request_stop(*_):
        nonlocal stopping
        stopping = True

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, request_stop)

    with ExitStack() as stack:
        core, nevm, timestamp = restore_pair(args.fixture_dir, args.run_dir, tools, deployment,
                                             boundary, stack)
        record = {
            "schema": "syscoin-v32-canonical-restore-ready-v1",
            "descriptor_sha256": args.descriptor_sha256,
            "nevm_rpc_url": f"http://127.0.0.1:{nevm.ports[0]}",
            "core_rpc_url": f"http://127.0.0.1:{core.ports[0]}",
            "core_cookie_file": "core/regtest/.cookie",
            "core_wallet_name": deployment["core_wallet_name"],
            "nevm_latest_timestamp": timestamp,
            "roles": roles,
        }
        if not stopping:
            temporary = args.run_dir / "ready.tmp"
            with temporary.open("x", encoding="utf-8") as out:
                json.dump(record, out, sort_keys=True)
            temporary.rename(args.run_dir / "ready.json")
        while not stopping:
            require(core.owned() and nevm.owned(), "owned backend exited after readiness")
            if select.select([sys.stdin], [], [], 0.25)[0]:
                require(not os.read(sys.stdin.fileno(), 1), "unexpected lifetime pipe data")
                break


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-dir", type=Path, required=True)
    parser.add_argument("--descriptor-sha256", required=True)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--private-config-dir", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        run(args)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, tarfile.TarError) as error:
        # Logs and runtime configs can contain credentials. Never relay daemon output.
        print(f"canonical restore failed ({type(error).__name__})", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
