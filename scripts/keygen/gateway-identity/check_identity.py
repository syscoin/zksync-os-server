#!/usr/bin/env python3
"""Offline ABI/CREATE2 cross-check. No RPC, subprocesses, signer, or network access."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

MASK = (1 << 64) - 1
ROUND_CONSTANTS = (
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
)
ROTATIONS = ((0, 36, 3, 41, 18), (1, 44, 10, 45, 2), (62, 6, 43, 15, 61),
             (28, 55, 25, 21, 56), (27, 20, 39, 8, 14))
FACTORY = "0x4e59b44847b379578588920ca78fbf26c0b4956c"
BRIDGEHUB = "0x0000000000000000000000000000000000010002"
ALIAS_OFFSET = 0x1111000000000000000000000000000000001111
SELECTORS = ("adminSelectors", "executorSelectors", "mailboxSelectors", "gettersSelectors",
             "migratorSelectors", "committerSelectors")
CONFIG_HASHES = ("bootloaderHash", "defaultAccountHash", "evmEmulatorHash", "genesisRoot",
                 "genesisBatchCommitment")
ARTIFACTS = {
    "l1_governance": ("Governance.sol", "Governance"),
    "proxy_admin_deployer": ("GatewayCTMDeployerProxyAdmin.sol", "GatewayCTMDeployerProxyAdmin"),
    "proxy_admin": ("ProxyAdmin.sol", "ProxyAdmin"),
    "validator_timelock_deployer": ("GatewayCTMDeployerValidatorTimelock.sol", "GatewayCTMDeployerValidatorTimelock"),
    "validator_timelock_implementation": ("ValidatorTimelock.sol", "ValidatorTimelock"),
    "validator_timelock": ("TransparentUpgradeableProxy.sol", "TransparentUpgradeableProxy"),
}


def keccak256(data):
    """Legacy Keccak-256 (Ethereum), deliberately independent of Foundry's helper."""
    def rotate(value, count):
        return ((value << count) | (value >> ((64 - count) % 64))) & MASK

    padded = bytearray(data)
    padded.append(0x01)
    padded.extend(b"\0" * ((-len(padded)) % 136))
    padded[-1] |= 0x80
    state = [0] * 25
    for start in range(0, len(padded), 136):
        block = padded[start:start + 136]
        for lane in range(17):
            state[lane] ^= int.from_bytes(block[8 * lane:8 * lane + 8], "little")
        for constant in ROUND_CONSTANTS:
            columns = [state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20]
                       for x in range(5)]
            for x in range(5):
                delta = columns[(x - 1) % 5] ^ rotate(columns[(x + 1) % 5], 1)
                for y in range(5):
                    state[x + 5 * y] ^= delta
            moved = [0] * 25
            for x in range(5):
                for y in range(5):
                    moved[y + 5 * ((2 * x + 3 * y) % 5)] = rotate(state[x + 5 * y], ROTATIONS[x][y])
            for x in range(5):
                for y in range(5):
                    state[x + 5 * y] = moved[x + 5 * y] ^ ((~moved[(x + 1) % 5 + 5 * y]) & moved[(x + 2) % 5 + 5 * y])
            state[0] ^= constant
    return b"".join(lane.to_bytes(8, "little") for lane in state)[:32]


def hex_bytes(value, length=None):
    if not isinstance(value, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})*", value):
        raise ValueError("expected explicit 0x-prefixed hexadecimal bytes")
    result = bytes.fromhex(value[2:])
    if length is not None and len(result) != length:
        raise ValueError(f"expected {length} bytes, got {len(result)}")
    return result


def as_hex(value):
    return "0x" + value.hex()


def address(value):
    result = hex_bytes(value, 20)
    if not any(result):
        raise ValueError("zero address is not an approved deployment identity")
    return as_hex(result)


def word(value):
    if type(value) is not int or not 0 <= value < 1 << 256:
        raise ValueError("expected uint256 JSON integer")
    return value.to_bytes(32, "big")


def address_word(value):
    return b"\0" * 12 + hex_bytes(value, 20)


def create2(deployer, salt, init_code):
    return as_hex(keccak256(b"\xff" + hex_bytes(deployer, 20) + hex_bytes(salt, 32) + keccak256(init_code))[-20:])


def alias(value):
    return as_hex(((int(address(value), 16) + ALIAS_OFFSET) % (1 << 160)).to_bytes(20, "big"))


def proxy_args(implementation, admin, owner):
    initialization = keccak256(b"initialize(address,uint32)")[:4] + address_word(owner) + word(0)
    dynamic = word(len(initialization)) + initialization + b"\0" * ((-len(initialization)) % 32)
    return address_word(implementation) + address_word(admin) + word(96) + dynamic


def load_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    raw = path.read_bytes()
    return raw, json.loads(raw, object_pairs_hook=unique)


def require_equal(actual, expected, label):
    if actual != expected:
        raise ValueError(f"{label} mismatch: expected {expected!r}, got {actual!r}")


def validate_input(data):
    require_equal(data["schema"], "syscoin-v32-offline-identity-input-v1", "input schema")
    require_equal(data["deployed"], False, "offline status")
    if type(data["deployed"]) is not bool:
        raise ValueError("deployed must be a JSON boolean")
    if not isinstance(data["namespace"], str) or not data["namespace"].strip():
        raise ValueError("explicit fresh/reused namespace description is required")
    require_equal(address(data["factory"]), FACTORY, "universal factory")
    hex_bytes(data["outer_salt"], 32)
    hex_bytes(data["relay_salt"], 32)
    config = data["config"]
    if data["scope"] not in ("critical-subtree", "full-ctm"):
        raise ValueError("unknown identity calculation scope")
    require_equal(config["salt"].lower(), data["outer_salt"].lower(), "canonical outer/inner salt")
    require_equal(address(config["aliasedGovernanceAddress"]), alias(data["l1_governance"]), "governance alias")
    if config["isZKsyncOS"] is not True or config["testnetVerifier"] is not False:
        raise ValueError("real ZKsync OS verifier mode is required")
    if data["scope"] == "full-ctm":
        for field in ("eraChainId", "l1ChainId", "genesisRollupLeafIndex", "protocolVersion"):
            word(config[field])
        if not config["eraChainId"] or not config["l1ChainId"]:
            raise ValueError("chain IDs must be positive")
        require_equal(config["protocolVersion"], 32 << 32, "packed protocol 0.32.0")
        for field in SELECTORS:
            if not isinstance(config[field], list) or not config[field]:
                raise ValueError(f"explicit nonempty {field} required")
            for item in config[field]:
                hex_bytes(item, 4)
        for field in CONFIG_HASHES:
            hex_bytes(config[field], 32)
        hex_bytes(config["forceDeploymentsData"])
    root = data["root_governance_constructor"]
    address(root["admin"])
    hex_bytes(root["security_council"], 20)
    word(root["min_delay_seconds"])
    bindings = data["source_bindings"]
    for field in ("era_base_commit", "era_source_patched_tree", "zksync_os_patched_tree"):
        if not re.fullmatch(r"[0-9a-f]{40}", bindings[field]) or set(bindings[field]) == {"0"}:
            raise ValueError(f"explicit nonzero {field} required")
    return config


def resolve_source(era_root, name):
    remappings = {
        "@openzeppelin/contracts-v4/": "l1-contracts/lib/openzeppelin-contracts-v4/contracts/",
        "@openzeppelin/contracts-upgradeable-v4/": "l1-contracts/lib/openzeppelin-contracts-upgradeable-v4/contracts/",
        "forge-std/": "l1-contracts/lib/forge-std/src/",
        "l2-contracts/": "l2-contracts/contracts/",
        "system-contracts/": "system-contracts/",
    }
    candidate = era_root / "l1-contracts" / name
    for prefix, target in remappings.items():
        if name.startswith(prefix):
            candidate = era_root / target / name[len(prefix):]
            break
    resolved = candidate.resolve(strict=True)
    resolved.relative_to(era_root.resolve(strict=True))
    if not resolved.is_file():
        raise ValueError(f"not a source file: {name}")
    return resolved


def artifact(era_root, path, evm_version):
    raw, data = load_json(path)
    metadata = data.get("metadata") or data.get("rawMetadata")
    if isinstance(metadata, str):
        metadata = json.loads(metadata)
    if not isinstance(metadata, dict):
        raise ValueError(f"missing compiler metadata: {path}")
    require_equal(metadata["compiler"]["version"], "0.8.28+commit.7893614a", "solc version")
    settings = metadata["settings"]
    require_equal(settings["evmVersion"], evm_version, "artifact EVM version")
    require_equal(settings["optimizer"]["enabled"], True, "optimizer enabled")
    require_equal(settings["optimizer"]["runs"], 9999999, "optimizer runs")
    require_equal(settings.get("viaIR", False), False, "canonical non-IR pipeline")
    require_equal(settings["metadata"]["bytecodeHash"], "ipfs", "canonical metadata hash")
    require_equal(settings["metadata"].get("appendCBOR", True), True, "canonical CBOR metadata")
    require_equal(settings.get("libraries", {}), {}, "no external library substitutions")
    for source_name, item in metadata["sources"].items():
        actual = as_hex(keccak256(resolve_source(era_root, source_name).read_bytes()))
        require_equal(actual, item["keccak256"].lower(), f"artifact source {source_name}")
    creation = data["bytecode"]
    if creation.get("linkReferences"):
        raise ValueError(f"external creation links require separate review: {path}")
    creation_bytes = hex_bytes(creation["object"])
    runtime = data["deployedBytecode"]
    runtime_bytes = hex_bytes(runtime["object"])
    if not creation_bytes or not runtime_bytes:
        raise ValueError(f"empty artifact: {path}")
    return {
        "creation": creation_bytes,
        "runtime": runtime_bytes,
        "immutable_references": runtime.get("immutableReferences", {}),
        "runtime_links": runtime.get("linkReferences", {}),
        "evidence": {
            "path": str(path.resolve()),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "creation_keccak256": as_hex(keccak256(creation_bytes)),
            "compiler": metadata["compiler"]["version"],
            "evm_version": evm_version,
            "source_hashes_checked": True,
        },
    }


def verify(input_path, helper_path, era_root, relay_path, artifacts_root):
    raw, data = load_json(input_path)
    config = validate_input(data)
    helper_raw, helper = load_json(helper_path)
    require_equal(helper["schema"], "syscoin-v32-solidity-identity-output-v1", "helper schema")
    require_equal(helper["status"], "offline-derived-not-deployed", "helper status")
    require_equal(helper["scope"], data["scope"], "helper scope")
    if helper["deployed"] is not False or helper["independent_host_reproduction"] is not False:
        raise ValueError("helper must not claim deployment/independent-host reproduction")
    require_equal(helper["input_keccak256"].lower(), as_hex(keccak256(raw)), "exact input bytes")
    artifacts = {
        name: artifact(era_root, artifacts_root / file / (contract + ".json"), "cancun")
        for name, (file, contract) in ARTIFACTS.items()
    }
    artifacts["relay"] = artifact(era_root, relay_path, "prague")
    salt = config["salt"].lower()
    owner = alias(data["l1_governance"])
    salt_word = hex_bytes(salt, 32)
    addresses = {}
    nodes = {}

    def derive(name, deployer, constructor_args, node_salt=salt):
        init_code = artifacts[name]["creation"] + constructor_args
        addresses[name] = create2(deployer, node_salt, init_code)
        require_equal(address(helper[name]), addresses[name], name)
        nodes[name] = {
            "factory": deployer, "salt": node_salt, "address": addresses[name],
            "constructor_args": as_hex(constructor_args),
            "constructor_args_keccak256": as_hex(keccak256(constructor_args)),
            "init_code_keccak256": as_hex(keccak256(init_code)),
            **artifacts[name]["evidence"],
        }
        return init_code

    root = data["root_governance_constructor"]
    derive("l1_governance", FACTORY, address_word(root["admin"]) + address_word(root["security_council"]) + word(root["min_delay_seconds"]))
    require_equal(address(data["l1_governance"]), addresses["l1_governance"], "planned root governance")
    pa_init = derive("proxy_admin_deployer", FACTORY, salt_word + address_word(owner))
    derive("proxy_admin", addresses["proxy_admin_deployer"], b"")
    vt_init = derive("validator_timelock_deployer", FACTORY,
                     salt_word + address_word(owner) + address_word(addresses["proxy_admin"]))
    derive("validator_timelock_implementation", addresses["validator_timelock_deployer"], address_word(BRIDGEHUB))
    derive("validator_timelock", addresses["validator_timelock_deployer"],
           proxy_args(addresses["validator_timelock_implementation"], addresses["proxy_admin"], owner))
    derive("relay", FACTORY, b"", data["relay_salt"].lower())
    for key, expected in (("l1_governance", address(data["l1_governance"])), ("aliased_governance", owner),
                          ("factory", FACTORY), ("outer_salt", salt), ("inner_salt", salt),
                          ("relay_salt", data["relay_salt"].lower())):
        require_equal(helper[key].lower(), expected, key)
    require_equal(hex_bytes(helper["proxy_admin_factory_calldata"]), salt_word + pa_init, "ProxyAdmin factory calldata")
    require_equal(hex_bytes(helper["validator_timelock_factory_calldata"]), salt_word + vt_init, "ValidatorTimelock factory calldata")
    for name in ("validator_timelock", "relay"):
        item = artifacts[name]
        if item["immutable_references"] or item["runtime_links"]:
            raise ValueError(f"{name} runtime cannot be used before immutable/link resolution")
        nodes[name]["runtime_size"] = len(item["runtime"])
        nodes[name]["runtime_keccak256"] = as_hex(keccak256(item["runtime"]))
    require_equal(helper["relay_init_code_hash"].lower(), nodes["relay"]["init_code_keccak256"], "source relay init-code pin")
    require_equal(helper["relay_runtime_hash"].lower(), nodes["relay"]["runtime_keccak256"], "source relay runtime pin")
    return {
        "schema": "syscoin-v32-offline-guest-bound-identity-v1",
        "status": "offline-derived-not-deployed",
        "scope": data["scope"],
        "deployed": False,
        "production_attested": False,
        "independent_host_reproduction": False,
        "independent_create2_calculation_matches_helper": True,
        "source_bindings": data["source_bindings"],
        "source_binding_scope": "operator-supplied tree identities; artifact metadata source hashes independently checked",
        "inputs": data,
        "input_sha256": hashlib.sha256(raw).hexdigest(),
        "helper_output_sha256": hashlib.sha256(helper_raw).hexdigest(),
        "checker_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "derivations": nodes,
        "downstream_values_are_provisional": True,
        "provisional_downstream": {key: helper[key] for key in
                                   ("provisional_verifier", "provisional_plonk_verifier", "provisional_ctm") if key in helper},
        "full_ctm_calculate_addresses_executed": data["scope"] == "full-ctm",
        "required_post_vk_check": "Recompile and rerun; compare guest-bound constructor/init-code/runtime hashes and addresses, while separately updating verifier/CTM/bundle identities.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--helper-output", type=Path, required=True)
    parser.add_argument("--era-root", type=Path, required=True)
    parser.add_argument("--relay-artifact", type=Path, required=True)
    parser.add_argument("--artifacts-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.input, args.helper_output, args.era_root, args.relay_artifact, args.artifacts_root)
    # Exclusive creation keeps a repeated check from silently replacing prior evidence.
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
    print(f"Offline CREATE2 cross-check passed; NOT deployed: {args.output}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        sys.exit(f"identity check failed: {error}")
