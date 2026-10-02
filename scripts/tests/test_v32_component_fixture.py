"""Pure component-boundary tests; no Anvil, generation, node, Cargo or RPC."""
import copy
import gzip
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("component_fixture", ROOT / "scripts/fixtures/verify-v32-component-fixture.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def ref(path, sha="a" * 64):
    return {"path": path, "size": 1, "sha256": sha}


def replay(chain, head=1):
    return {"schema": "syscoin-v32-component-replay-archive-v1", "chain_id": chain,
            "anchor_block_number": head, "anchor_block_hash": "0x" + "b" * 64,
            "objects": [ref(f"replay/{chain}/{number}/" + ("b" if number == head else "a") * 64
                            + "/123-" + "c" * 32 + "-public-component") for number in range(head + 1)]}


def descriptor():
    # Metadata model only: these bytes are never a generated/registered fixture.
    return {
        "schema_version": 1, "scope": "AnvilComponentOnly", "protocol_version": "v32.0",
        "execution_version": 7, "proving_version": 8, "security_bits": 100,
        "verification_key_hash": gate.VK, "root_chain_id": 31337, "default_chain_id": 57001, "gateway_chain_id": 57001,
        "edge_chain_ids": [6565, 6566], "reverter_address": gate.REVERTER,
        "compressed_state": ref("l1-state.json.zst"), "decompressed_state": ref("l1-state.json"),
        "files": [ref(path, gate.GENESIS if path == "default/genesis.json" else "a" * 64)
                  for path in sorted(gate.FILES)],
        "replay_archives": [replay(chain) for chain in sorted(gate.REPLAY_CHAINS)],
    }


class ComponentBoundaryTests(unittest.TestCase):
    def test_only_exact_public_seed_database_identities_are_packaged(self):
        for chain in gate.REPLAY_CHAINS:
            marker = {"schema_version": 1, "protocol_version": "v32.0", "l1_chain_id": 31337,
                      "l1_genesis_block_hash": "0x" + "a" * 64, "l2_chain_id": chain,
                      "diamond_proxy_l1": "0x" + "b" * 40, "l2_genesis_block_hash": "0x" + "c" * 64}
            self.assertEqual(gate.checked_database_identity(marker, chain), marker)
            self.assertIn(f"replay/{chain}/database_identity.json", gate.FILES)
            for field, value in (("schema_version", True), ("protocol_version", "v31.0"),
                                 ("l1_chain_id", 1), ("l2_chain_id", 12345),
                                 ("l1_genesis_block_hash", "0x" + "0" * 64),
                                 ("l2_genesis_block_hash", "bad"), ("diamond_proxy_l1", "0x" + "0" * 40)):
                bad = dict(marker, **{field: value})
                with self.subTest(chain=chain, field=field), self.assertRaises(ValueError):
                    gate.checked_database_identity(bad, chain)
            with self.assertRaises(ValueError):
                gate.checked_database_identity(dict(marker, private_key="not-a-marker-field"), chain)

    def test_explicit_current_metadata_model(self):
        self.assertEqual(gate.checked_descriptor(descriptor())["scope"], "AnvilComponentOnly")
        self.assertEqual(gate.REPLAY_CHAINS, {57001, 6565, 6566})
        with self.assertRaises(ValueError):
            gate.checked_replay_manifest(replay(12345))

    def test_default_is_an_independent_config_view_of_one_gateway_seed(self):
        gateway = {"genesis": {"chain_id": 57001, "bridgehub_address": "0x" + "a" * 40},
                   "l1_sender": {"pubdata_mode": "Blobs"}, "rpc": {"address": "127.0.0.1:3052"}}
        direct_l1 = copy.deepcopy(gateway)
        direct_l1["rpc"]["address"] = "127.0.0.1:3050"
        gate.checked_gateway_views(direct_l1, gateway)
        for change in ("unsupported_chain", "bool_chain", "gateway_settlement", "second_seed", "same_port"):
            bad = copy.deepcopy(direct_l1)
            if change in {"unsupported_chain", "bool_chain"}:
                bad["genesis"]["chain_id"] = 12345 if change == "unsupported_chain" else True
            elif change == "gateway_settlement":
                bad["gateway_provider"] = {"rpc_url": "http://127.0.0.1:3052"}
            elif change == "second_seed":
                bad["genesis"]["bridgehub_address"] = "0x" + "b" * 40
            else:
                bad["rpc"]["address"] = "127.0.0.1:3052"
            with self.subTest(change=change), self.assertRaises(ValueError):
                gate.checked_gateway_views(bad, gateway)

    def test_state_capacity_is_finite_and_matches_the_rust_consumer(self):
        good = descriptor()
        good["compressed_state"]["size"] = 64 * 1024**2
        good["decompressed_state"]["size"] = 2 * 1024**3
        gate.checked_descriptor(good)
        for role in ("compressed_state", "decompressed_state"):
            bad = copy.deepcopy(good)
            bad[role]["size"] += 1
            with self.subTest(role=role), self.assertRaises(ValueError):
                gate.checked_descriptor(bad)
        rust = (ROOT / "integration-tests/src/fixture_component.rs").read_text()
        self.assertIn("descriptor.compressed_state.size > 64 * 1024 * 1024", rust)
        self.assertIn("descriptor.decompressed_state.size > 2 * 1024 * 1024 * 1024", rust)

    def test_wrong_protocol_guest_key_topology_and_bool_alias_rejected(self):
        for key, value in {
            "protocol_version": "v31.0", "execution_version": 5, "proving_version": 6,
            "security_bits": 80, "verification_key_hash": "0x" + "0" * 64,
            "gateway_chain_id": 506, "default_chain_id": 12345, "root_chain_id": 5700, "edge_chain_ids": [6565, True],
            "schema_version": True, "scope": "CanonicalSyscoin", "reverter_address": "unreviewed",
        }.items():
            bad = descriptor()
            bad[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                gate.checked_descriptor(bad)

    def test_missing_extra_and_duplicate_roles_rejected(self):
        for change in ("missing", "extra", "duplicate"):
            bad = descriptor()
            if change == "missing":
                bad["files"].pop()
            elif change == "extra":
                bad["files"].append(ref("wallets.yaml"))
            else:
                bad["files"][-1] = copy.deepcopy(bad["files"][0])
            with self.subTest(change=change), self.assertRaises(ValueError):
                gate.checked_descriptor(bad)

    def test_changed_genesis_and_unknown_field_rejected(self):
        bad = descriptor()
        next(item for item in bad["files"] if item["path"] == "default/genesis.json")["sha256"] = "b" * 64
        with self.assertRaises(ValueError):
            gate.checked_descriptor(bad)
        bad = descriptor()
        bad["canonical_qualified"] = True
        with self.assertRaises(ValueError):
            gate.checked_descriptor(bad)

    def test_unsafe_identity_rejected(self):
        for path in ("../wallet", "/wallet", "x//y", "./x", "a/../b", "x\\y"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                gate.identity(ref(path))
        for size in (True, 0, -1):
            bad = ref("public.json")
            bad["size"] = size
            with self.assertRaises(ValueError):
                gate.identity(bad)

    def test_public_replay_contiguous_anchor_and_closed_topology(self):
        for chain in gate.REPLAY_CHAINS:
            for head in (0, 1, 3):
                self.assertEqual(gate.checked_replay_manifest(replay(chain, head))["anchor_block_number"], head)
        for field, wrong in (("chain_id", True), ("chain_id", 31337), ("anchor_block_number", True),
                             ("anchor_block_number", -1), ("anchor_block_number", 4096),
                             ("anchor_block_hash", "0x" + "0" * 64), ("schema", "private-db-v1")):
            bad = replay(57001)
            bad[field] = wrong
            with self.subTest(field=field, wrong=wrong), self.assertRaises(ValueError):
                gate.checked_replay_manifest(bad)
        for change in ("gap", "order", "anchor", "session", "leading_zero", "size", "unknown"):
            bad = replay(57001)
            if change == "gap":
                bad["objects"].pop(0)
            elif change == "order":
                bad["objects"].reverse()
            elif change == "anchor":
                bad["anchor_block_hash"] = "0x" + "e" * 64
            elif change in {"session", "leading_zero"}:
                bad["objects"][0]["path"] = bad["objects"][0]["path"].rsplit("/", 1)[0] + (
                    "/123-" + "c" * 32 + "-.." if change == "session" else "/0123-" + "c" * 32 + "-public")
            elif change == "size":
                bad["objects"][0]["size"] = 256 * 1024**2 + 1
            else:
                bad["database_copy"] = True
            with self.subTest(change=change), self.assertRaises(ValueError):
                gate.checked_replay_manifest(bad)
        for change in ("missing", "duplicate", "aggregate"):
            bad = descriptor()
            if change == "missing":
                bad["replay_archives"].pop()
            elif change == "duplicate":
                bad["replay_archives"][-1] = copy.deepcopy(bad["replay_archives"][0])
            else:
                bad["replay_archives"] = [replay(chain, 1365) for chain in sorted(gate.REPLAY_CHAINS)]
            with self.subTest(change=change), self.assertRaises(ValueError):
                gate.checked_descriptor(bad)

    def test_held_public_identity_and_symlink_tamper(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            path = root / "public.json"
            path.write_bytes(b"unit-only")
            good = {"path": path.name, "size": 9, "sha256": hashlib.sha256(b"unit-only").hexdigest()}
            self.assertEqual(gate.read_public(root, good), b"unit-only")
            path.write_bytes(b"unit-wrong")
            with self.assertRaises(ValueError):
                gate.read_public(root, good)
            (root / "link").symlink_to(path)
            with self.assertRaises(ValueError):
                gate.read_public(root, {**good, "path": "link"})

    def test_unissued_registration_is_not_authority(self):
        registration = json.loads(gate.REGISTRATION.read_bytes())
        registration["descriptor_sha256"] = None
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            registry = root / "unit-only-registration.json"
            registry.write_text(json.dumps(registration))
            with self.assertRaises(ValueError):
                gate.verify(root / "nonexistent-fixture", registry)

    def test_only_fixed_public_signer_and_local_rpc(self):
        gate.public_config({"l1_sender": {"operator_prove_sk": gate.PUBLIC_KEY},
                            "l1_provider": {"rpc_url": "http://127.0.0.1:8545"}})
        for value in ({"operator_prove_sk": "not-a-public-test-key"}, {"password": "unit-only"},
                      {"cookie_file": "unit-only"}, {"rpc_url": "https://remote.invalid"}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                gate.public_config(value)
        with self.assertRaises(ValueError):
            gate.public_config({"rpc_url": "http://localhost:8545@remote.invalid"})

    def test_canonical_loader_marker_and_prover_default_remain_separate(self):
        backend = (ROOT / "integration-tests/src/fixture_backend.rs").read_text()
        self.assertIn('if cfg!(feature = "prover-tests")', backend)
        self.assertIn('V32 canonical fixture cannot use the component-only Anvil backend', backend)
        self.assertIn('check_regeneration_marker(root)?;', backend.split('pub fn load_fixture_inventory(', 1)[1])
        self.assertIn('base.join("anvil-component-only").join(version)', backend)
        self.assertTrue((ROOT / "local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED").is_file())
        self.assertFalse((ROOT / "local-chains/v32.0/versions.yaml").exists())

    def test_all_current_path_consumers_share_explicit_scope(self):
        config = (ROOT / "integration-tests/src/config.rs").read_text()
        wallets = (ROOT / "integration-tests/src/wallets.rs").read_text()
        self.assertIn('self.protocol_dir().join("default").join("genesis.json")', config)
        self.assertIn('FixtureScope::AnvilComponentOnly => fixture_backend::component::load(', config)
        self.assertIn('layout.backend_inventory()', wallets)
        self.assertIn('PUBLIC_ANVIL_REVERTER_KEY', wallets)
        self.assertNotIn('.join(layout.protocol_version())', wallets)
        build = (ROOT / "integration-tests/build.rs").read_text()
        self.assertIn('local_chains.join("anvil-component-only/v32.0")', build)

    def test_ci_selection_and_config_startup_rpc_transaction_preserved(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        smoke = (ROOT / ".github/scripts/test-configs.sh").read_text()
        self.assertIn('nextest run --locked --release --workspace', workflow)
        self.assertIn('test-configs.sh --anvil-component-only', workflow)
        for token in ('anvil --load-state', './zksync-os-server --config', 'nc -z', 'cast send'):
            self.assertIn(token, smoke)
        selected_branch = smoke.split('if [ "${COMPONENT_ONLY}" = true ]; then', 1)[1].split('\nfi', 1)[0]
        component_branch, normal_branch = selected_branch.split('\nelse\n', 1)
        self.assertIn('chmod a+x ./anvil-component-bootstrap', component_branch)
        self.assertNotIn('chmod a+x ./zksync-os-server', component_branch)
        self.assertIn('chmod a+x ./zksync-os-server', normal_branch)
        self.assertNotIn('chmod a+x ./zksync-os-server', smoke.split('COMPONENT_ONLY=false', 1)[0])
        runtime = (ROOT / "integration-tests/src/lib.rs").read_text()
        database_assignment = runtime.split('fn bind_runtime_config(', 1)[1].split(';', 1)[0]
        self.assertIn('.canonicalize()', database_assignment)
        self.assertIn('.join("rocksdb")', database_assignment)
        self.assertEqual(smoke.count('python3 scripts/fixtures/verify-v32-component-fixture.py'), 2)
        prover = (ROOT / "integration-tests/tests/prover.rs").read_text()
        self.assertIn('#![cfg(feature = "prover-tests")]', prover)
        self.assertIn('fake_fri_provers.enabled = false', prover)
        self.assertIn('fake_snark_provers.enabled = false', prover)

    def test_config_cleanup_waits_for_nodes_before_removing_database(self):
        smoke = (ROOT / ".github/scripts/test-configs.sh").read_text()
        cleanup = smoke.split('cleanup() {\n', 1)[1].split('\n}\n', 1)[0]
        remove = cleanup.index('rm -rf ./db')
        self.assertLess(cleanup.index('wait "${SERVER_PID}"'), remove)
        self.assertLess(cleanup.index('wait "${ANVIL_PID}"'), remove)
        self.assertEqual(cleanup.count('rm -rf ./db'), 1)

    def test_component_smoke_uses_zstd_and_legacy_smoke_keeps_gzip(self):
        smoke = (ROOT / ".github/scripts/test-configs.sh").read_text()
        decode = smoke.split('  echo "Decompressing anvil state..."\n', 1)[1].split('  ANVIL_CLOCK_ARGS=()', 1)[0]
        component, legacy = decode.split('\n  else\n', 1)
        self.assertIn('zstd --long=27 -dfk "${CUR_STATE}.zst"', component)
        self.assertNotIn('gzip', component)
        self.assertIn('gzip -dfk "${CUR_STATE}.gz"', legacy)
        self.assertIn('wget jq zstd libssl-dev', (ROOT / ".github/actions/runner-setup/action.yaml").read_text())
        with tempfile.TemporaryDirectory() as tmp:
            state = pathlib.Path(tmp) / "l1-state.json"
            raw = b'{"block":{"timestamp":"1"},"historical_states":[]}\n'
            compressed = subprocess.run(["zstd", "--long=27", "-q", "-c"], input=raw,
                                        capture_output=True, check=True).stdout
            state.with_suffix(".json.zst").write_bytes(compressed)
            command = 'CUR_STATE="$1"; COMPONENT_ONLY="$2"; ' + decode
            subprocess.run(["bash", "-e", "-c", command, "codec-test", str(state), "true"],
                           capture_output=True, check=True)
            self.assertEqual(state.read_bytes(), raw)
            state.unlink()
            state.with_suffix(".json.gz").write_bytes(gzip.compress(raw))
            subprocess.run(["bash", "-e", "-c", command, "codec-test", str(state), "false"],
                           capture_output=True, check=True)
            self.assertEqual(state.read_bytes(), raw)

    def test_component_smoke_clock_uses_only_authenticated_state_timestamp(self):
        smoke = (ROOT / ".github/scripts/test-configs.sh").read_text()
        clock = smoke.split('L1_TIMESTAMP=$(', 1)[1].split('\n    )', 1)[0]
        branch = smoke.split('  ANVIL_CLOCK_ARGS=()\n', 1)[1].split('\n  fi\n', 1)[0]
        self.assertIn('if [ "${COMPONENT_ONLY}" = true ]; then', branch)
        self.assertLess(branch.index('verify-v32-component-fixture.py'), branch.index('L1_TIMESTAMP='))
        self.assertIn('ANVIL_CLOCK_ARGS=(--timestamp "${L1_TIMESTAMP}")', branch)
        self.assertIn('anvil --load-state "${CUR_STATE}" "${ANVIL_CLOCK_ARGS[@]}"', smoke)
        self.assertIn('jq --stream -c', clock)
        self.assertIn('select(length == 2 and .[0] == ["block", "timestamp"]) | .[1]', clock)
        self.assertNotIn('json.load(state)', clock)
        self.assertNotIn('open(', clock)
        with tempfile.TemporaryDirectory() as tmp:
            state = pathlib.Path(tmp) / "state.json"
            for value, expected in (("0x6ac0fc00", "1791032320"), ("1791032320", "1791032320"),
                                    ("0xffffffffffffffff", str(2**64 - 1)), ("0", "0")):
                state.write_text(json.dumps({"block": {"timestamp": value},
                                             "historical_states": [{"unrelated": "x" * 1000000}]}))
                result = subprocess.run(["bash", "-o", "pipefail", "-c", 'CUR_STATE="$1"; ' + clock,
                                         "clock-test", str(state)],
                                        capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.strip(), expected)
            for value in (True, 1791032320, None, {}, [], "-1", str(2**64), "invalid", "+1", " 1", "1\n"):
                state.write_text(json.dumps({"block": {"timestamp": value}}))
                result = subprocess.run(["bash", "-o", "pipefail", "-c", 'CUR_STATE="$1"; ' + clock,
                                         "clock-test", str(state)],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
            for malformed in ('{"block": {}}', '{"block":{"timestamp":"1","timestamp":"2"}}',
                              '{"block":{"timestamp":"1"},"malformed":'):
                state.write_text(malformed)
                result = subprocess.run(["bash", "-o", "pipefail", "-c", 'CUR_STATE="$1"; ' + clock,
                                         "clock-test", str(state)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)

    def test_registered_bootstrap_clock_is_source_authenticated_and_component_only(self):
        source = (ROOT / "integration-tests/src/test_config.rs").read_text()
        bootstrap = source.split('pub async fn run_component_fixture_bootstrap(', 1)[1]
        branch = bootstrap.split('if let Some(root) = registered_root {', 1)[1].split('\n    let runtime =', 1)[0]
        for token in ('component::registered_hash("v32.0")', 'component::load(&root, "v32.0", &hash)',
                      '.anvil_state(&root)', 'state_identity.verify(&root)', '.verify_bytes(&state_bytes)',
                      'crate::l1_state_timestamp(&state_bytes)?', 'drop(state_bytes);',
                      'i64::try_from(timestamp)?.saturating_sub(i64::try_from(now())?)'):
            self.assertIn(token, branch)
        self.assertLess(branch.index('.verify_bytes(&state_bytes)'), branch.index('crate::l1_state_timestamp'))
        self.assertNotIn('block_timestamp_offset_seconds', bootstrap.split('if let Some(root) = registered_root {', 1)[0])
        self.assertIn('.validate()', bootstrap)
        self.assertIn('zksync_os_server::run(&runtime, config)', bootstrap)


if __name__ == "__main__":
    unittest.main()
