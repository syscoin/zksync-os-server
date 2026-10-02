#!/usr/bin/env python3
"""Read-only public-package gate for V32/V8 Anvil component CI, not release CI.

No generator, registry writer, wallet importer, RPC, process or canonical fallback.
The registration must be issued in source review only after genuine generation.
"""
import hashlib
import json
import os
import pathlib
import re
import stat
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parents[2]
REGISTRATION = ROOT / "scripts/fixtures/v32-component-registration.json"
FIXTURE = ROOT / "local-chains/anvil-component-only/v32.0"
GENESIS = "5adf0dd1b618911d51c335e983c0c71cc1c74fc7db37161bf76a4b51e5055a95"
VK = "0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe"
PUBLIC_KEY = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
# Intentionally public, insecure development signers, with separate nonce owners
# for Root/Gateway/each edge. The unchanged validator requires distinct roles.
PUBLIC_COMMIT_KEY = "0" * 63 + "1"
PUBLIC_EXECUTE_KEY = "0" * 63 + "2"
PUBLIC_ROOT_PROVE_KEY = "0" * 63 + "7"
PUBLIC_EDGE2_KEYS = tuple("0" * 63 + str(scalar) for scalar in (4, 5, 6))
PUBLIC_KEYS = {PUBLIC_KEY, PUBLIC_COMMIT_KEY, PUBLIC_EXECUTE_KEY,
               PUBLIC_ROOT_PROVE_KEY, *PUBLIC_EDGE2_KEYS}
REVERTER = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
FILES = {
    "default/config.yaml", "default/genesis.json", "deployment.json",
    "multi_chain/chain_6565.yaml", "multi_chain/chain_6566.yaml",
    "multi_chain/chain_57001.yaml", "proof-identities.json",
    "source-identities.json", "test-signers.json", "versions.yaml",
    "replay/57001/database_identity.json", "replay/6565/database_identity.json",
    "replay/6566/database_identity.json",
}
FIELDS = {
    "schema_version", "scope", "protocol_version", "execution_version",
    "proving_version", "security_bits", "verification_key_hash", "root_chain_id",
    "default_chain_id", "gateway_chain_id", "edge_chain_ids", "reverter_address", "compressed_state",
    "decompressed_state", "files", "replay_archives",
}
REPLAY_CHAINS = {57001, 6565, 6566}
REPLAY_FIELDS = {"schema", "chain_id", "anchor_block_number", "anchor_block_hash", "objects"}


def checked_database_identity(doc, chain_id):
    """Public marker created by the fresh node, never a reconstructed DB identity."""
    need(type(doc) is dict and set(doc) == {"schema_version", "protocol_version", "l1_chain_id",
         "l1_genesis_block_hash", "l2_chain_id", "diamond_proxy_l1", "l2_genesis_block_hash"},
         "component database identity shape")
    need(type(doc["schema_version"]) is int and doc["schema_version"] == 1
         and doc["protocol_version"] == "v32.0"
         and type(doc["l1_chain_id"]) is int and doc["l1_chain_id"] == 31337
         and type(doc["l2_chain_id"]) is int and doc["l2_chain_id"] == chain_id
         and chain_id in REPLAY_CHAINS, "component database deployment identity")
    for field in ("l1_genesis_block_hash", "l2_genesis_block_hash"):
        need(type(doc[field]) is str and doc[field].startswith("0x") and digest(doc[field][2:]),
             "component database genesis hash")
    address = doc["diamond_proxy_l1"]
    need(type(address) is str and re.fullmatch(r"0x[0-9a-f]{40}", address) is not None
         and address != "0x" + "0" * 40, "component database diamond address")
    return doc


def checked_replay_manifest(doc):
    """Only public Noop archives from newly generated component chains."""
    need(type(doc) is dict and set(doc) == REPLAY_FIELDS
         and doc["schema"] == "syscoin-v32-component-replay-archive-v1", "replay manifest shape")
    chain, head, anchor = doc["chain_id"], doc["anchor_block_number"], doc["anchor_block_hash"]
    need(type(chain) is int and chain in REPLAY_CHAINS and type(head) is int and 0 <= head < 4096,
         "component replay chain/head")
    need(type(anchor) is str and anchor.startswith("0x") and digest(anchor[2:]), "replay canonical anchor")
    objects = doc["objects"]
    need(type(objects) is list and len(objects) == head + 1, "contiguous replay object count")
    for number, ref in enumerate(objects):
        identity(ref)
        parts = ref["path"].split("/")
        need(len(parts) == 5 and parts[:3] == ["replay", str(chain), str(number)]
             and digest(parts[3]) and re.fullmatch(r"(?:0|[1-9][0-9]*)-[0-9a-f]{32}-.+", parts[4]) is not None
             and parts[4].split("-", 2)[2] not in {".", ".."}
             and "\\" not in parts[4] and ref["size"] <= 256 * 1024 * 1024, "replay object path/bound")
    need(objects[-1]["path"].split("/")[3] == anchor[2:], "replay anchor object mismatch")
    need(sum(ref["size"] for ref in objects) <= 2 * 1024 * 1024 * 1024, "replay byte bound")
    return doc


def need(ok, message):
    if not ok:
        raise ValueError(message)


def digest(value):
    return (type(value) is str and len(value) == 64 and value != "0" * 64
            and all(c in "0123456789abcdef" for c in value))


def identity(ref):
    need(type(ref) is dict and set(ref) == {"path", "size", "sha256"}, "file identity shape")
    path = ref["path"]
    need(type(path) is str and path and "\\" not in path
         and not path.startswith("/") and all(p not in {"", ".", ".."} for p in path.split("/")),
         "unsafe file path")
    need(type(ref["size"]) is int and ref["size"] > 0 and digest(ref["sha256"]), "file identity")


def checked_descriptor(doc):
    need(type(doc) is dict and set(doc) == FIELDS, "component descriptor shape")
    for name, value in {
        "schema_version": 1, "scope": "AnvilComponentOnly", "protocol_version": "v32.0",
        "execution_version": 7, "proving_version": 8, "security_bits": 100,
        "verification_key_hash": VK, "root_chain_id": 31337, "default_chain_id": 57001, "gateway_chain_id": 57001,
        "edge_chain_ids": [6565, 6566], "reverter_address": REVERTER,
    }.items():
        need(type(doc[name]) is type(value) and doc[name] == value, "component current identity")
        if type(value) is list:
            need(all(type(item) is int for item in doc[name]), "component chain types")
    identity(doc["compressed_state"])
    identity(doc["decompressed_state"])
    need(doc["compressed_state"]["path"] == "l1-state.json.zst"
         and doc["decompressed_state"]["path"] == "l1-state.json", "explicit Anvil state paths")
    need(doc["compressed_state"]["size"] <= 64 * 1024 * 1024
         and doc["decompressed_state"]["size"] <= 2 * 1024**3, "component state size bound")
    need(type(doc["files"]) is list and len(doc["files"]) == len(FILES), "public artifact count")
    for ref in doc["files"]:
        identity(ref)
        if ref["path"].endswith("/database_identity.json"):
            need(ref["size"] <= 16 * 1024, "bounded component database identity")
    need({ref["path"] for ref in doc["files"]} == FILES, "public artifact roles")
    need(next(ref for ref in doc["files"] if ref["path"] == "default/genesis.json")["sha256"] == GENESIS,
         "accepted current genesis")
    archives = doc["replay_archives"]
    need(type(archives) is list and len(archives) == 3, "three physical component replay archives required")
    for archive in archives:
        checked_replay_manifest(archive)
    need({archive["chain_id"] for archive in archives} == REPLAY_CHAINS, "component replay chain set")
    need(sum(len(archive["objects"]) for archive in archives) <= 4096
         and sum(ref["size"] for archive in archives for ref in archive["objects"]) <= 2 * 1024 * 1024 * 1024,
         "whole component replay bound")
    return doc


def read_public(root, ref):
    identity(ref)
    path = root
    need(root.is_dir() and not root.is_symlink(), "fixture root")
    for part in ref["path"].split("/"):
        path = path / part
        need(not path.is_symlink(), "symlinked public input")
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb") as stream:
        before = os.fstat(stream.fileno())
        need(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_size == ref["size"],
             "public input metadata")
        data = stream.read()
        held_after = os.fstat(stream.fileno())
    after = path.stat()
    need((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
         == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
         == (held_after.st_dev, held_after.st_ino, held_after.st_size, held_after.st_mtime_ns, held_after.st_ctime_ns)
         and hashlib.sha256(data).hexdigest() == ref["sha256"], "public input changed")
    return data


def public_config(value):
    """The public package contains JSON (also valid YAML), never wallet YAML.

    Component roles may reference only the seven fixed insecure development signers.
    Real operator-key fields, auth/cookies and non-loopback RPC URLs are rejected.
    """
    if type(value) is dict:
        for key, item in value.items():
            low = key.lower()
            if low.endswith("_sk") or low == "private_key":
                need(type(item) is str and item.removeprefix("0x") in PUBLIC_KEYS, "non-public test signer")
            if any(word in low for word in ("password", "cookie", "mnemonic", "secret")):
                need(item is None, "private configuration field")
            if low == "rpc_url":
                need(type(item) is str, "component RPC type")
                parsed = urllib.parse.urlsplit(item)
                need(parsed.scheme == "http" and parsed.hostname in {"127.0.0.1", "localhost"}
                     and parsed.port is not None and parsed.username is None and parsed.password is None
                     and not parsed.query and not parsed.fragment, "non-local component RPC")
            public_config(item)
    elif type(value) is list:
        for item in value:
            public_config(item)


def checked_gateway_views(direct_l1, gateway):
    """Two config views of one physical seed, never a second producer/chain."""
    for config, address in ((direct_l1, "127.0.0.1:3050"), (gateway, "127.0.0.1:3052")):
        need(type(config) is dict and type(config.get("genesis")) is dict
             and type(config["genesis"].get("chain_id")) is int
             and config["genesis"]["chain_id"] == 57001
             and type(config.get("l1_sender")) is dict
             and config["l1_sender"].get("pubdata_mode") == "Blobs"
             and "gateway_provider" not in config
             and config.get("rpc") == {"address": address}, "direct-L1 Gateway config view identity")
    need({key: value for key, value in direct_l1.items() if key != "rpc"}
         == {key: value for key, value in gateway.items() if key != "rpc"},
         "default and Gateway views must share the same physical seed/config")


def verify(root=FIXTURE, registration_path=REGISTRATION):
    # This source-controlled registration is independent of the package being checked.
    registration = json.loads(registration_path.read_bytes())
    need(type(registration) is dict and set(registration) == {"schema", "protocol_version", "descriptor_sha256"}
         and registration["schema"] == "syscoin-v32-anvil-component-registration-v1"
         and registration["protocol_version"] == "v32.0", "registration shape")
    need(digest(registration["descriptor_sha256"]), "genuine component fixture is not registered")
    marker = root / "CANONICAL_V8_REGENERATION_REQUIRED"
    need(not marker.exists() and not marker.is_symlink(), "component regeneration marker")
    descriptor_path = root / "anvil-component.json"
    need(not descriptor_path.is_symlink(), "descriptor symlink")
    raw = descriptor_path.read_bytes()
    need(len(raw) <= 1024 * 1024 and hashlib.sha256(raw).hexdigest() == registration["descriptor_sha256"],
         "trusted component descriptor")
    doc = checked_descriptor(json.loads(raw))
    # Check names before reading anything: never read/hash/export an unknown wallet file.
    allowed = FILES | {"anvil-component.json", "l1-state.json.zst", "l1-state.json", "l1-state.json.sha256"}
    for archive in doc["replay_archives"]:
        allowed.add("replay/" + str(archive["chain_id"]) + "/manifest.json")
        allowed.update(ref["path"] for ref in archive["objects"])
    actual = {str(path.relative_to(root)) for path in root.rglob("*") if path.is_file() or path.is_symlink()}
    need(actual <= allowed, "unexpected/private package file")
    config_views = {}
    for ref in [doc["compressed_state"], *doc["files"]]:
        data = read_public(root, ref)
        if ref["path"].endswith("/database_identity.json"):
            need(len(data) <= 16 * 1024, "bounded component database identity")
            checked_database_identity(json.loads(data), int(ref["path"].split("/")[1]))
        if ref["path"].endswith("config.yaml") or "/chain_" in ref["path"]:
            config = json.loads(data)
            public_config(config)
            config_views[ref["path"]] = config
    checked_gateway_views(config_views["default/config.yaml"], config_views["multi_chain/chain_57001.yaml"])
    if (root / "l1-state.json").exists():
        read_public(root, doc["decompressed_state"])
    for archive in doc["replay_archives"]:
        manifest = root / "replay" / str(archive["chain_id"]) / "manifest.json"
        need(manifest.is_file() and not manifest.is_symlink() and manifest.stat().st_size <= 1024 * 1024,
             "derived public replay manifest")
        raw = read_public(root, {"path": str(manifest.relative_to(root)), "size": manifest.stat().st_size,
                                "sha256": hashlib.sha256(manifest.read_bytes()).hexdigest()})
        need(json.loads(raw) == archive, "derived replay manifest differs from trusted descriptor")
        for ref in archive["objects"]:
            read_public(root, ref)
    return doc


if __name__ == "__main__":
    try:
        verify()
    except (ValueError, OSError, json.JSONDecodeError):
        raise SystemExit("V32/V8 AnvilComponentOnly package missing, unregistered or invalid; canonical fixture is not a fallback")
    print("verified V32/V8 AnvilComponentOnly public fixture (not canonical Syscoin release qualification)")
