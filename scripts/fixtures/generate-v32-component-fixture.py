#!/usr/bin/env python3
"""Generate a NEW V32/V8 localhost component fixture from actual deployments.

Execution requires a separately reviewed, artifact-bound finite plan. The sole
code-install exception is the explicit strict32-byte component DA mock at the
new Anvil's empty 0x63; no other code/storage override, stock fixture, fork,
wallet export, VK generation, registration writer, canonical-release fallback,
or remote RPC is supported. The CLI adapter
is built only in an isolated current zkstack source tree; production CLI and
production node sources are not patched by this program.
"""
import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import pwd
import signal
import socket
import stat
import subprocess
import sys
import time
import tomllib
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("component_gate", ROOT / "scripts/fixtures/verify-v32-component-fixture.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
GOVERNOR = "0x622a54ea3a123127ca5fe8b98de90e957471093a"
GOVERNANCE = "0x7d631ab177342c99d3a925703d985d2de3fbfdf8"
SALT = "0x7a7ae2cf64eaa133584178cf81c0c2f0b2eafd0b5eb5d05c208760eb00459fb0"
FACTORY = "0x4e59b44847b379578588920ca78fbf26c0b4956c"
TIMELOCK = "0xabb69e8e899c06e51414efde62d4423de4f35004"
TIMELOCK_HASH = "0xd98965fa7f49fc4302a2d161454fb0ef619516fbb05a24724e64bb3a3e06e5c4"
RELAY = "0x758b06cda80bdd016f79afd0df1a984039067a21"
RELAY_HASH = "0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0"
RELAY_INIT_HASH = "0x3b2e17477401a6d3df4356c346fdc18278330bbefca56154a81da87cbfd44bf2"
COMPONENT_DA_ADDRESS = "0x0000000000000000000000000000000000000063"
CLI_TREE = "2e4eed988d0014be40a7fcbdc0d9920229bfb56e"
SERVER_HEAD = "64665f1139eaab0ec9874dfe99854188fd0f7eab"
TOOL_NAMES = {"python", "zkstack", "forge", "cast", "anvil", "node", "solc", "era_solc", "zksolc", "bootstrap", "deposit", "zstd"}
SOLC_SHA256 = "9a0fb7e0db2c0641dbae1c5cc645dc686820c83af516226abb1c0a2f76636f25"
ERA_SOLC_SHA256 = "cebf505bd1874f7ce3a6e7de3545f7f7fd52b4ba7bccb54f710c7daf7ae42edd"
ZKSOLC_SHA256 = "aac04e482baf59c33d4cb758bb9f0d2c33dd88ba2ea32dfeae402109dc4ba6d4"
COMMIT_ADDRESS = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
EXECUTE_ADDRESS = "0x2b5ad5c4795c026514f8317c7a215e218dccd6cf"
MANAGEMENT_KEY = "0" * 63 + "3"
MANAGEMENT_ADDRESS = "0x6813eb9362372eef6200f3b1dbc3f819671cba69"
ROOT_PROVE_ADDRESS = "0xd41c057fd1c78805aac12b0a94a405c0461a6fbb"
EDGE2_ADDRESSES = ("0x1eff47bc3a10a45d4b230b5d10e37751fe6aa718",
                   "0xe1ab8145f7e55dc933d51a18c793f901a3a0b276",
                   "0xe57bfe9f44b819898f47bf37e5af72a0783e1141")
PLAN_KEYS = {"schema", "scope", "execute", "deadline_unix", "working_directory", "package_directory",
             "era_root", "source_attestation", "tools", "ports", "genesis", "server_revision"}
PORT_NAMES = {"anvil", "gateway", "edge6565", "edge6566"}
CHAIN_IDS = {"gateway": 57001, "edge6565": 6565, "edge6566": 6566}
# Exact metadata remappings from the accepted 9b4 Cancun contracts and Prague
# relay. Auto-discovered unused library aliases still change CBOR / CREATE2.
ACCEPTED_REMAPPINGS = (
    "@ensdomains/=node_modules/@ensdomains/",
    "@openzeppelin/contracts-upgradeable-v4/=lib/openzeppelin-contracts-upgradeable-v4/contracts/",
    "@openzeppelin/contracts-v4/=lib/openzeppelin-contracts-v4/contracts/",
    "ds-test/=lib/forge-std/lib/ds-test/src/",
    "erc4626-tests/=lib/openzeppelin-contracts-upgradeable-v4/lib/erc4626-tests/",
    "eth-gas-reporter/=node_modules/eth-gas-reporter/",
    "forge-std/=lib/forge-std/src/",
    "foundry-test/=test/foundry/",
    "hardhat/=node_modules/hardhat/",
    "l2-contracts/=../l2-contracts/contracts/",
    "murky/=lib/murky/src/",
    "openzeppelin-contracts-upgradeable-v4/=lib/openzeppelin-contracts-upgradeable-v4/",
    "openzeppelin-contracts-v4/=lib/openzeppelin-contracts-v4/",
    "openzeppelin-contracts/=lib/murky/lib/openzeppelin-contracts/",
    "system-contracts/=../system-contracts/",
    "test-utils/=test/foundry/l1/integration/utils/",
)


def need(ok, message):
    if not ok:
        raise ValueError(message)


def absolute_path(value):
    need(type(value) is str and value.startswith("/"), "absolute path required")
    path = pathlib.Path(value)
    need(str(path) == value and not any(part in {".", ".."} for part in path.parts), "canonical path required")
    return path


def held(ref, maximum=512 * 1024 * 1024, *, tool=False):
    need(type(ref) is dict and set(ref) == {"path", "size", "sha256"}, "held reference shape")
    need(type(ref["size"]) is int and 0 < ref["size"] <= maximum and gate.digest(ref["sha256"]), "held reference identity")
    path = absolute_path(ref["path"])
    need(path.resolve(strict=True) == path, "held reference redirected")
    if tool:
        for parent in (*reversed(path.parent.parents), path.parent):
            info = parent.lstat()
            need(stat.S_ISDIR(info.st_mode) and info.st_uid in {0, os.getuid()}
                 and not info.st_mode & 0o022, "trusted tool parent required")
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb") as stream:
        before = os.fstat(stream.fileno())
        owner_ok = before.st_uid in {0, os.getuid()} if tool else before.st_uid == os.getuid()
        mode_ok = bool(before.st_mode & 0o111) and not before.st_mode & 0o022 if tool else True
        need(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and owner_ok and mode_ok
             and before.st_size == ref["size"], "nonowned regular singlelink input")
        data = stream.read(maximum + 1)
        after = os.fstat(stream.fileno())
    named = path.stat()
    fields = lambda s: (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_nlink,
                        s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    need(fields(before) == fields(after) == fields(named) and hashlib.sha256(data).hexdigest() == ref["sha256"], "input changed")
    return data


def checked_plan(plan, now):
    need(type(plan) is dict and set(plan) == PLAN_KEYS, "generation plan shape")
    need(plan["schema"] == "syscoin-v32-anvil-component-generation-v1"
         and plan["scope"] == "AnvilComponentOnly" and type(plan["execute"]) is bool, "generation scope")
    need(plan["server_revision"] == SERVER_HEAD, "current server source revision")
    need(type(plan["deadline_unix"]) is int and now < plan["deadline_unix"] <= now + 3600, "finite generation deadline")
    work = absolute_path(plan["working_directory"])
    package = absolute_path(plan["package_directory"])
    era = absolute_path(plan["era_root"])
    need(work != package and work not in package.parents and package not in work.parents, "disjoint new outputs")
    for output in (work, package):
        need(not output.exists() and not output.is_symlink() and output.parent.resolve(strict=True) == output.parent,
             "new exclusive output required")
        parent = output.parent.stat()
        need(parent.st_uid == os.getuid() and stat.S_ISDIR(parent.st_mode)
             and stat.S_IMODE(parent.st_mode) == 0o700, "private owned output parent")
    need(era.is_dir() and era.resolve(strict=True) == era, "isolated current Era tree")
    need(type(plan["ports"]) is dict and set(plan["ports"]) == PORT_NAMES, "four physical localhost ports required")
    ports = list(plan["ports"].values())
    need(all(type(p) is int and 1024 < p < 65536 for p in ports) and len(set(ports)) == len(ports), "port identities")
    need(type(plan["tools"]) is dict and set(plan["tools"]) == TOOL_NAMES, "exact current artifact toolset")
    need(plan["tools"]["solc"]["sha256"] == SOLC_SHA256, "pinned Solc 0.8.28 required")
    need(plan["tools"]["era_solc"]["sha256"] == ERA_SOLC_SHA256
         and plan["tools"]["zksolc"]["sha256"] == ZKSOLC_SHA256,
         "pinned EraVM Solc 0.8.28-1.0.1 and ZkSolc 1.5.11 required")
    for ref in plan["tools"].values():
        held(ref, 512 * 1024 * 1024, tool=True)
    need(hashlib.sha256(held(plan["genesis"], 1024 * 1024)).hexdigest() == gate.GENESIS, "accepted current genesis")
    attestation = json.loads(held(plan["source_attestation"], 8 * 1024 * 1024))
    need(type(attestation) is dict and set(attestation) == {"schema", "scope", "zkstack_base_tree", "server_revision",
          "adapter_sha256", "tools", "source_files", "contract_artifacts", "era_root"}, "source attestation shape")
    need(attestation["schema"] == "syscoin-v32-component-tool-build-v1"
         and attestation["scope"] == "AnvilComponentOnly" and attestation["zkstack_base_tree"] == CLI_TREE
         and attestation["server_revision"] == SERVER_HEAD and attestation["era_root"] == str(era)
         and attestation["tools"] == plan["tools"], "current source/tool attestation joins")
    adapter = ROOT / "scripts/fixtures/zkstack-anvil-component.patch"
    need(attestation["adapter_sha256"] == hashlib.sha256(adapter.read_bytes()).hexdigest(), "exact component-only adapter")
    need(type(attestation["source_files"]) is list and len(attestation["source_files"]) >= 10, "source closure required")
    paths = [ref["path"] for ref in attestation["source_files"]]
    need(len(set(paths)) == len(paths), "source closure duplicates")
    for ref in attestation["source_files"]:
        held(ref)
    need(type(attestation["contract_artifacts"]) is list and len(attestation["contract_artifacts"]) >= 8,
         "actual current compiled contract artifacts required")
    artifact_paths = [ref["path"] for ref in attestation["contract_artifacts"]]
    need(len(set(artifact_paths)) == len(artifact_paths)
         and all(str(era) + "/" in p for p in artifact_paths), "isolated contract artifact closure")
    for ref in attestation["contract_artifacts"]:
        held(ref)
    required_suffixes = {"crates/common/src/forge.rs", "crates/zkstack/src/utils/forge.rs",
        "deploy-scripts/ecosystem/DeployL1CoreContracts.s.sol", "deploy-scripts/ctm/DeployCTM.s.sol",
        "deploy-scripts/gateway/GatewayCTMDeployerHelper.sol", "contracts/common/SyscoinConfig.sol",
        "src/bin/anvil-component-bootstrap.rs", "src/test_config.rs",
        "scripts/fixtures/ComponentBitcoinDAAvailabilityMock.sol"}
    need(all(sum(p.endswith("/" + suffix) for p in paths) == 1 for suffix in required_suffixes), "required genuine source roles")
    return attestation


def write_json(path, value):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())


def read_yaml(path):
    # Explicit generation and local-loader regression dependency.
    import yaml

    class ComponentLoader(yaml.SafeLoader):
        pass

    def integer(loader, node):
        # The CLI emits unquoted hex addresses/hashes. Preserve their exact
        # spelling (including leading zeros), not Python's integer coercion.
        if node.value.startswith(("0x", "0X")):
            return node.value
        return yaml.SafeLoader.construct_yaml_int(loader, node)

    ComponentLoader.add_constructor("tag:yaml.org,2002:int", integer)
    return yaml.load(path.read_bytes(), Loader=ComponentLoader)


def operator_roles(name):
    need(name in CHAIN_IDS, "physical component operator identity")
    if name == "edge6566":
        return tuple(zip(EDGE2_ADDRESSES, gate.PUBLIC_EDGE2_KEYS))
    return ((COMMIT_ADDRESS, gate.PUBLIC_COMMIT_KEY),
            (ROOT_PROVE_ADDRESS, gate.PUBLIC_ROOT_PROVE_KEY) if name == "gateway"
            else (gate.REVERTER, gate.PUBLIC_KEY),
            (EXECUTE_ADDRESS, gate.PUBLIC_EXECUTE_KEY))


def public_wallets(ecosystem, name="gateway"):
    public = {"address": gate.REVERTER, "private_key": "0x" + gate.PUBLIC_KEY}
    result = {name: dict(public) for name in ("deployer", "operator", "blob_operator", "prove_operator",
                                             "execute_operator", "governor", "token_multiplier_setter", "test_wallet")}
    result["fee_account"] = {"address": "0x5a67ee02274d9ec050d412b96fe810be4d71e7a0", "private_key": None}
    if ecosystem:
        result["governor"] = {"address": GOVERNOR, "private_key": None}
    management = {"address": MANAGEMENT_ADDRESS, "private_key": "0x" + MANAGEMENT_KEY}
    result["deployer"] = dict(management)
    if not ecosystem:
        result["governor"] = dict(management)
    commit, prove, execute = operator_roles(name)
    result["operator"] = {"address": commit[0], "private_key": "0x" + commit[1]}
    result["blob_operator"] = dict(result["operator"])
    result["prove_operator"] = {"address": prove[0], "private_key": "0x" + prove[1]}
    result["execute_operator"] = {"address": execute[0], "private_key": "0x" + execute[1]}
    return result


def chain_args(name, chain_id, wallet):
    return ["--chain-name", name, "--chain-id", str(chain_id), "--prover-mode", "no-proofs",
            "--wallet-creation", "in-file", "--wallet-path", str(wallet),
            "--l1-batch-commit-data-generator-mode", "rollup", "--base-token-address", "0x0000000000000000000000000000000000000001",
            "--base-token-price-nominator", "1", "--base-token-price-denominator", "1",
            "--set-as-default", "false", "--evm-emulator", "false", "--zksync-os"]


def server_config(name, contracts, root_rpc, gateway_rpc, port, genesis, work):
    # Construct from public addresses, never copy a generated wallet/config.
    core = contracts["ecosystem_contracts"]
    commit, prove, execute = operator_roles(name)
    cfg = {"general": {"rocks_db_path": str(work / (name + "-db")), "startup_sl_finalization_timeout": "300s"},
        "l1_provider": {"rpc_url": root_rpc, "rpc_poll_interval": "100ms"},
        "l1_watcher": {"poll_interval": "100ms", "finalized_poll_interval": "100ms", "confirmations": 0},
        "genesis": {"bridgehub_address": core["bridgehub_proxy_addr"],
                    "bytecode_supplier_address": core["l1_bytecodes_supplier_addr"],
                    "genesis_input_path": str(genesis), "chain_id": CHAIN_IDS[name]},
        "sequencer": {"block_pubdata_limit_bytes": 2097152, "fee_collector_address": "0x5a67ee02274d9ec050d412b96fe810be4d71e7a0"},
        "l1_sender": {"operator_commit_sk": commit[1], "operator_prove_sk": prove[1],
                      "operator_execute_sk": execute[1], "command_limit": 1, "poll_interval": "100ms"},
        "rpc": {"address": "127.0.0.1:" + str(port)}, "status_server": {"enabled": False},
        "network": {"enabled": False},
        "replay_archive": {"type": "FileSystem", "root_path": str(work / (name + "-replay-archive")),
                           "encryption": {"type": "Noop"}},
        "batcher": {"batch_timeout": "1s"},
        "prover_api": {"enabled": False, "proof_storage": {"path": str(work / (name + "-proofs"))},
            "fake_fri_provers": {"enabled": True, "min_age": "0s", "compute_time": "200ms"},
            "fake_snark_provers": {"enabled": True, "max_batch_age": "0s"}},
        "external_price_api_client": {"source": "Forced", "forced_prices": {"0x0000000000000000000000000000000000000001": 3000}}}
    if gateway_rpc:
        cfg["gateway_provider"] = {"rpc_url": gateway_rpc, "rpc_poll_interval": "100ms"}
        cfg["gateway_sender"] = {"operator_commit_sk": commit[1], "operator_prove_sk": prove[1],
                                 "operator_execute_sk": execute[1], "command_limit": 1, "poll_interval": "100ms"}
        cfg["l1_watcher"]["optimistic_gateway_head"] = True
    else:
        cfg["l1_sender"]["pubdata_mode"] = "Blobs"
    gate.public_config(cfg)
    return cfg


class Generation:
    def __init__(self, plan):
        self.plan = plan
        self.work = pathlib.Path(plan["working_directory"])
        self.tools = {name: ref["path"] for name, ref in plan["tools"].items()}
        self.children = []
        self.command_children = []
        component_user_home = pwd.getpwuid(os.getuid()).pw_dir
        need(os.environ.get("HOME") == component_user_home, "actual user home changed")
        global_foundry_config = pathlib.Path(component_user_home) / ".foundry/foundry.toml"
        need(not global_foundry_config.exists() and not global_foundry_config.is_symlink(),
             "global Foundry config must be absent; metadata-only check")
        self.env = {"PATH": ":".join(dict.fromkeys([str(pathlib.Path(self.tools[name]).parent) for name in ("forge", "node")])) + ":/usr/bin:/bin",
                    "HOME": component_user_home,
                    "FOUNDRY_SOLC": self.tools["solc"],
                    # This Forge revision ignores FOUNDRY_ZKSYNC_SOLC_PATH:
                    # zksync is not in its standalone-prefix section list.
                    # Its existing inline-dictionary provider selects the two
                    # EraVM compilers without changing the native L1 compiler.
                    "FOUNDRY_ZKSYNC": "{ solc_path = " + json.dumps(self.tools["era_solc"])
                        + ", zksolc = " + json.dumps(self.tools["zksolc"]) + " }",
                    "FOUNDRY_AUTO_DETECT_REMAPPINGS": "false",
                    "FOUNDRY_REMAPPINGS": "\n".join(ACCEPTED_REMAPPINGS),
                    "RAYON_NUM_THREADS": "2",
                    "SYSCOIN_ANVIL_COMPONENT_ONLY": "31337",
                    "CREATE2_FACTORY_SALT": SALT, "FOUNDRY_PROFILE": "default", "FOUNDRY_EVM_VERSION": "cancun"}
        self.url = "http://127.0.0.1:" + str(plan["ports"]["anvil"])
        self.gateway_url = "http://127.0.0.1:" + str(plan["ports"]["gateway"])
        self.ecosystem = self.work / "component"
        self.commands = []

    def remaining(self):
        remaining = self.plan["deadline_unix"] - time.time()
        need(remaining > 0, "generation deadline reached")
        need(all(child.poll() is None for child in self.children), "owned component stopped unexpectedly")
        return remaining

    def run(self, argv, cwd=None):
        self.commands.append(argv)
        index = len(self.commands)
        stdout = self.work / ("command-" + str(index) + ".private.stdout")
        stderr = self.work / ("command-" + str(index) + ".private.stderr")
        with os.fdopen(os.open(stdout, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as out:
            with os.fdopen(os.open(stderr, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as err:
                timeout = min(self.remaining() - 60, 1800)
                need(timeout > 0, "command must retain cooperative drain time")
                child = subprocess.Popen(argv, cwd=cwd or self.ecosystem, env=self.env,
                                         stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                         start_new_session=True)
                self.command_children.append(child)
                failure = None
                deadline = time.monotonic() + timeout
                while child.poll() is None:
                    stopped = [node.pid for node in self.children if node.poll() is not None]
                    if stopped:
                        failure = "owned component dependency stopped: pid=" + str(stopped[0])
                        break
                    if time.monotonic() >= deadline:
                        failure = "owned command exceeded finite execution deadline"
                        break
                    try:
                        child.wait(timeout=min(0.25, max(0.001, deadline - time.monotonic())))
                    except subprocess.TimeoutExpired:
                        pass
                if failure:
                    # This command owns its new process group, including any
                    # Forge/Node children. Never signal a preexisting group.
                    need(os.getpgid(child.pid) == child.pid, "owned command process group changed")
                    os.killpg(child.pid, signal.SIGTERM)
                    try:
                        child.wait(timeout=60)
                    except subprocess.TimeoutExpired:
                        write_json(self.work / ("COMMAND-" + str(index) + "-NEEDS-ATTENTION.json"),
                                   {"scope": "AnvilComponentOnly", "pid": child.pid,
                                    "tool": pathlib.Path(argv[0]).name, "force_kill": False})
                        raise ValueError("owned command remains alive after cooperative drain") from None
                    raise ValueError(failure) from None
        need(child.returncode == 0, "current tool command failed: command_index=" + str(index)
             + " tool=" + pathlib.Path(argv[0]).name + " exit_code=" + str(child.returncode))
        need(stdout.stat().st_size <= 8 * 1024 * 1024, "bounded public command result")
        return stdout.read_text()

    def start(self, argv, name):
        log = open(self.work / (name + ".private.log"), "xb")
        os.chmod(log.name, 0o600)
        child = subprocess.Popen(argv, cwd=self.work, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        log.close()
        self.children.append(child)
        return child

    def rpc(self, method, params, url=None):
        self.remaining()
        request = urllib.request.Request(url or self.url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
                                         headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=min(self.remaining(), 10)) as response:
            result = json.loads(response.read(512 * 1024 * 1024))
        need(type(result) is dict and result.get("error") is None and result.get("id") == 1, "component RPC failed")
        return result["result"]

    def wait_rpc(self, url, chain_id):
        while True:
            try:
                need(int(self.rpc("eth_chainId", [], url), 16) == chain_id, "component chain mismatch")
                return
            except (OSError, ValueError):
                self.remaining()
                time.sleep(0.2)

    def cli(self, *args):
        # checked_plan admits the actual component toolset. The legacy CLI
        # prerequisite bundle additionally requires unused Docker/Era tooling.
        return self.run([self.tools["zkstack"], "--ignore-prerequisites", *args])

    def install_component_da_mock(self):
        # Sole explicit code-install exception: the NEW loopback Anvil's empty
        # native-precompile address. No production DB, other address or storage
        # is modified. This fixture assumes availability, never DA finality.
        need(self.plan["scope"] == "AnvilComponentOnly"
             and self.url == "http://127.0.0.1:" + str(self.plan["ports"]["anvil"])
             and int(self.rpc("eth_chainId", []), 16) == 31337
             and int(self.rpc("eth_blockNumber", []), 16) < 8
             and self.rpc("eth_getCode", [COMPONENT_DA_ADDRESS, "latest"]) == "0x",
             "fresh empty localhost component precompile required")
        source = ROOT / "scripts/fixtures/ComponentBitcoinDAAvailabilityMock.sol"
        attestation = json.loads(held(self.plan["source_attestation"], 8 * 1024 * 1024))
        refs = [ref for ref in attestation["source_files"] if ref["path"] == str(source)]
        need(len(refs) == 1, "exact component mock source attestation required")
        source_bytes = held(refs[0], 64 * 1024)
        compiled = json.loads(self.run([self.tools["solc"], "--evm-version", "cancun", "--optimize",
            "--optimize-runs", "9999999", "--combined-json", "bin,bin-runtime", str(source)], self.work))
        contracts = {key.rsplit(":", 1)[1]: value for key, value in compiled["contracts"].items()}
        need(len(compiled["contracts"]) == 2
             and set(contracts) == {"ComponentBitcoinDAAvailabilityMock", "ComponentBitcoinDAGasProbe"},
             "exact source-built component mock contracts required")
        mock, probe = (contracts[name] for name in (
            "ComponentBitcoinDAAvailabilityMock", "ComponentBitcoinDAGasProbe"))
        runtime = "0x" + mock["bin-runtime"]
        need(len(runtime) > 2 and len(runtime) < 8192, "bounded source-built component mock runtime")
        self.rpc("anvil_setCode", [COMPONENT_DA_ADDRESS, runtime])
        need(self.rpc("eth_getCode", [COMPONENT_DA_ADDRESS, "latest"]) == runtime,
             "installed component mock runtime changed")
        receipt = json.loads(self.run([self.tools["cast"], "send", "--unlocked", "--from", gate.REVERTER,
            "--rpc-url", self.url, "--json", "--create", "0x" + probe["bin"]], self.work))
        need(receipt.get("status") in ("0x1", "1", 1) and type(receipt.get("contractAddress")) is str,
             "actual component gas probe deployment failed")
        canonical = self.rpc("eth_getTransactionReceipt", [receipt["transactionHash"]])
        need(type(canonical) is dict and canonical.get("transactionHash") == receipt["transactionHash"]
             and canonical.get("status") == "0x1" and canonical.get("blockHash") == receipt.get("blockHash")
             and canonical.get("blockNumber") == receipt.get("blockNumber"), "actual gas probe receipt join")
        block = self.rpc("eth_getBlockByNumber", [canonical["blockNumber"], False])
        need(type(block) is dict and block.get("hash") == canonical["blockHash"], "canonical gas probe block")
        self.component_da_probe = receipt["contractAddress"]
        self.component_da_probe_runtime = "0x" + probe["bin-runtime"]
        need(self.rpc("eth_getCode", [self.component_da_probe, "latest"]) == self.component_da_probe_runtime,
             "actual source-built gas probe code changed")
        controls = self.component_da_controls()
        self.component_da_runtime = runtime
        self.component_da_evidence = {"scope": "AnvilComponentOnly", "address": COMPONENT_DA_ADDRESS,
            "source_sha256": hashlib.sha256(source_bytes).hexdigest(),
            "runtime_sha256": hashlib.sha256(bytes.fromhex(runtime[2:])).hexdigest(),
            "runtime_size": len(bytes.fromhex(runtime[2:])), "caller_gas": 1400,
            "availability_assumed_for_all_32byte_ids": True, "real_da": False,
            "real_finality_qualification": False, "actual_gas_controls": controls,
            "gas_probe_deployment": {"address": self.component_da_probe,
                "transaction_hash": canonical["transactionHash"], "block_hash": canonical["blockHash"],
                "block_number": canonical["blockNumber"], "status": 1}}

    def component_da_controls(self):
        controls = []
        for data, expected in (("11" * 32, True), ("a5" * 32, True), ("", False),
                               ("11" * 31, False), ("11" * 33, False)):
            result = self.run([self.tools["cast"], "call", "--rpc-url", self.url, self.component_da_probe,
                "check(bytes,bool)(bool)", "0x" + data, str(expected).lower()], self.work).strip()
            need(result == "true", "actual 1400-gas component DA control failed")
            controls.append({"input_size": len(data) // 2, "expected_success": expected, "actual_control_passed": True})
        return controls

    def build_component_relay(self, relay_out):
        # This pinned Forge gives the env value precedence over the CLI flag.
        # Scope Prague to this separate relay build; all native L1 stays Cancun.
        previous_evm = self.env["FOUNDRY_EVM_VERSION"]
        self.env["FOUNDRY_EVM_VERSION"] = "prague"
        try:
            self.run([self.tools["forge"], "build", "contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol",
                      "--skip", "test", "--force", "--evm-version", "prague", "--out", str(relay_out),
                      "--cache-path", str(self.work / "relay-cache")], pathlib.Path(self.plan["era_root"]) / "contracts/l1-contracts")
        finally:
            self.env["FOUNDRY_EVM_VERSION"] = previous_evm
        artifact = relay_out / "SyscoinRelayedSLDAValidator.sol/SyscoinRelayedSLDAValidator.json"
        compiled = json.loads(artifact.read_bytes())
        metadata = compiled["metadata"]
        if type(metadata) is str:
            metadata = json.loads(metadata)
        need(metadata["compiler"]["version"] == "0.8.28+commit.7893614a"
             and metadata["settings"]["evmVersion"] == "prague"
             and metadata["settings"]["remappings"] == list(ACCEPTED_REMAPPINGS),
             "actual relay compiler/Prague/remapping identity changed")
        for key, expected in (("bytecode", RELAY_INIT_HASH), ("deployedBytecode", RELAY_HASH)):
            need(self.run([self.tools["cast"], "keccak", compiled[key]["object"]], self.work).strip().lower() == expected,
                 "actual relay source-built bytecode identity changed")
        return artifact

    def contracts(self, name):
        path = self.ecosystem / "chains" / name / "configs/contracts.yaml"
        return read_yaml(path)

    def ensure_unpaused(self, name):
        # Successful migration finalization normally unpauses itself. Never
        # send a redundant unpause that correctly reverts DepositsNotPaused.
        diamond = self.contracts(name)["l1"]["diamond_proxy_addr"]
        paused = self.call(diamond, "depositsPaused()(bool)", self.url).lower()
        need(paused in {"true", "false"}, "actual Root deposits-paused state")
        if paused == "true":
            self.cli("chain", "unpause-deposits", "--chain", name, "--l1-rpc-url", self.url)
        need(self.call(diamond, "depositsPaused()(bool)", self.url).lower() == "false",
             "actual Root deposits must be unpaused")
        migrated = read_yaml(self.ecosystem / "chains" / name / "configs/gateway_chain.yaml")
        need(self.call(migrated["diamond_proxy_addr"], "depositsPaused()(bool)", self.gateway_url).lower() == "false",
             "actual Gateway deposits must be unpaused")
        return {"chain_id": CHAIN_IDS[name], "root_diamond": diamond,
                "gateway_diamond": migrated["diamond_proxy_addr"],
                "actual_root_deposits_paused": False, "actual_gateway_deposits_paused": False}

    def call(self, address, signature, url=None):
        return self.run([self.tools["cast"], "call", address, signature, "--rpc-url", url or self.url]).strip()

    def assert_code(self, url, address, expected_size, expected_hash):
        code = self.rpc("eth_getCode", [address, "latest"], url)
        need(type(code) is str and code.startswith("0x") and len(bytes.fromhex(code[2:])) == expected_size, "current deployed code size")
        need(self.run([self.tools["cast"], "keccak", code]).strip().lower() == expected_hash, "current deployed code identity")

    def grant_root_reverter(self, name):
        # RegisterZKChain assigns REVERTER to wallet.operator (scalar-1),
        # separately from the F39 reverter used by the original tests.
        # Keep that mapping: Gateway migration also enables/funds operator.
        # Grant only REVERTER through the ordinary owner-only chain admin API.
        need(name == "gateway", "only the physical Root-settled Gateway role grant")
        contracts = self.contracts(name)["l1"]
        diamond, timelock, admin = (contracts[key] for key in (
            "diamond_proxy_addr", "validator_timelock_addr", "chain_admin_addr"))
        need(self.call(diamond, "getAdmin()(address)", self.url).lower() == admin.lower(),
             "actual Root diamond admin")
        need(self.call(admin, "owner()(address)", self.url).lower() == MANAGEMENT_ADDRESS,
             "actual source-public chain governor")
        role = self.call(timelock, "REVERTER_ROLE()(bytes32)", self.url)
        role_admin = self.run([self.tools["cast"], "call", timelock,
            "getRoleAdmin(address,bytes32)(bytes32)", diamond, role, "--rpc-url", self.url]).strip().lower()
        need(role_admin == "0x" + "0" * 64, "ordinary chain-admin role authority")
        data = self.run([self.tools["cast"], "calldata", "grantRole(address,bytes32,address)",
                        diamond, role, gate.REVERTER]).strip()
        receipt = json.loads(self.run([self.tools["cast"], "send", admin,
            "multicall((address,uint256,bytes)[],bool)", "[(" + timelock + ",0," + data + ")]", "true",
            "--from", MANAGEMENT_ADDRESS, "--unlocked", "--rpc-url", self.url, "--json"]))
        need(type(receipt) is dict and receipt.get("status") in {"0x1", "1", 1}
             and gate.digest(receipt.get("transactionHash", "")[2:]), "actual role-grant receipt")
        canonical = self.rpc("eth_getTransactionReceipt", [receipt["transactionHash"]])
        need(type(canonical) is dict and canonical.get("transactionHash") == receipt["transactionHash"]
             and canonical.get("status") == "0x1" and canonical.get("blockHash") == receipt.get("blockHash")
             and canonical.get("blockNumber") == receipt.get("blockNumber"), "actual role-grant receipt join")
        block = self.rpc("eth_getBlockByNumber", [canonical["blockNumber"], False])
        need(type(block) is dict and block.get("hash") == canonical["blockHash"], "canonical role-grant block")
        return {"chain_id": CHAIN_IDS[name], "chain_admin": admin, "timelock": timelock,
                "role_holder": gate.REVERTER, "transaction_hash": canonical["transactionHash"],
                "block_hash": canonical["blockHash"], "block_number": canonical["blockNumber"], "status": 1}

    def grant_gateway_reverter(self, name):
        # The migrated diamond admin is the Root ChainAdmin's L1-to-L2
        # alias, not an impersonated/deployed Gateway owner. Use the retained
        # priority admin route; never call owner() or setCode on that alias.
        need(name in {"edge6565", "edge6566"}, "only actual migrated component edges")
        contracts = self.contracts(name)
        root = contracts["l1"]
        root_admin = root["chain_admin_addr"]
        need(self.call(root["diamond_proxy_addr"], "getAdmin()(address)", self.url).lower() == root_admin.lower()
             and self.call(root_admin, "owner()(address)", self.url).lower() == MANAGEMENT_ADDRESS,
             "actual source-public Root edge admin")
        migrated = read_yaml(self.ecosystem / "chains" / name / "configs/gateway_chain.yaml")
        diamond, timelock = migrated["diamond_proxy_addr"], migrated["validator_timelock_addr"]
        alias = "0x" + format((int(root_admin, 16) + int("1111000000000000000000000000000000001111", 16)) % 2**160, "040x")
        need(migrated["gateway_chain_id"] == 57001
             and timelock.lower() == TIMELOCK
             and self.call(diamond, "getAdmin()(address)", self.gateway_url).lower() == alias,
             "actual migrated diamond Root-admin alias")
        role = self.call(timelock, "REVERTER_ROLE()(bytes32)", self.gateway_url)
        data = self.run([self.tools["cast"], "calldata", "grantRole(address,bytes32,address)",
                         diamond, role, gate.REVERTER]).strip()
        bridgehub = contracts["ecosystem_contracts"]["bridgehub_proxy_addr"]
        gas_price = int(self.rpc("eth_gasPrice", []), 16)
        contract_root = pathlib.Path(self.plan["era_root"]) / "contracts/l1-contracts"
        self.run([self.tools["forge"], "script", "deploy-scripts/AdminFunctions.s.sol:AdminFunctions",
            "--sig", "adminL1L2TxViaGateway(address,uint256,uint256,uint256,address,uint256,bytes,address,bool)",
            bridgehub, str(gas_price), str(CHAIN_IDS[name]), "57001", timelock, "0", data,
            MANAGEMENT_ADDRESS, "false", "--rpc-url", self.url, "--ffi"], contract_root)
        prepared = tomllib.loads((contract_root / "script-out/output-admin-functions.toml").read_text())
        need(prepared["admin_address"].lower() == root_admin.lower(), "ordinary prepared Root admin")
        decoded = json.loads(self.run([self.tools["cast"], "abi-decode", "f()((address,uint256,bytes)[])",
                                       prepared["encoded_data"], "--json"]))
        need(type(decoded) is list and len(decoded) == 1 and type(decoded[0]) is list
             and len(decoded[0]) == 1, "one genuine ETH-base-token priority admin call")
        target, value, calldata = decoded[0][0]
        need(target.lower() == bridgehub.lower() and type(value) is int and 0 < value < 10**22
             and type(calldata) is str and calldata.startswith("0x") and len(calldata) < 262144,
             "prepared ordinary priority-call target/value/data")
        receipt = json.loads(self.run([self.tools["cast"], "send", root_admin,
            "multicall((address,uint256,bytes)[],bool)", "[(" + target + "," + str(value) + "," + calldata + ")]", "true",
            "--value", str(value), "--from", MANAGEMENT_ADDRESS, "--unlocked", "--rpc-url", self.url, "--json"]))
        canonical = self.rpc("eth_getTransactionReceipt", [receipt["transactionHash"]])
        need(type(canonical) is dict and canonical.get("status") == "0x1"
             and canonical.get("transactionHash") == receipt["transactionHash"]
             and canonical.get("blockHash") == receipt.get("blockHash")
             and canonical.get("blockNumber") == receipt.get("blockNumber"), "canonical Root priority role-grant receipt")
        block = self.rpc("eth_getBlockByNumber", [canonical["blockNumber"], False])
        need(block.get("hash") == canonical["blockHash"], "canonical Root priority role-grant block")
        topic = self.run([self.tools["cast"], "keccak", "NewPriorityRequestId(uint256,bytes32)"]).strip().lower()
        gateway_diamond = self.contracts("gateway")["l1"]["diamond_proxy_addr"]
        logs = [log for log in canonical["logs"] if log["address"].lower() == gateway_diamond.lower()
                and len(log.get("topics", [])) == 3 and log["topics"][0].lower() == topic]
        need(len(logs) == 1 and gate.digest(logs[0]["topics"][2][2:]), "one actual Gateway priority transaction")
        tx_hash = logs[0]["topics"][2]
        while True:
            applied = self.rpc("eth_getTransactionReceipt", [tx_hash], self.gateway_url)
            if applied is not None:
                break
            self.remaining()
            time.sleep(0.2)
        need(applied.get("status") == "0x1" and applied.get("transactionHash") == tx_hash,
             "actual Gateway priority role grant succeeded")
        applied_block = self.rpc("eth_getBlockByNumber", [applied["blockNumber"], False], self.gateway_url)
        need(applied_block.get("hash") == applied["blockHash"], "canonical Gateway priority role-grant block")
        return {"chain_id": CHAIN_IDS[name], "role_holder": gate.REVERTER, "root_chain_admin": root_admin,
                "gateway_admin_alias": alias, "management_address": MANAGEMENT_ADDRESS,
                "root_transaction_hash": canonical["transactionHash"], "root_block_hash": canonical["blockHash"],
                "gateway_transaction_hash": tx_hash, "gateway_block_hash": applied["blockHash"], "status": 1}

    def chain_readback(self, name):
        contracts = self.contracts(name)
        settlement = self.gateway_url if name.startswith("edge") else self.url
        if name.startswith("edge"):
            # Migration preserves the Root diamond in contracts.yaml and writes
            # the separately deployed Gateway diamond into gateway_chain.yaml.
            migrated = read_yaml(self.ecosystem / "chains" / name / "configs/gateway_chain.yaml")
            need(type(migrated) is dict and type(migrated["gateway_chain_id"]) is int
                 and migrated["gateway_chain_id"] == 57001
                 and migrated["validator_timelock_addr"].lower() == TIMELOCK,
                 "actual migrated Gateway chain identity")
            diamond = migrated["diamond_proxy_addr"]
            timelock = migrated["validator_timelock_addr"]
        else:
            diamond = contracts["l1"]["diamond_proxy_addr"]
            timelock = contracts["l1"]["validator_timelock_addr"]
        verifier = self.call(diamond, "getVerifier()(address)", settlement)
        need(self.call(verifier, "verificationKeyHash()(bytes32)", settlement).lower() == gate.VK,
             "actual chain accepted verification key")
        need(self.call(verifier, "IS_TESTNET_VERIFIER()(bool)", settlement).lower() == "true",
             "actual chain component verifier")
        need(int(self.call(diamond, "getProtocolVersion()(uint256)", settlement).split()[0]) == 32 << 32,
             "actual deployed V32 protocol")
        role = self.call(timelock, "REVERTER_ROLE()(bytes32)", settlement)
        need(self.run([self.tools["cast"], "call", timelock, "hasRoleForChainId(uint256,bytes32,address)(bool)",
                       str(CHAIN_IDS[name]), role, gate.REVERTER, "--rpc-url", settlement]).strip().lower() == "true",
             "actual public prove operator reverter role")
        operators = []
        for label, (address, _) in zip(("COMMITTER", "PROVER", "EXECUTOR"), operator_roles(name)):
            role_id = self.call(timelock, label + "_ROLE()(bytes32)", settlement)
            need(self.run([self.tools["cast"], "call", timelock, "hasRoleForChainId(uint256,bytes32,address)(bool)",
                           str(CHAIN_IDS[name]), role_id, address, "--rpc-url", settlement]).strip().lower() == "true",
                 "actual component operator role")
            balance = int(self.rpc("eth_getBalance", [address, "latest"], settlement), 16)
            need(balance >= 10**18, "actual funded settlement operator")
            operators.append({"role": label.lower(), "address": address, "actual_role": True,
                              "actual_balance_wei": str(balance)})
        return {"chain_id": CHAIN_IDS[name], "diamond": diamond, "verifier": verifier,
                "verification_key_hash": gate.VK, "protocol_semantic_version": "0.32.0",
                "component_test_verifier": True, "timelock": timelock,
                "actual_reverter_role_holder": gate.REVERTER, "actual_operators": operators}

    def stop_nodes(self, preserve_anvil=False):
        selected = self.children[1:] if preserve_anvil else self.children
        for child in reversed(selected):
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
        failures = []
        deadline = time.monotonic() + 60
        for child in reversed(selected):
            try:
                child.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                # Retain the failure and the original process; no forced kill.
                failures.append("pid=" + str(child.pid) + " remains alive; force_kill=false")
                continue
            if child.returncode != 0:
                failures.append("pid=" + str(child.pid) + " exit_code=" + str(child.returncode))
        need(not failures, "owned component shutdown failed: " + "; ".join(failures))
        if preserve_anvil:
            self.children = self.children[:1]

    def prepare_script_output(self):
        # Every retained deployment/admin/Gateway ScriptParams output uses
        # this already-permitted directory. Vendored public trees omit it.
        parent = pathlib.Path(self.plan["era_root"]) / "contracts/l1-contracts"
        info = parent.stat()
        need(parent.resolve(strict=True) == parent and stat.S_ISDIR(info.st_mode)
             and info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700,
             "fresh private generation contract root required")
        output = parent / "script-out"
        need(not output.exists() and not output.is_symlink(), "new script output required")
        output.mkdir(mode=0o700)

    def compress_component_snapshot(self, source, destination):
        """Package exact decoded bytes without buffering compiler stdout or editing history."""
        need(all(child.poll() is not None for child in self.children), "snapshot compression requires stopped nodes")
        argv = [self.tools["zstd"], "--long=27", "-19", "-T2", "--stdout"]
        timeout = min(300, self.plan["deadline_unix"] - time.time() - 60)
        need(timeout > 0, "snapshot compression must retain cooperative drain time")
        with os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW), "rb") as original:
            before = os.fstat(original.fileno())
            need(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid() and before.st_nlink == 1
                 and 0 < before.st_size <= 2 * 1024**3, "bounded original snapshot required")
            with os.fdopen(os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as output:
                with os.fdopen(os.open(self.work / "snapshot-zstd.private.stderr",
                                       os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as errors:
                    child = subprocess.Popen(argv, cwd=self.work, env=self.env, stdin=original,
                                             stdout=output, stderr=errors, start_new_session=True)
                    self.command_children.append(child)
                    try:
                        child.wait(timeout=timeout)
                    except subprocess.TimeoutExpired:
                        need(os.getpgid(child.pid) == child.pid, "owned compression group changed")
                        os.killpg(child.pid, signal.SIGTERM)
                        try:
                            child.wait(timeout=60)
                        except subprocess.TimeoutExpired:
                            write_json(self.work / "SNAPSHOT-COMPRESSION-NEEDS-ATTENTION.json",
                                       {"scope": "AnvilComponentOnly", "pid": child.pid, "force_kill": False})
                            raise ValueError("owned snapshot compression remains alive after cooperative drain") from None
                        raise ValueError("snapshot compression exceeded finite execution deadline") from None
                    need(child.returncode == 0, "snapshot zstd command failed: exit_code=" + str(child.returncode))
                    output.flush()
                    os.fsync(output.fileno())
            fields = lambda s: (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_nlink,
                                s.st_size, s.st_mtime_ns, s.st_ctime_ns)
            need(fields(before) == fields(os.fstat(original.fileno())) == fields(source.stat()),
                 "original snapshot changed during compression")
        need(0 < destination.stat().st_size <= 64 * 1024**2, "bounded GitHub component compressed snapshot")

    def execute(self):
        for port in self.plan["ports"].values():
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", port))
        self.work.mkdir(mode=0o700)
        self.prepare_script_output()
        # The pinned L1 build script produces both native out/ and EraVM
        # zkout/. The required L2 priority deployment reads proxy/multicall
        # and Gateway admin artifacts from the latter, not the native files.
        # Keep the native inputs untouched and use the original EraVM skips.
        self.run([self.tools["forge"], "build", "--zksync", "--skip",
                  "*/l1-contracts/test/*", "L2BaseToken.sol", "Burner.sol", "--threads", "2"],
                 pathlib.Path(self.plan["era_root"]) / "contracts/l1-contracts")
        ecosystem_wallet = self.work / "public-ecosystem-wallets.yaml"
        chain_wallet = self.work / "public-chain-wallets.yaml"
        write_json(ecosystem_wallet, public_wallets(True))
        write_json(chain_wallet, public_wallets(False))
        self.start([self.tools["anvil"], "--host", "127.0.0.1", "--port", str(self.plan["ports"]["anvil"]),
                    "--chain-id", "31337", "--block-time", "0.25", "--auto-impersonate", "--silent"], "anvil")
        self.wait_rpc(self.url, 31337)
        need(int(self.rpc("eth_blockNumber", []), 16) < 8 and self.rpc("eth_getCode", [GOVERNANCE, "latest"]) == "0x", "fresh unmodified Anvil required")
        need(self.rpc("eth_getCode", [FACTORY, "latest"]) != "0x", "real deterministic factory deployment required")
        self.install_component_da_mock()
        for address in (GOVERNOR, gate.REVERTER, COMMIT_ADDRESS, EXECUTE_ADDRESS,
                        MANAGEMENT_ADDRESS, ROOT_PROVE_ADDRESS, *EDGE2_ADDRESSES,
                        "0x36615cf349d7f6344891b1e7ca7c72883f5dc049"):
            self.rpc("anvil_setBalance", [address, hex(10 ** 24)])
        self.run([self.tools["zkstack"], "--ignore-prerequisites", "ecosystem", "create", "--ecosystem-name", "component", "--l1-network", "localhost",
                  "--link-to-code", self.plan["era_root"], "--start-containers", "false",
                  *chain_args("gateway", 57001, ecosystem_wallet)], self.work)
        # Only these just-created public config outputs are replaced; no old workspace is admitted.
        initial = self.ecosystem / "configs/initial_deployments.yaml"
        need(initial.is_file() and not initial.is_symlink(), "fresh CLI initial configuration")
        initial.unlink()
        write_json(initial, {"create2_factory_addr": FACTORY, "create2_factory_salt": SALT,
            "governance_min_delay": 0, "token_weth_address": "0x0000000000000000000000000000000000000000",
            "bridgehub_create_new_chain_salt": 0, "max_number_of_chains": 100,
            "validator_timelock_execution_delay": 0, "gateway_settlement_fee": "0x3b9aca00"})
        wallet_path = self.ecosystem / "chains/gateway/configs/wallets.yaml"
        need(wallet_path.is_file() and not wallet_path.is_symlink(), "fresh Gateway wallet output")
        wallet_path.unlink()
        write_json(wallet_path, public_wallets(False))
        self.cli("ecosystem", "init", "--zksync-os", "--l1-rpc-url", self.url, "--update-submodules", "false",
                 "--deploy-ecosystem", "true", "--deploy-erc20", "false", "--deploy-paymaster", "false",
                 "--ecosystem-only", "--no-genesis", "--observability", "false", "--no-port-reallocation",
                 "--skip-contract-compilation-override")
        ecosystem_contracts = read_yaml(self.ecosystem / "configs/contracts.yaml")
        need(ecosystem_contracts["l1"]["governance_addr"].lower() == GOVERNANCE, "fixed guest governance constructor changed")
        need(self.call(GOVERNANCE, "owner()(address)").lower() == GOVERNOR, "real governance ownership")
        ctm_verifier = ecosystem_contracts["zksync_os_ctm"]["verifier_addr"]
        need(self.call(ctm_verifier, "verificationKeyHash()(bytes32)").lower() == gate.VK,
             "actual deployed accepted verification key")
        need(self.call(ctm_verifier, "IS_TESTNET_VERIFIER()(bool)").lower() == "true", "explicit component test verifier")
        self.cli("chain", "init", "--chain", "gateway", "--no-genesis", "--deploy-paymaster", "false", "--l1-rpc-url", self.url)
        # The Gateway script reads this artifact through its existing ./out
        # permission. Only the new generation Era tree is writable here.
        relay_out = pathlib.Path(self.plan["era_root"]) / "contracts/l1-contracts/out/anvil-component-relay"
        relay_parent = relay_out.parent.stat()
        need(relay_out.parent.resolve(strict=True) == relay_out.parent
             and stat.S_ISDIR(relay_parent.st_mode) and relay_parent.st_uid == os.getuid()
             and stat.S_IMODE(relay_parent.st_mode) == 0o700
             and not relay_out.exists() and not relay_out.is_symlink(),
             "fresh private generation relay namespace required")
        self.env["SYSCOIN_EDGE_DA_RELAY_ARTIFACT"] = str(self.build_component_relay(relay_out))
        self.cli("chain", "gateway", "create-tx-filterer", "--chain", "gateway", "--l1-rpc-url", self.url)
        self.cli("chain", "gateway", "convert-to-gateway", "--chain", "gateway", "--l1-rpc-url", self.url)
        bridgehub = ecosystem_contracts["core_ecosystem_contracts"]["bridgehub_proxy_addr"]
        reverter_grants = [self.grant_root_reverter("gateway")]
        # Queue genuine priority deposits before any settlement sender starts.
        # F39 remains the original tests' wallet/reverter, not Root prove; the
        # management actor needs Gateway gas only for ordinary CLI admin calls.
        for key in (gate.PUBLIC_KEY, MANAGEMENT_KEY):
            self.run([self.tools["deposit"], "--bridgehub", bridgehub, "--chain-id", "57001",
                      "--l1-rpc-url", self.url, "--amount", "1000", "--private-key", "0x" + key])
        genesis = self.work / "accepted-genesis.json"
        with os.fdopen(os.open(genesis, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as output:
            output.write(held(self.plan["genesis"], 1024 * 1024))
        configs = {}
        for name in ("gateway",):
            configs[name] = server_config(name, self.contracts(name), self.url, None, self.plan["ports"][name], genesis, self.work)
            config_path = self.work / (name + "-config.json")
            write_json(config_path, configs[name])
            self.start([self.tools["bootstrap"], str(config_path), str(self.plan["deadline_unix"])], name)
            self.wait_rpc("http://127.0.0.1:" + str(self.plan["ports"][name]), CHAIN_IDS[name])
        for address in (gate.REVERTER, MANAGEMENT_ADDRESS):
            while int(self.rpc("eth_getBalance", [address, "latest"], self.gateway_url), 16) == 0:
                self.remaining()
                time.sleep(0.2)
        # These are actual deployed-byte readbacks, not code/storage overrides.
        while True:
            try:
                self.assert_code(self.gateway_url, TIMELOCK, 2840, TIMELOCK_HASH)
                self.assert_code(self.gateway_url, RELAY, 1590, RELAY_HASH)
                break
            except ValueError:
                self.remaining()
                time.sleep(0.2)
        unpaused_readbacks = []
        for name in ("edge6565", "edge6566"):
            edge_wallet = self.work / (name + "-public-wallets.yaml")
            write_json(edge_wallet, public_wallets(False, name))
            self.cli("chain", "create", *chain_args(name, CHAIN_IDS[name], edge_wallet))
            self.cli("chain", "init", "--chain", name, "--no-genesis", "--deploy-paymaster", "false",
                     "--skip-priority-txs", "--pause-deposits", "--l1-rpc-url", self.url)
            self.cli("chain", "gateway", "migrate-to-gateway", "--chain", name, "--gateway-chain-name", "gateway",
                     "--l1-rpc-url", self.url, "--gateway-rpc-url", self.gateway_url)
            self.cli("chain", "gateway", "finalize-chain-migration-to-gateway", "--chain", name,
                     "--gateway-chain-name", "gateway", "--l1-rpc-url", self.url, "--gateway-rpc-url", self.gateway_url,
                     "--deploy-paymaster", "false")
            unpaused_readbacks.append(self.ensure_unpaused(name))
            reverter_grants.append(self.grant_gateway_reverter(name))
            configs[name] = server_config(name, self.contracts(name), self.url, self.gateway_url,
                                          self.plan["ports"][name], genesis, self.work)
            config_path = self.work / (name + "-config.json")
            write_json(config_path, configs[name])
            self.start([self.tools["bootstrap"], str(config_path), str(self.plan["deadline_unix"])], name)
            self.wait_rpc("http://127.0.0.1:" + str(self.plan["ports"][name]), CHAIN_IDS[name])
        deployment_readbacks = [self.chain_readback(name) for name in CHAIN_IDS]
        deposits = []
        for name, chain_id in CHAIN_IDS.items():
            # Existing public development signer is the deposit tool's source default.
            # No generated signer file or raw key argument is read/exported.
            self.run([self.tools["deposit"], "--bridgehub", bridgehub, "--chain-id", str(chain_id),
                      "--l1-rpc-url", self.url, "--amount", "1000"])
            url = "http://127.0.0.1:" + str(self.plan["ports"][name])
            while int(self.rpc("eth_getBalance", ["0x36615cf349d7f6344891b1e7ca7c72883f5dc049", "latest"], url), 16) == 0:
                self.remaining()
                time.sleep(0.2)
            deposits.append({"chain_id": chain_id, "actual_balance_positive": True})
        # Flush and stop only the fresh node processes first. L1 state is dumped
        # after their actual clean shutdown, never while senders can mutate it.
        self.stop_nodes(preserve_anvil=True)
        # Export only these newly generated public Noop replay records after
        # the node/writer has drained. No historical task DB or DB copy is an
        # input. A failure leaves an incomplete, unregistered package.
        package = pathlib.Path(self.plan["package_directory"])
        package.mkdir(mode=0o700)
        replay_archives = []
        for name, chain_id in CHAIN_IDS.items():
            archive = json.loads(self.run([self.tools["bootstrap"], "export-replay",
                str(self.work / (name + "-replay-archive")), str(self.work / (name + "-db")),
                str(chain_id), str(package)]))
            gate.checked_replay_manifest(archive)
            need(archive["chain_id"] == chain_id, "actual exported replay chain")
            manifest = package / "replay" / str(chain_id) / "manifest.json"
            manifest_raw = held({"path": str(manifest), "size": manifest.stat().st_size,
                                 "sha256": hashlib.sha256(manifest.read_bytes()).hexdigest()}, 1024 * 1024)
            need(json.loads(manifest_raw) == archive, "actual exported replay manifest join")
            for ref in archive["objects"]:
                gate.read_public(package, ref)
            # The seed node created this public marker from the actual deployment.
            # Keep its exact bytes; ordinary startup still compares it with live L1.
            marker = self.work / (name + "-db") / "database_identity.json"
            need(0 < marker.stat().st_size <= 16 * 1024, "bounded seed database identity")
            marker_raw = held({"path": str(marker), "size": marker.stat().st_size,
                               "sha256": hashlib.sha256(marker.read_bytes()).hexdigest()}, 16 * 1024)
            marker_doc = gate.checked_database_identity(json.loads(marker_raw), chain_id)
            need(marker_doc["diamond_proxy_l1"] == self.contracts(name)["l1"]["diamond_proxy_addr"].lower()
                 and marker_doc["l1_genesis_block_hash"] == self.rpc("eth_getBlockByNumber", ["0x0", False])["hash"]
                 and marker_doc["l2_genesis_block_hash"] == "0x" + archive["objects"][0]["path"].split("/")[3],
                 "seed database marker deployment/replay join")
            with os.fdopen(os.open(package / "replay" / str(chain_id) / "database_identity.json",
                                   os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as output:
                output.write(marker_raw)
                output.flush()
                os.fsync(output.fileno())
            replay_archives.append(archive)
        need(sum(len(archive["objects"]) for archive in replay_archives) <= 4096
             and sum(ref["size"] for archive in replay_archives for ref in archive["objects"]) <= 2 * 1024**3,
             "whole component replay bound")
        state = self.rpc("anvil_dumpState", [True])
        need(type(state) is str and state.startswith("0x"), "actual Anvil dump encoding")
        # Stop only these newly owned test processes; no old task processes are admitted.
        self.stop_nodes()
        raw_dump = bytes.fromhex(state[2:])
        need(len(raw_dump) <= 256 * 1024 * 1024, "bounded compressed state archive")
        # Preserve the exact dump before decoding: a size failure must not
        # require another deployment just to recover its original input.
        original_dump = self.work / "anvil-state.original.json.gz"
        with os.fdopen(os.open(original_dump, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as output:
            output.write(raw_dump)
        replay_state = self.work / "restored-state.json"
        decoded_size, decoded_hash = 0, hashlib.sha256()
        decoded_ceiling = 2 * 1024**3
        with os.fdopen(os.open(replay_state, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as output:
            with gzip.GzipFile(fileobj=io.BytesIO(raw_dump)) as compressed:
                while decoded_size <= decoded_ceiling:
                    chunk = compressed.read(min(1024 * 1024, decoded_ceiling + 1 - decoded_size))
                    if not chunk:
                        break
                    output.write(chunk)
                    decoded_hash.update(chunk)
                    decoded_size += len(chunk)
        snapshot_sizes = {"compressed_state_bytes": len(raw_dump), "decoded_state_bytes": decoded_size,
                          "decoded_size_is_exact": decoded_size <= decoded_ceiling,
                          "decoded_ceiling_bytes": decoded_ceiling,
                          "historical_states_preserved": True}
        write_json(self.work / "SNAPSHOT-SIZE.actual.json", snapshot_sizes)
        need(decoded_size <= decoded_ceiling, "bounded decoded state archive")
        # Before completing even an unregistered package, exercise the exact
        # dump and public archive against fresh WALs and normal node startup.
        # The unchanged raw JSON is streamed verbatim; the genuine Anvil load
        # and fresh-node restoration below validate it without a second giant
        # Python JSON object or a modified/filtered historical snapshot.
        self.children = []  # All original owned children were joined cleanly above.
        self.start([self.tools["anvil"], "--host", "127.0.0.1", "--port", str(self.plan["ports"]["anvil"]),
                    "--chain-id", "31337", "--load-state", str(replay_state), "--block-time", "0.25",
                    "--mixed-mining", "--slots-in-an-epoch", "10", "--silent"], "restored-anvil")
        self.wait_rpc(self.url, 31337)
        restored = []
        need(self.rpc("eth_getCode", [COMPONENT_DA_ADDRESS, "latest"]) == self.component_da_runtime,
             "restored component DA mock code changed")
        need(self.rpc("eth_getCode", [self.component_da_probe, "latest"]) == self.component_da_probe_runtime,
             "restored component gas probe code changed")
        self.component_da_evidence["actual_dump_restore_runtime_match"] = True
        self.component_da_evidence["actual_restored_gas_controls"] = self.component_da_controls()
        for name, chain_id in CHAIN_IDS.items():
            cfg = json.loads(json.dumps(configs[name]))
            cfg["general"]["rocks_db_path"] = str(self.work / ("restored-" + name + "-db"))
            cfg["prover_api"]["proof_storage"]["path"] = str(self.work / ("restored-" + name + "-proofs"))
            cfg["replay_archive"]["root_path"] = str(self.work / ("restored-" + name + "-replay-archive"))
            archive = next(archive for archive in replay_archives if archive["chain_id"] == chain_id)
            manifest = package / "replay" / str(chain_id) / "manifest.json"
            self.run([self.tools["bootstrap"], "recover-replay", str(package), str(manifest),
                      hashlib.sha256(manifest.read_bytes()).hexdigest(), cfg["general"]["rocks_db_path"]])
            marker = package / "replay" / str(chain_id) / "database_identity.json"
            marker_raw = held({"path": str(marker), "size": marker.stat().st_size,
                               "sha256": hashlib.sha256(marker.read_bytes()).hexdigest()}, 16 * 1024)
            gate.checked_database_identity(json.loads(marker_raw), chain_id)
            destination = pathlib.Path(cfg["general"]["rocks_db_path"])
            need(destination.resolve(strict=True) == destination and destination.stat().st_uid == os.getuid()
                 and stat.S_IMODE(destination.stat().st_mode) == 0o700,
                 "fresh recovered database root identity")
            with os.fdopen(os.open(destination / "database_identity.json",
                                   os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as output:
                output.write(marker_raw)
                output.flush()
                os.fsync(output.fileno())
            config_path = self.work / ("restored-" + name + "-config.json")
            write_json(config_path, cfg)
            self.start([self.tools["bootstrap"], str(config_path), str(self.plan["deadline_unix"])], "restored-" + name)
            url = "http://127.0.0.1:" + str(self.plan["ports"][name])
            self.wait_rpc(url, chain_id)
            need(int(self.rpc("eth_blockNumber", [], url), 16) >= archive["anchor_block_number"],
                 "actual fresh replay head")
            anchor = self.rpc("eth_getBlockByNumber", [hex(archive["anchor_block_number"]), False], url)
            need(type(anchor) is dict and anchor.get("hash") == archive["anchor_block_hash"],
                 "actual fresh replay canonical anchor")
            # Source-published development account used by the original tests.
            # Never read/export a generated signer or reuse private chain state.
            sender = "0x36615cf349d7f6344891b1e7ca7c72883f5dc049"
            need(int(self.rpc("eth_getBalance", [sender, "latest"], url), 16) > 0,
                 "fresh restored test wallet is not funded")
            receipt = json.loads(self.run([self.tools["cast"], "send", "--json", "--rpc-url", url,
                "--private-key", "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
                "0x5a67ee02274d9ec050d412b96fe810be4d71e7a0", "--value", "1"]))
            need(receipt.get("status") in ("0x1", 1), "actual restored component transaction failed")
            actual = self.rpc("eth_getTransactionReceipt", [receipt["transactionHash"]], url)
            need(actual == receipt or (type(actual) is dict and actual.get("status") == "0x1"
                 and actual.get("transactionHash") == receipt["transactionHash"]), "actual restored receipt join")
            restored.append({"chain_id": chain_id, "transaction_hash": receipt["transactionHash"],
                             "actual_receipt_status": 1, "fresh_node_db": True,
                             "public_replay_anchor_block_number": archive["anchor_block_number"],
                             "public_replay_anchor_block_hash": archive["anchor_block_hash"]})
        self.assert_code(self.gateway_url, TIMELOCK, 2840, TIMELOCK_HASH)
        self.assert_code(self.gateway_url, RELAY, 1590, RELAY_HASH)
        self.stop_nodes()
        for name, cfg in configs.items():
            cfg = json.loads(json.dumps(cfg))
            cfg["general"]["rocks_db_path"] = "./db"
            cfg["genesis"]["genesis_input_path"] = "./local-chains/anvil-component-only/v32.0/default/genesis.json"
            cfg["l1_provider"]["rpc_url"] = "http://127.0.0.1:8545"
            cfg["prover_api"]["proof_storage"]["path"] = "./db/proof_storage"
            # Consumers recover the source-registered public archive into a
            # fresh WAL; no generator root or packaged archive is a writer.
            cfg["replay_archive"] = {"type": "Noop"}
            if "gateway_provider" in cfg:
                cfg["gateway_provider"]["rpc_url"] = "http://127.0.0.1:3052"
            cfg["rpc"]["address"] = "127.0.0.1:" + ("3052" if name == "gateway" else "3050")
            relative = "multi_chain/chain_" + str(CHAIN_IDS[name]) + ".yaml"
            write_json(package / relative, cfg)
            if name == "gateway":
                # Direct-L1 tests instantiate a separate runtime from this
                # same genuine Gateway seed. This is a config view, not a
                # second physical chain or a concurrent duplicate producer.
                direct_l1_view = json.loads(json.dumps(cfg))
                direct_l1_view["rpc"]["address"] = "127.0.0.1:3050"
                gate.checked_gateway_views(direct_l1_view, cfg)
                write_json(package / "default/config.yaml", direct_l1_view)
        (package / "default/genesis.json").write_bytes(held(self.plan["genesis"], 1024 * 1024))
        os.chmod(package / "default/genesis.json", 0o600)
        self.compress_component_snapshot(replay_state, package / "l1-state.json.zst")
        snapshot_sizes["packaged_compression"] = "zstd-long27-level19"
        snapshot_sizes["packaged_compressed_state_bytes"] = (package / "l1-state.json.zst").stat().st_size
        write_json(package / "versions.yaml", {"version": 32, "execution_version": 7, "proving_version": 8})
        write_json(package / "deployment.json", {"scope": "AnvilComponentOnly", "governance": GOVERNANCE,
            "component_da_mock": self.component_da_evidence,
            "timelock": TIMELOCK, "timelock_runtime_hash": TIMELOCK_HASH, "relay": RELAY, "relay_runtime_hash": RELAY_HASH,
            "chains": {name: self.contracts(name) for name in CHAIN_IDS}, "deposit_readbacks": deposits,
            "layout_views": {"default/config.yaml": {"physical_chain": "gateway", "chain_id": 57001,
                "settlement_layer": "L1", "same_replay_seed_as": "multi_chain/chain_57001.yaml",
                "distinct_runtime_per_test": True}},
            "actual_reverter_grants": reverter_grants,
            "actual_snapshot_byte_counts": snapshot_sizes,
            "actual_deposit_state_readbacks": unpaused_readbacks,
            "actual_chain_readbacks": deployment_readbacks, "actual_fresh_restore_rpc_transactions": restored})
        write_json(package / "proof-identities.json", {"scope": "AnvilComponentOnly", "security_bits": 100,
            "execution_version": 7, "proving_version": 8, "vk_hash": gate.VK, "real_proof_qualification": False})
        write_json(package / "source-identities.json", {"server_revision": SERVER_HEAD, "zkstack_base_tree": CLI_TREE,
            "source_attestation": self.plan["source_attestation"], "component_adapter_sha256": hashlib.sha256((ROOT / "scripts/fixtures/zkstack-anvil-component.patch").read_bytes()).hexdigest()})
        write_json(package / "test-signers.json", {"scope": "PublicInsecureDevelopmentOnly", "reverter_address": gate.REVERTER,
            "management_address": MANAGEMENT_ADDRESS,
            "settlement_operator_addresses": {name: {label: address for label, (address, _) in zip(
                ("commit", "prove", "execute"), operator_roles(name))} for name in CHAIN_IDS},
            "root_governor_public_address": GOVERNOR,
            "root_governor_private_key_used": False, "anvil_impersonation": True})
        def identity(relative):
            path = package / relative
            data = path.read_bytes()
            return {"path": relative, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        descriptor = {"schema_version": 1, "scope": "AnvilComponentOnly", "protocol_version": "v32.0",
            "execution_version": 7, "proving_version": 8, "security_bits": 100, "verification_key_hash": gate.VK,
            "root_chain_id": 31337, "default_chain_id": 57001, "gateway_chain_id": 57001, "edge_chain_ids": [6565, 6566],
            "reverter_address": gate.REVERTER, "compressed_state": identity("l1-state.json.zst"),
            "decompressed_state": {"path": "l1-state.json", "size": decoded_size, "sha256": decoded_hash.hexdigest()},
            "files": [identity(relative) for relative in sorted(gate.FILES)], "replay_archives": replay_archives}
        gate.checked_descriptor(descriptor)
        write_json(package / "anvil-component.json", descriptor)
        # No source registration is written. Actual config smoke and all original
        # 110 live integration tests are separate mandatory post-generation gates.
        return {"scope": "AnvilComponentOnly", "package_directory": str(package),
                "descriptor_sha256": hashlib.sha256((package / "anvil-component.json").read_bytes()).hexdigest(),
                "actual_snapshot_byte_counts": snapshot_sizes,
                "fresh_restore_bootstrap_rpc_transactions": True,
                "canonical_syscoin_qualification": False, "integration_suite_qualification": False,
                "config_smoke_qualification": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan")
    parser.add_argument("sha256")
    args = parser.parse_args()
    path = absolute_path(args.plan)
    need(gate.digest(args.sha256), "reviewed plan SHA required")
    raw = held({"path": str(path), "size": path.stat().st_size, "sha256": args.sha256}, 64 * 1024)
    plan = json.loads(raw)
    checked_plan(plan, time.time())
    need(plan["execute"] is True, "inert plan has no generation authority")
    need(str(pathlib.Path(sys.executable).resolve()) == plan["tools"]["python"]["path"], "reviewed Python interpreter required")
    run = Generation(plan)
    try:
        result = run.execute()
        write_json(run.work / "GENERATION-RESULT.json", result)
        print(json.dumps(result, sort_keys=True))
    except BaseException:
        # Cleanup must not replace the original command/dependency failure.
        try:
            run.stop_nodes()
        except (ValueError, OSError) as cleanup:
            print(json.dumps({"owned_cleanup_failed": True, "reason": str(cleanup)[:240],
                              "force_kill": False}), file=sys.stderr)
        raise
    else:
        run.stop_nodes()


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, json.JSONDecodeError, KeyError, ImportError) as error:
        reason = str(error)[:240] if type(error) is ValueError else type(error).__name__
        print(json.dumps({"component_generation_failed_closed": True, "reason": reason,
                          "registration": False, "canonical_qualification": False,
                          "automatic_retry": False}), file=sys.stderr)
        raise SystemExit(1) from None
