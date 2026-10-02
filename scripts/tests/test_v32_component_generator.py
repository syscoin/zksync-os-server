"""Source / pure-policy tests only; never build, deploy, start a node or use RPC."""
import ast
import copy
import hashlib
import importlib.util
import pathlib
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
PATH = ROOT / "scripts/fixtures/generate-v32-component-fixture.py"
spec = importlib.util.spec_from_file_location("component_generator", PATH)
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


class GeneratorBoundaryTests(unittest.TestCase):
    def test_accepted_remappings_are_complete_without_metadata_or_address_waiver(self):
        import json
        self.assertEqual(len(subject.ACCEPTED_REMAPPINGS), 16)
        self.assertEqual(hashlib.sha256(json.dumps(subject.ACCEPTED_REMAPPINGS,
                            separators=(",", ":")).encode()).hexdigest(),
                         "c5eacf3e5b958a22571a6a1f38fdd4381e13e14be0b8a8bb822b7c4e3210d246")
        plan = {"working_directory": "/unit-component", "ports": {"anvil": 19991, "gateway": 19992},
                "tools": {name: {"path": "/unit-tools/" + name} for name in subject.TOOL_NAMES}}
        actual = subject.pwd.getpwuid(subject.os.getuid()).pw_dir
        with patch.dict(subject.os.environ, {"HOME": actual}), \
                patch.object(pathlib.Path, "exists", return_value=False), \
                patch.object(pathlib.Path, "is_symlink", return_value=False):
            env = subject.Generation(plan).env
        self.assertEqual(env["FOUNDRY_AUTO_DETECT_REMAPPINGS"], "false")
        self.assertEqual(env["FOUNDRY_REMAPPINGS"].splitlines(), list(subject.ACCEPTED_REMAPPINGS))
        self.assertNotIn("FOUNDRY_CBOR_METADATA", env)
        self.assertNotIn("FOUNDRY_BYTECODE_HASH", env)
        self.assertEqual(subject.GOVERNANCE, "0x7d631ab177342c99d3a925703d985d2de3fbfdf8")

    def test_cli_hex_scalars_remain_strings_without_global_yaml_mutation(self):
        import yaml
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / "contracts.yaml"
            path.write_text("governance_addr: " + subject.GOVERNANCE + "\n"
                            "zero_address: 0x" + "0" * 40 + "\n"
                            "hash: 0x" + "0" * 63 + "1\n"
                            "chain_id: 12345\nenabled: true\nabsent: null\n")
            parsed = subject.read_yaml(path)
            self.assertEqual(parsed["governance_addr"], subject.GOVERNANCE)
            self.assertEqual(parsed["zero_address"], "0x" + "0" * 40)
            self.assertEqual(parsed["hash"], "0x" + "0" * 63 + "1")
            self.assertEqual(parsed["chain_id"], 12345)
            self.assertIs(parsed["enabled"], True)
            self.assertIsNone(parsed["absent"])
            self.assertEqual(yaml.safe_load(path.read_bytes())["governance_addr"],
                             int(subject.GOVERNANCE, 16))

    def test_eravm_compilers_are_separate_from_fixed_l1_compiler(self):
        import tomllib
        plan = {"working_directory": "/unit-component", "ports": {"anvil": 19991, "gateway": 19992},
                "tools": {name: {"path": "/unit-tools/" + name} for name in subject.TOOL_NAMES}}
        actual = subject.pwd.getpwuid(subject.os.getuid()).pw_dir
        with patch.dict(subject.os.environ, {"HOME": actual}), \
                patch.object(pathlib.Path, "exists", return_value=False), \
                patch.object(pathlib.Path, "is_symlink", return_value=False):
            env = subject.Generation(plan).env
        self.assertEqual(env["FOUNDRY_SOLC"], "/unit-tools/solc")
        self.assertEqual(env["RAYON_NUM_THREADS"], "2")
        self.assertEqual(tomllib.loads("zksync = " + env["FOUNDRY_ZKSYNC"])["zksync"],
                         {"solc_path": "/unit-tools/era_solc", "zksolc": "/unit-tools/zksolc"})
        self.assertNotIn("FOUNDRY_ZKSYNC_SOLC_PATH", env)
        self.assertEqual(subject.ERA_SOLC_SHA256, "cebf505bd1874f7ce3a6e7de3545f7f7fd52b4ba7bccb54f710c7daf7ae42edd")
        self.assertEqual(subject.ZKSOLC_SHA256, "aac04e482baf59c33d4cb758bb9f0d2c33dd88ba2ea32dfeae402109dc4ba6d4")
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertNotIn("'default', '--no-genesis'", execute)
        self.assertIn("'gateway', '--no-genesis', '--deploy-paymaster', 'false', '--l1-rpc-url'", execute)

    def test_required_l1_eravm_artifacts_use_original_build_before_deployment(self):
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertIn("'build', '--zksync', '--skip', '*/l1-contracts/test/*', 'L2BaseToken.sol', 'Burner.sol', '--threads', '2'", execute)
        self.assertLess(execute.index("'--zksync'"), execute.index("self.start("))
        self.assertNotIn("'--out'", execute.split("self.start(", 1)[0])

    def test_script_output_is_private_create_only_and_precedes_deployment(self):
        with tempfile.TemporaryDirectory() as temp:
            era = pathlib.Path(temp).resolve()
            parent = era / "contracts/l1-contracts"
            parent.mkdir(parents=True, mode=0o700)
            model = object.__new__(subject.Generation)
            model.plan = {"era_root": str(era)}
            model.prepare_script_output()
            self.assertEqual((parent / "script-out").stat().st_mode & 0o777, 0o700)
            with self.assertRaisesRegex(ValueError, "new script output"):
                model.prepare_script_output()
            (parent / "script-out").rmdir()
            (parent / "script-out").symlink_to(parent / "absent")
            with self.assertRaisesRegex(ValueError, "new script output"):
                model.prepare_script_output()
            (parent / "script-out").unlink()
            parent.chmod(0o755)
            with self.assertRaisesRegex(ValueError, "private generation contract root"):
                model.prepare_script_output()
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertLess(execute.index("self.prepare_script_output()"), execute.index("self.start("))

    def test_fixed_guest_addresses_are_twenty_bytes(self):
        for value in (subject.GOVERNOR, subject.GOVERNANCE, subject.FACTORY, subject.TIMELOCK,
                      subject.RELAY, subject.COMMIT_ADDRESS, subject.EXECUTE_ADDRESS, subject.gate.REVERTER):
            self.assertEqual(len(bytes.fromhex(value[2:])), 20)
        expected = (ROOT / "scripts/gateway-launch/_common.sh").read_text()
        self.assertIn(subject.TIMELOCK, expected.lower())

    def test_published_signers_are_distinct_and_governance_keyless(self):
        ecosystem = subject.public_wallets(True)
        chain = subject.public_wallets(False)
        self.assertEqual(ecosystem["governor"], {"address": subject.GOVERNOR, "private_key": None})
        self.assertEqual(chain["governor"]["address"], subject.MANAGEMENT_ADDRESS)
        self.assertEqual(chain["deployer"]["address"], subject.MANAGEMENT_ADDRESS)
        roles = [chain[name] for name in ("operator", "prove_operator", "execute_operator")]
        self.assertEqual(len({role["address"] for role in roles}), 3)
        self.assertEqual({role["private_key"][2:] for role in roles},
                         {subject.gate.PUBLIC_COMMIT_KEY, subject.gate.PUBLIC_ROOT_PROVE_KEY,
                          subject.gate.PUBLIC_EXECUTE_KEY})

    def test_nonce_owners_are_distinct_per_actual_settlement_endpoint(self):
        root = subject.operator_roles("gateway")
        edge1, edge2 = (subject.operator_roles(name) for name in ("edge6565", "edge6566"))
        self.assertEqual(root[1], (subject.ROOT_PROVE_ADDRESS, "0" * 63 + "7"))
        self.assertNotIn(subject.gate.REVERTER, {address for address, _ in root})
        self.assertEqual(edge1[1], (subject.gate.REVERTER, subject.gate.PUBLIC_KEY))
        self.assertEqual({key for _, key in edge2}, {"0" * 63 + str(i) for i in (4, 5, 6)})
        self.assertTrue({address for address, _ in edge1}.isdisjoint(address for address, _ in edge2))
        self.assertNotIn(subject.MANAGEMENT_ADDRESS, {address for address, _ in (*root, *edge1, *edge2)})
        contracts = {"ecosystem_contracts": {"bridgehub_proxy_addr": "0x" + "1" * 40,
                      "l1_bytecodes_supplier_addr": "0x" + "2" * 40}}
        for name in subject.CHAIN_IDS:
            wallet = subject.public_wallets(False, name)
            cfg = subject.server_config(name, contracts, "http://127.0.0.1:8545",
                                        "http://127.0.0.1:3052" if name.startswith("edge") else None,
                                        3050, pathlib.Path("/unit-genesis"), pathlib.Path("/unit-work"))
            sender = cfg["gateway_sender"] if name.startswith("edge") else cfg["l1_sender"]
            for role, wallet_key in (("commit", "blob_operator"), ("prove", "prove_operator"), ("execute", "execute_operator")):
                self.assertEqual(sender["operator_" + role + "_sk"], wallet[wallet_key]["private_key"][2:])
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertLess(execute.index("for key in (gate.PUBLIC_KEY, MANAGEMENT_KEY)"),
                        execute.index("configs = {}"))
        self.assertNotIn("'--private-key', '0x' + gate.PUBLIC_KEY", execute.split("configs = {}", 1)[1])

    def test_command_dependency_failure_cooperatively_drains_own_group(self):
        from unittest.mock import Mock
        import time
        with tempfile.TemporaryDirectory() as temp:
            model = object.__new__(subject.Generation)
            model.work = pathlib.Path(temp)
            model.ecosystem = model.work
            model.env = {}
            model.plan = {"deadline_unix": time.time() + 600}
            model.commands, model.command_children = [], []
            dependency = Mock(pid=12)
            dependency.poll.side_effect = [None, 1]
            model.children = [dependency]
            command = Mock(pid=23, returncode=-15)
            command.poll.return_value = None
            with patch.object(subject.subprocess, "Popen", return_value=command), \
                    patch.object(subject.os, "getpgid", return_value=23), \
                    patch.object(subject.os, "killpg") as terminate:
                with self.assertRaisesRegex(ValueError, "dependency stopped: pid=12"):
                    model.run(["/unit-tools/zkstack"])
                terminate.assert_called_once_with(23, subject.signal.SIGTERM)
                command.wait.assert_called_once_with(timeout=60)
        main = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                if isinstance(node, ast.FunctionDef) and node.name == "main"))
        self.assertIn("except BaseException:", main)
        self.assertIn("'owned_cleanup_failed': True", main)

    def test_real_contract_schema_and_no_production_validation_waiver(self):
        contracts = {"ecosystem_contracts": {"bridgehub_proxy_addr": "0x" + "1" * 40,
                      "l1_bytecodes_supplier_addr": "0x" + "2" * 40}}
        cfg = subject.server_config("gateway", contracts, "http://127.0.0.1:8545", None,
                                    3052, pathlib.Path("/unit-genesis"), pathlib.Path("/unit-work"))
        self.assertEqual(cfg["genesis"]["bytecode_supplier_address"], "0x" + "2" * 40)
        self.assertEqual(len({cfg["l1_sender"][name] for name in (
            "operator_commit_sk", "operator_prove_sk", "operator_execute_sk")}), 3)
        rust = (ROOT / "integration-tests/src/test_config.rs").read_text().split("pub async fn run_component_fixture_bootstrap", 1)[1]
        compact = "".join(rust.split())
        self.assertLess(compact.index("config.validate().await"), compact.index("zksync_os_server::run"))
        self.assertIn("maybe_start_bitcoin_da_mock", rust)
        self.assertIn("component critical task failed", rust)

    def test_non_public_signer_and_remote_urls_rejected(self):
        for key in subject.gate.PUBLIC_KEYS:
            subject.gate.public_config({"operator_commit_sk": key})
        for value in ({"operator_commit_sk": "3" * 64}, {"private_key": "unknown"},
                      {"rpc_url": "http://127.0.0.1:8545@remote.invalid"},
                      {"rpc_url": "https://127.0.0.1:8545"}, {"cookie": "unit-only"}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                subject.gate.public_config(value)

    def test_plan_scope_and_deadline_reject_before_artifact_reads(self):
        doc = {name: None for name in subject.PLAN_KEYS}
        doc.update(schema="syscoin-v32-anvil-component-generation-v1", scope="AnvilComponentOnly",
                   execute=False, server_revision=subject.SERVER_HEAD, deadline_unix=1200)
        for field, wrong in (("scope", "CanonicalSyscoin"), ("execute", 1),
                             ("server_revision", "8886"), ("deadline_unix", True),
                             ("deadline_unix", 1000), ("deadline_unix", 4601)):
            bad = copy.deepcopy(doc)
            bad[field] = wrong
            with self.subTest(field=field, wrong=wrong), patch.object(subject, "held") as reader:
                with self.assertRaises(ValueError):
                    subject.checked_plan(bad, 1000)
                reader.assert_not_called()

    def test_exclusive_owned_outputs_and_duplicate_ports(self):
        with tempfile.TemporaryDirectory() as temp:
            parent = pathlib.Path(temp).resolve()
            parent.chmod(0o700)
            doc = {name: None for name in subject.PLAN_KEYS}
            doc.update(schema="syscoin-v32-anvil-component-generation-v1", scope="AnvilComponentOnly",
                       execute=False, server_revision=subject.SERVER_HEAD, deadline_unix=1200,
                       working_directory=str(parent / "work"), package_directory=str(parent / "package"),
                       era_root=str(parent), ports={name: 8545 for name in subject.PORT_NAMES})
            with self.assertRaises(ValueError):
                subject.checked_plan(doc, 1000)
            (parent / "work").mkdir()
            with self.assertRaises(ValueError):
                subject.checked_plan(doc, 1000)

    def test_held_input_identity_symlink_and_hardlink_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp).resolve() / "public"
            path.write_bytes(b"public-model")
            ref = {"path": str(path), "size": 12, "sha256": hashlib.sha256(b"public-model").hexdigest()}
            self.assertEqual(subject.held(ref), b"public-model")
            link = path.with_name("redirect")
            link.symlink_to(path)
            with self.assertRaises(ValueError):
                subject.held({**ref, "path": str(link)})
            link.unlink()
            link.hardlink_to(path)
            with self.assertRaises(ValueError):
                subject.held(ref)

    def test_no_forced_timeout_or_home_replacement(self):
        tree = ast.parse(PATH.read_text())
        attributes = {node.attr for node in ast.walk(tree) if isinstance(node, ast.Attribute)}
        self.assertNotIn("kill", attributes)
        self.assertNotIn("run", {node.attr for node in ast.walk(tree)
                                if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name)
                                and node.value.id == "subprocess"})
        constructor = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)
                           and node.name == "__init__")
        text = ast.unparse(constructor)
        self.assertIn("pwd.getpwuid(os.getuid()).pw_dir", text)
        self.assertIn("os.environ.get('HOME') == component_user_home", text)
        self.assertIn("'HOME': component_user_home", text)
        self.assertIn("SIGTERM", attributes)
        self.assertIn("force_kill", PATH.read_text())

    def test_generation_preserves_only_actual_login_home(self):
        plan = {"working_directory": "/unit-component", "ports": {"anvil": 19991, "gateway": 19992},
                "tools": {name: {"path": "/unit-tools/" + name} for name in subject.TOOL_NAMES}}
        actual = subject.pwd.getpwuid(subject.os.getuid()).pw_dir
        with patch.dict(subject.os.environ, {"HOME": actual}):
            self.assertEqual(subject.Generation(plan).env["HOME"], actual)
        with patch.dict(subject.os.environ, {"HOME": "/unit-repointed-home"}), self.assertRaises(ValueError):
            subject.Generation(plan)

    def test_qualified_solc_and_absent_global_foundry_config(self):
        plan = {"working_directory": "/unit-component", "ports": {"anvil": 19991, "gateway": 19992},
                "tools": {name: {"path": "/unit-tools/" + name} for name in subject.TOOL_NAMES}}
        actual = subject.pwd.getpwuid(subject.os.getuid()).pw_dir
        with patch.dict(subject.os.environ, {"HOME": actual}), \
                patch.object(pathlib.Path, "exists", return_value=False), \
                patch.object(pathlib.Path, "is_symlink", return_value=False):
            self.assertEqual(subject.Generation(plan).env["FOUNDRY_SOLC"], "/unit-tools/solc")
        for exists, symlink in ((True, False), (True, True), (False, True)):
            with self.subTest(exists=exists, symlink=symlink), \
                    patch.dict(subject.os.environ, {"HOME": actual}), \
                    patch.object(pathlib.Path, "exists", return_value=exists), \
                    patch.object(pathlib.Path, "is_symlink", return_value=symlink), \
                    self.assertRaisesRegex(ValueError, "global Foundry config"):
                subject.Generation(plan)
        self.assertEqual(subject.SOLC_SHA256, "9a0fb7e0db2c0641dbae1c5cc645dc686820c83af516226abb1c0a2f76636f25")
        import json
        template = json.loads((ROOT / "scripts/fixtures/v32-component-generation.plan.inert.json").read_text())
        self.assertEqual(set(template["tools"]), subject.TOOL_NAMES)
        self.assertFalse(template["execute"])
        self.assertTrue(all(ref is None for ref in template["tools"].values()))

    def test_system_tool_trust_does_not_relax_source_ownership(self):
        system = pathlib.Path("/usr/bin/true").resolve(strict=True)
        data = system.read_bytes()
        ref = {"path": str(system), "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        self.assertEqual(subject.held(ref, tool=True), data)
        if system.stat().st_uid != subject.os.getuid():
            with self.assertRaises(ValueError):
                subject.held(ref)
        self.assertIn('before.st_uid in {0, os.getuid()} if tool', PATH.read_text())
        self.assertIn('not info.st_mode & 0o022', PATH.read_text())
        self.assertIn('before.st_nlink == 1', PATH.read_text())

    def test_command_failure_diagnostic_is_safe_and_specific(self):
        text = PATH.read_text()
        self.assertIn('" tool=" + pathlib.Path(argv[0]).name', text)
        self.assertIn('" exit_code=" + str(child.returncode)', text)
        self.assertIn('"current tool command failed: command_index=" + str(index)', text)
        self.assertNotIn('raise SystemExit("component generation failed closed', text)

    def test_only_explicit_component_da_code_install_no_storage_fork_or_private_fixture(self):
        tree = ast.parse(PATH.read_text())
        constants = {node.value for node in ast.walk(tree)
                     if isinstance(node, ast.Constant) and isinstance(node.value, str)}
        for forbidden in ("anvil_setStorageAt", "anvil_loadState", "--fork-url", "wallets.zip"):
            self.assertNotIn(forbidden, constants)
        self.assertIn("anvil_setBalance", constants)  # Public development funding only.
        installs = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Attribute) and node.func.attr == "rpc"
                    and node.args and isinstance(node.args[0], ast.Constant)
                    and node.args[0].value == "anvil_setCode"]
        self.assertEqual(len(installs), 1)
        self.assertEqual(ast.unparse(installs[0].args[1]), "[COMPONENT_DA_ADDRESS, runtime]")
        self.assertEqual(subject.COMPONENT_DA_ADDRESS, "0x" + "0" * 38 + "63")
        source = (ROOT / "scripts/fixtures/ComponentBitcoinDAAvailabilityMock.sol").read_text()
        self.assertIn("require(msg.data.length == 32)", source)
        self.assertIn("return(0, 32)", source)
        self.assertIn("address(0x63).call{gas: 1400}(input)", source)
        self.assertNotIn("sload", source.lower())
        installer = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)
                         and node.name == "install_component_da_mock")
        install_text = ast.unparse(installer)
        for predicate in ("'AnvilComponentOnly'", "== 31337", "< 8", "== '0x'",
                          "'source_attestation'", "'actual gas probe receipt join'"):
            self.assertIn(predicate, install_text)
        for node in ast.walk(installer):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "run":
                self.assertEqual(ast.unparse(node.args[1]), "self.work")
        execute = ast.unparse(next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)
                                  and node.name == "execute"))
        self.assertLess(execute.index("self.install_component_da_mock()"), execute.index("'ecosystem', 'create'"))
        self.assertIn("'actual_restored_gas_controls'] = self.component_da_controls()", execute)

    def test_component_da_actual_gas_controls_keep_two_positive_three_negative_inputs(self):
        model = object.__new__(subject.Generation)  # Pure command model, no process/RPC.
        model.tools, model.url, model.work = {"cast": "/unit/cast"}, "http://127.0.0.1:19991", pathlib.Path("/unit/work")
        model.component_da_probe = "0x" + "a" * 40
        commands = []
        model.run = lambda argv, cwd: commands.append((argv, cwd)) or "true"
        controls = model.component_da_controls()
        self.assertEqual([(row["input_size"], row["expected_success"]) for row in controls],
                         [(32, True), (32, True), (0, False), (31, False), (33, False)])
        self.assertTrue(all(cwd == model.work for _, cwd in commands))
        self.assertTrue(all("check(bytes,bool)(bool)" in argv for argv, _ in commands))
        model.run = lambda argv, cwd: "false"
        with self.assertRaisesRegex(ValueError, "1400-gas component DA control"):
            model.component_da_controls()

    def test_prague_env_is_scoped_to_relay_and_exact_compiled_identity_checked(self):
        import json
        with tempfile.TemporaryDirectory() as temporary:
            relay = pathlib.Path(temporary)
            artifact = relay / "SyscoinRelayedSLDAValidator.sol/SyscoinRelayedSLDAValidator.json"
            artifact.parent.mkdir()
            compiled = {"metadata": {"compiler": {"version": "0.8.28+commit.7893614a"},
                "settings": {"evmVersion": "prague", "remappings": list(subject.ACCEPTED_REMAPPINGS)}},
                "bytecode": {"object": "0x11"}, "deployedBytecode": {"object": "0x22"}}
            artifact.write_text(json.dumps(compiled))
            model = object.__new__(subject.Generation)  # Pure output/command model, no compiler/RPC.
            model.env = {"FOUNDRY_EVM_VERSION": "cancun", "FOUNDRY_SOLC": "/unit/solc"}
            model.tools = {"forge": "/unit/forge", "cast": "/unit/cast"}
            model.plan, model.work = {"era_root": "/unit/era"}, pathlib.Path("/unit/work")
            seen = []
            def run(argv, cwd):
                seen.append((argv, model.env["FOUNDRY_EVM_VERSION"]))
                return "" if argv[0] == model.tools["forge"] else {"0x11": subject.RELAY_INIT_HASH, "0x22": subject.RELAY_HASH}[argv[-1]]
            model.run = run
            self.assertEqual(model.build_component_relay(relay), artifact)
            self.assertEqual(seen[0][1], "prague")
            self.assertTrue(all(value == "cancun" for _, value in seen[1:]))
            self.assertEqual(model.env, {"FOUNDRY_EVM_VERSION": "cancun", "FOUNDRY_SOLC": "/unit/solc"})
            compiled["metadata"]["settings"]["evmVersion"] = "cancun"
            artifact.write_text(json.dumps(compiled))
            with self.assertRaisesRegex(ValueError, "actual relay compiler/Prague"):
                model.build_component_relay(relay)
            model.run = lambda argv, cwd: (_ for _ in ()).throw(ValueError("compile model failed"))
            with self.assertRaisesRegex(ValueError, "compile model failed"):
                model.build_component_relay(relay)
            self.assertEqual(model.env["FOUNDRY_EVM_VERSION"], "cancun")

    def test_actual_deployment_and_fresh_restore_gates_precede_package(self):
        text = PATH.read_text()
        self.assertIn("verificationKeyHash()(bytes32)", text)
        self.assertIn("IS_TESTNET_VERIFIER()(bool)", text)
        interface = (ROOT / "lib/contract_interface/src/lib.rs").read_text()
        self.assertIn("function IS_TESTNET_VERIFIER() external view returns (bool)", interface)
        self.assertNotIn("isTestnetVerifier()(bool)", text)
        self.assertIn("getProtocolVersion()(uint256)", text)
        self.assertIn('"chain", "unpause-deposits"', text)
        self.assertLess(text.index("self.stop_nodes(preserve_anvil=True)"), text.index('self.rpc("anvil_dumpState"'))
        self.assertLess(text.index("actual restored receipt join"), text.index("descriptor ="))
        self.assertIn('"fresh_node_db": True', text)
        self.assertIn('"integration_suite_qualification": False', text)
        self.assertIn('"canonical_syscoin_qualification": False', text)

    def test_new_component_replay_export_stop_restore_and_packaging_order(self):
        text = PATH.read_text()
        self.assertLess(text.index("self.stop_nodes(preserve_anvil=True)"), text.index('"export-replay"'))
        self.assertLess(text.index('"export-replay"'), text.index('self.rpc("anvil_dumpState"'))
        self.assertLess(text.index('"recover-replay"'), text.index('"actual fresh replay canonical anchor"'))
        self.assertLess(text.index('"actual fresh replay canonical anchor"'), text.index('"actual restored receipt join"'))
        self.assertIn('"replay_archives": replay_archives', text)
        self.assertIn('cfg["replay_archive"] = {"type": "Noop"}', text)
        self.assertIn('"restored-" + name + "-replay-archive"', text)
        contracts = {"ecosystem_contracts": {"bridgehub_proxy_addr": "0x" + "1" * 40,
                      "l1_bytecodes_supplier_addr": "0x" + "2" * 40}}
        cfg = subject.server_config("gateway", contracts, "http://127.0.0.1:8545", None,
                                    3050, pathlib.Path("/unit-genesis"), pathlib.Path("/unit-work"))
        self.assertEqual(cfg["replay_archive"], {"type": "FileSystem", "root_path": "/unit-work/gateway-replay-archive",
                                                "encryption": {"type": "Noop"}})
        self.assertEqual(subject.CHAIN_IDS, {"gateway": 57001, "edge6565": 6565, "edge6566": 6566})
        self.assertEqual(subject.PORT_NAMES, {"anvil", "gateway", "edge6565", "edge6566"})
        self.assertEqual(cfg["genesis"]["chain_id"], 57001)
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(text))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertIn("chain_args('gateway', 57001, ecosystem_wallet)", execute)
        self.assertNotIn("chain_args('default'", execute)
        self.assertNotIn("self.cli('chain', 'create', *chain_args('gateway'", execute)
        self.assertIn("'default_chain_id': 57001", execute)
        self.assertIn("'layout_views'", execute)
        self.assertIn("gate.checked_gateway_views(direct_l1_view, cfg)", execute)

    def test_fresh_replay_preserves_the_genuine_seed_deployment_marker(self):
        text = PATH.read_text()
        execute = ast.unparse(next(node for node in ast.walk(ast.parse(text))
                                   if isinstance(node, ast.FunctionDef) and node.name == "execute"))
        self.assertLess(execute.index("self.stop_nodes(preserve_anvil=True)"),
                        execute.index("'seed database marker deployment/replay join'"))
        recover = execute.index("'recover-replay'")
        publish = execute.index("destination / 'database_identity.json'", recover)
        startup = execute.index("self.start(", publish)
        self.assertLess(recover, publish)
        self.assertLess(publish, startup)
        self.assertIn("os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW", execute[publish - 120:publish + 260])
        self.assertIn("gate.checked_database_identity(json.loads(marker_raw), chain_id)", execute)
        self.assertIn("marker_doc['l2_genesis_block_hash']", execute)
        self.assertNotIn("'schema_version': 1", execute[recover:startup])
        production = (ROOT / "node/bin/src/database_identity.rs").read_text()
        self.assertIn("stored == *expected", production)
        self.assertIn("unmarked database root", production)

    def test_component_zstd_packages_exact_bytes_with_direct_create_only_stdout(self):
        with tempfile.TemporaryDirectory() as temp:
            work = pathlib.Path(temp)
            source, output = work / "original.json", work / "l1-state.json.zst"
            original = b'{"historical_states":["unchanged public bytes"]}\n'
            source.write_bytes(original)
            model = object.__new__(subject.Generation)
            model.children, model.command_children = [], []
            model.tools, model.work = {"zstd": "/unit/zstd"}, work
            model.env, model.plan = {}, {"deadline_unix": subject.time.time() + 1000}
            seen = []
            class Child:
                returncode = 0
                def wait(self, timeout):
                    self.timeout = timeout
                    return 0
            def popen(argv, **options):
                seen.append((argv, options["stdin"].read(), options["start_new_session"]))
                options["stdout"].write(b"pure-model-zstd-output")
                return Child()
            with patch.object(subject.subprocess, "Popen", side_effect=popen):
                model.compress_component_snapshot(source, output)
            self.assertEqual(seen, [(["/unit/zstd", "--long=27", "-19", "-T2", "--stdout"], original, True)])
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(len(model.command_children), 1)
            self.assertLessEqual(model.command_children[0].timeout, 300)
            with self.assertRaises(FileExistsError):
                model.compress_component_snapshot(source, output)
        text = PATH.read_text()
        self.assertIn('"compressed_state": identity("l1-state.json.zst")', text)
        self.assertIn('self.work / "anvil-state.original.json.gz"', text)
        self.assertIn('self.compress_component_snapshot(replay_state, package / "l1-state.json.zst")', text)

    def test_component_zstd_timeout_is_cooperative_and_retains_partial_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            work = pathlib.Path(temp)
            source, output = work / "original.json", work / "l1-state.json.zst"
            source.write_bytes(b"exact original\n")
            model = object.__new__(subject.Generation)
            model.children, model.command_children = [], []
            model.tools, model.work = {"zstd": "/unit/zstd"}, work
            model.env, model.plan = {}, {"deadline_unix": subject.time.time() + 1000}
            class Child:
                pid, returncode = 42, 0
                def __init__(self):
                    self.waits = []
                def wait(self, timeout):
                    self.waits.append(timeout)
                    if len(self.waits) == 1:
                        raise subject.subprocess.TimeoutExpired("pure-zstd-model", timeout)
                    return 0
            child = Child()
            with patch.object(subject.subprocess, "Popen", return_value=child), \
                    patch.object(subject.os, "getpgid", return_value=42), \
                    patch.object(subject.os, "killpg") as stop:
                with self.assertRaisesRegex(ValueError, "finite execution deadline"):
                    model.compress_component_snapshot(source, output)
            stop.assert_called_once_with(42, subject.signal.SIGTERM)
            self.assertEqual(child.waits[-1], 60)
            self.assertTrue(output.exists())
            self.assertEqual(source.read_bytes(), b"exact original\n")

    def test_l1_dump_preserves_history_needed_by_fresh_wal_startup(self):
        calls = [node for node in ast.walk(ast.parse(PATH.read_text()))
                 if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                 and node.func.attr == "rpc" and node.args
                 and isinstance(node.args[0], ast.Constant)
                 and node.args[0].value == "anvil_dumpState"]
        self.assertEqual(len(calls), 1)
        self.assertEqual(ast.literal_eval(calls[0].args[1]), [True])

    def test_snapshot_stream_preserves_original_and_reports_exact_or_lower_bound(self):
        from types import SimpleNamespace
        execute = next(node for node in ast.walk(ast.parse(PATH.read_text()))
                       if isinstance(node, ast.FunctionDef) and node.name == "execute")
        start = next(i for i, node in enumerate(execute.body) if isinstance(node, ast.Assign)
                     and any(isinstance(target, ast.Name) and target.id == "original_dump" for target in node.targets))
        end = next(i for i, node in enumerate(execute.body[start:], start)
                   if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                   and node.value.func.id == "need" and "bounded decoded state archive" in ast.unparse(node))
        body = copy.deepcopy(execute.body[start:end + 1])
        ceiling = next(node for node in body if isinstance(node, ast.Assign)
                       and any(isinstance(target, ast.Name) and target.id == "decoded_ceiling" for target in node.targets))
        self.assertEqual(ast.unparse(ceiling.value), "2 * 1024 ** 3")
        ceiling.value = ast.Constant(32)  # Small pure model, not an execution plan.
        code = compile(ast.fix_missing_locations(ast.Module(body=body, type_ignores=[])), "snapshot-stream-model", "exec")
        for payload in (b"{\"block\": {}}", b"x" * 33):
            with tempfile.TemporaryDirectory() as temp:
                work = pathlib.Path(temp)
                raw = subject.gzip.compress(payload)
                scope = {**subject.__dict__, "self": SimpleNamespace(work=work), "raw_dump": raw}
                if len(payload) <= 32:
                    exec(code, scope)
                else:
                    with self.assertRaisesRegex(ValueError, "bounded decoded state archive"):
                        exec(code, scope)
                self.assertEqual((work / "anvil-state.original.json.gz").read_bytes(), raw)
                self.assertEqual((work / "restored-state.json").read_bytes(), payload)
                sizes = subject.json.loads((work / "SNAPSHOT-SIZE.actual.json").read_bytes())
                self.assertEqual(sizes["decoded_state_bytes"], len(payload))
                self.assertEqual(sizes["decoded_size_is_exact"], len(payload) <= 32)
                self.assertEqual(sizes["historical_states_preserved"], True)
                if len(payload) <= 32:
                    self.assertEqual(scope["decoded_hash"].hexdigest(), hashlib.sha256(payload).hexdigest())
                with self.assertRaises(FileExistsError):
                    exec(code, scope)

    def test_edge_readback_uses_actual_gateway_diamond_not_root(self):
        model = object.__new__(subject.Generation)  # Pure method model, no constructor/process/RPC.
        model.ecosystem = pathlib.Path("/unit-only")
        model.url = "http://127.0.0.1:8545"
        model.gateway_url = "http://127.0.0.1:3052"
        model.tools = {"cast": "/unit-only/cast"}
        model.run = lambda argv: "true"
        model.rpc = lambda method, params, url: hex(10**20)
        root_diamond, gateway_diamond, verifier = ("0x" + char * 40 for char in "abc")
        observed = []
        model.contracts = lambda name: {"l1": {"diamond_proxy_addr": root_diamond}}
        def call(address, signature, url):
            observed.append((address, signature, url))
            return {"getVerifier()(address)": verifier, "verificationKeyHash()(bytes32)": subject.gate.VK,
                    "IS_TESTNET_VERIFIER()(bool)": "true", "getProtocolVersion()(uint256)": str(32 << 32),
                    "REVERTER_ROLE()(bytes32)": "0x" + "d" * 64,
                    "COMMITTER_ROLE()(bytes32)": "0x" + "1" * 64,
                    "PROVER_ROLE()(bytes32)": "0x" + "2" * 64,
                    "EXECUTOR_ROLE()(bytes32)": "0x" + "3" * 64}[signature]
        model.call = call
        migration = {"gateway_chain_id": 57001, "validator_timelock_addr": subject.TIMELOCK,
                     "diamond_proxy_addr": gateway_diamond}
        with patch.object(subject, "read_yaml", return_value=migration):
            self.assertEqual(model.chain_readback("edge6565")["diamond"], gateway_diamond)
        self.assertEqual(observed[0], (gateway_diamond, "getVerifier()(address)", model.gateway_url))
        self.assertNotIn(root_diamond, [row[0] for row in observed])

    def test_reverter_loader_keeps_explicit_three_physical_chains(self):
        rust = (ROOT / "integration-tests/src/wallets.rs").read_text()
        self.assertIn("[6565, 6566, 57001].contains(&chain_id)", rust)
        self.assertIn("hasRoleForChainId(uint256,bytes32,address)(bool)", PATH.read_text())
        self.assertNotIn("hasRole(bytes32,address)(bool)", PATH.read_text())

    def test_root_reverter_uses_only_owned_ordinary_admin_permission(self):
        model = object.__new__(subject.Generation)  # Pure API/receipt model, no process or RPC.
        model.url = "http://127.0.0.1:8545"
        model.tools = {"cast": "/unit-only/cast"}
        diamond, timelock, admin = ("0x" + char * 40 for char in "abc")
        model.contracts = lambda name: {"l1": {"diamond_proxy_addr": diamond,
            "validator_timelock_addr": timelock, "chain_admin_addr": admin}}
        role = "0x" + "d" * 64
        receipt = {"transactionHash": "0x" + "e" * 64, "blockHash": "0x" + "f" * 64,
                   "blockNumber": "0x10", "status": "0x1"}
        calls = []
        def run(argv):
            calls.append(argv)
            if argv[1] == "call":
                return "0x" + "0" * 64
            if argv[1] == "calldata":
                self.assertEqual(argv[2:], ["grantRole(address,bytes32,address)", diamond, role, subject.gate.REVERTER])
                return "0x1234"
            self.assertEqual(argv[1:6], ["send", admin, "multicall((address,uint256,bytes)[],bool)",
                                         "[(" + timelock + ",0,0x1234)]", "true"])
            return subject.json.dumps(receipt)
        model.run = run
        model.call = lambda address, signature, url: {
            "getAdmin()(address)": admin, "owner()(address)": subject.MANAGEMENT_ADDRESS,
            "REVERTER_ROLE()(bytes32)": role}[signature]
        model.rpc = lambda method, params: receipt if method == "eth_getTransactionReceipt" else {"hash": receipt["blockHash"]}
        self.assertEqual(model.grant_root_reverter("gateway")["status"], 1)
        self.assertIn("--unlocked", calls[-1])
        self.assertIn(subject.MANAGEMENT_ADDRESS, calls[-1])
        self.assertNotIn("--private-key", calls[-1])
        # Wrong actual owner/diamond/admin role/reverted receipt fail before any successful grant qualification.
        for field in ("owner", "diamond", "role_admin", "status", "canonical"):
            with self.subTest(field=field):
                saved_call, saved_run, saved_rpc = model.call, model.run, model.rpc
                if field in {"owner", "diamond"}:
                    wrong = "owner()(address)" if field == "owner" else "getAdmin()(address)"
                    model.call = lambda a, s, u, wrong=wrong: "0x" + "9" * 40 if s == wrong else saved_call(a, s, u)
                elif field == "role_admin":
                    model.run = lambda argv: "0x" + "9" * 64 if argv[1] == "call" else saved_run(argv)
                elif field == "status":
                    model.run = lambda argv: subject.json.dumps({**receipt, "status": "0x0"}) if argv[1] == "send" else saved_run(argv)
                else:
                    model.rpc = lambda method, params: {**receipt, "status": "0x0"} if method == "eth_getTransactionReceipt" else saved_rpc(method, params)
                with self.assertRaises(ValueError):
                    model.grant_root_reverter("gateway")
                model.call, model.run, model.rpc = saved_call, saved_run, saved_rpc
        with self.assertRaises(ValueError):
            model.grant_root_reverter("edge6565")
        chain = subject.public_wallets(False)
        self.assertEqual(chain["operator"]["address"], subject.COMMIT_ADDRESS)
        self.assertEqual(chain["blob_operator"]["address"], subject.COMMIT_ADDRESS)

    def test_migrated_reverter_uses_existing_priority_admin_not_alias_impersonation(self):
        method = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                  if isinstance(node, ast.FunctionDef) and node.name == "grant_gateway_reverter"))
        self.assertIn("adminL1L2TxViaGateway(address,uint256,uint256,uint256,address,uint256,bytes,address,bool)", method)
        self.assertIn("'false', '--rpc-url', self.url", method)
        self.assertIn("prepared['admin_address'].lower() == root_admin.lower()", method)
        self.assertIn("'--value', str(value), '--from', MANAGEMENT_ADDRESS, '--unlocked'", method)
        self.assertIn("NewPriorityRequestId(uint256,bytes32)", method)
        self.assertIn("'actual Gateway priority role grant succeeded'", method)
        self.assertNotIn("'--from', alias", method)
        readback = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                    if isinstance(node, ast.FunctionDef) and node.name == "chain_readback"))
        self.assertIn("('COMMITTER', 'PROVER', 'EXECUTOR')", readback)
        self.assertIn("balance >= 10 ** 18", readback)

    def test_finalized_migration_unpause_is_actual_state_aware(self):
        from unittest.mock import Mock
        model = object.__new__(subject.Generation)
        model.url, model.gateway_url = "http://127.0.0.1:8545", "http://127.0.0.1:3052"
        model.ecosystem = pathlib.Path("/unit-component")
        model.contracts = lambda name: {"l1": {"diamond_proxy_addr": "root-diamond"}}
        model.cli = Mock()
        with patch.object(subject, "read_yaml", return_value={"diamond_proxy_addr": "gateway-diamond"}):
            for initial in ("false", "true"):
                model.cli.reset_mock()
                model.call = Mock(side_effect=[initial, "false", "false"])
                model.ensure_unpaused("edge6565")
                self.assertEqual(model.cli.call_count, int(initial == "true"))
                self.assertEqual(model.call.call_args_list[-1].args,
                                 ("gateway-diamond", "depositsPaused()(bool)", model.gateway_url))
            for values in (["unknown"], ["false", "true"], ["true", "false", "true"]):
                model.call = Mock(side_effect=values)
                with self.assertRaises(ValueError):
                    model.ensure_unpaused("edge6565")

    def test_relay_namespace_uses_existing_era_out_permission(self):
        tree = ast.parse((ROOT / "scripts/fixtures/generate-v32-component-fixture.py").read_text())
        method = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.FunctionDef) and node.name == "execute")
        text = ast.unparse(method)
        self.assertIn("contracts/l1-contracts/out/anvil-component-relay", text)
        self.assertIn("relay_out.parent.resolve(strict=True) == relay_out.parent", text)
        self.assertIn("relay_parent.st_uid == os.getuid()", text)
        self.assertIn("stat.S_IMODE(relay_parent.st_mode) == 448", text)
        self.assertIn("not relay_out.exists()", text)
        self.assertIn("not relay_out.is_symlink()", text)
        self.assertNotIn("self.work / 'relay-out'", text)
        self.assertIn("self.build_component_relay(relay_out)", text)
        relay = ast.unparse(next(node for node in ast.walk(ast.parse(PATH.read_text()))
                                 if isinstance(node, ast.FunctionDef) and node.name == "build_component_relay"))
        self.assertIn("'--evm-version', 'prague'", relay)

    def test_component_cli_uses_individually_qualified_tools(self):
        tree = ast.parse(PATH.read_text())
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                 and isinstance(node.func, ast.Attribute) and node.func.attr == "run"
                 and node.args and isinstance(node.args[0], ast.List)
                 and node.args[0].elts
                 and ast.unparse(node.args[0].elts[0]) == "self.tools['zkstack']"]
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertEqual(ast.literal_eval(call.args[0].elts[1]), "--ignore-prerequisites")
        admission = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                         and node.name == "checked_plan")
        self.assertIn("held(ref, 512 * 1024 * 1024, tool=True)", ast.unparse(admission))

    def test_adapter_is_explicit_and_preserves_normal_cli_branch(self):
        text = (ROOT / "scripts/fixtures/zkstack-anvil-component.patch").read_text()
        self.assertIn('SYSCOIN_ANVIL_COMPONENT_ONLY', text)
        self.assertIn('self.private_key.is_none()', text)
        self.assertIn('url.host_str() == Some("127.0.0.1")', text)
        self.assertIn('ForgeScriptArg::Unlocked', text)
        self.assertIn('     if !forge.wallet_args_passed()', text)

    def test_adapted_cli_frozen_install_and_component_only_shell_boundary(self):
        text = (ROOT / "scripts/fixtures/zkstack-anvil-component.patch").read_text()
        added = "\n".join(line[1:] for line in text.splitlines()
                          if line.startswith("+") and not line.startswith("+++"))
        self.assertIn("yarn install --frozen-lockfile --ignore-scripts --check-files", added)
        self.assertIn("yarn install --frozen-lockfile --check-files", added)
        self.assertNotIn("yarn check --integrity", added)
        self.assertEqual(added.count("std::fs::read(&lock)? == original_lock"), 2)
        self.assertIn('std::env::var("SYSCOIN_ANVIL_COMPONENT_ONLY").as_deref() != Ok("31337")', added)
        self.assertIn("configure_shell_autocompletion()", added)
        self.assertNotIn('set_var("HOME"', added)

    def test_config_ci_retains_real_startup_rpc_and_transactions(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        shell = (ROOT / ".github/scripts/test-configs.sh").read_text()
        self.assertIn('--bin anvil-component-bootstrap', workflow)
        self.assertIn('SYSCOIN_ANVIL_COMPONENT_ONLY=31337 ./anvil-component-bootstrap', shell)
        self.assertIn('./zksync-os-server --config', shell)  # Canonical branch stays real and separate.
        self.assertIn('cast send', shell)
        self.assertIn('python3 scripts/fixtures/verify-v32-component-fixture.py', shell)
        self.assertIn('CANONICAL_V8_REGENERATION_REQUIRED', shell)


if __name__ == "__main__":
    unittest.main()
