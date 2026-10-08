"""SYSCOIN: Protected address-only administrators; no network, signing or deployment."""
from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
COMMON = ROOT / "scripts/gateway-launch/_common.sh"
ADMIN = "0x622a54ea3a123127ca5fe8b98de90e957471093a"
GENERATED = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"


class ExternalAdminWalletTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.gateway = self.root / "gateway"
        self.wallet = self.gateway / "chains/gateway/configs/wallets.yaml"
        self.wallet.parent.mkdir(parents=True)
        self.keystores = self.root / ".foundry/keystores"
        self.keystores.mkdir(parents=True)
        self.account = self.keystores / "v32-admin"
        self.account.write_text("{}\n")
        self.account.chmod(0o600)
        self.password = self.root / "admin.password"
        self.password.write_text("fixture-only\n")
        self.password.chmod(0o600)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        cast = self.bin / "cast"
        cast.write_text("""#!/usr/bin/env python3
import json,os,sys
args=sys.argv[1:]
if args[:2] == ['wallet','address']:
    with open(os.environ['CAST_ARGS_LOG'],'a') as handle: handle.write(json.dumps(args)+'\\n')
    password=os.path.realpath(os.path.join(os.environ['VALIDATION_CWD'],os.environ['DEPLOYER_PASSWORD_FILE']))
    if args[2:] != ['--account','v32-admin','--password-file',password]:
        raise SystemExit(91)
    print(os.environ['CAST_ACCOUNT_ADDRESS'])
elif args[0] == 'keccak':
    print('0x'+'00'*12+os.environ['GENERATED_ADDRESS'][2:])
else: raise SystemExit(92)
""")
        cast.chmod(0o755)
        self.env = {**os.environ, "HOME": str(self.root), "PATH": f"{self.bin}:{os.environ['PATH']}",
                    "COMMON": str(COMMON), "GATEWAY_DIR": str(self.gateway),
                    "FOUNDRY_KEYSTORE_DIR": str(self.keystores),
                    "DEPLOYER_SIGNER": "account", "DEPLOYER_ACCOUNT_NAME": "v32-admin",
                    "DEPLOYER_PASSWORD_FILE": str(self.password),
                    "EDGE_GATEWAY_GOVERNOR_SIGNER": "account",
                    "EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME": "v32-admin",
                    "EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE": str(self.password),
                    "CAST_ACCOUNT_ADDRESS": ADMIN, "GENERATED_ADDRESS": GENERATED,
                    "VALIDATION_CWD": str(self.gateway),
                    "CAST_ARGS_LOG": str(self.root / "cast-args.jsonl"),
                    "PYTHONDONTWRITEBYTECODE": "1"}
        self.write_wallet({"deployer": self.external(), "governor": self.external()})

    def external(self, address=ADMIN):
        return {"address": address, "private_key": None}

    def generated(self):
        return {"address": GENERATED, "private_key": "0x" + "00" * 31 + "01"}

    def write_wallet(self, entries):
        self.wallet.write_text(yaml.safe_dump(entries))
        self.wallet.chmod(0o600)

    def run_shell(self, command, **overrides):
        return subprocess.run(["bash", "-c", 'set -euo pipefail; source "$COMMON"; ' + command],
                              env={**self.env, **overrides}, cwd=self.gateway,
                              capture_output=True, text=True, timeout=10)

    def test_null_administrators_authenticate_same_named_account(self):
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-addresses gateway deployer governor')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"{ADMIN}|{ADMIN}")

    def test_selector_arguments_are_repeated_and_preserve_spaces(self):
        password = self.root / "admin password"
        password.write_text("fixture-only\n")
        password.chmod(0o600)
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args gateway deployer governor; '
                                'printf "%s\\0" "${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]}"',
                                DEPLOYER_PASSWORD_FILE=str(password),
                                EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE=str(password))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split("\0")[:-1],
                         ["--additional-args", "--account", "--additional-args", "v32-admin",
                          "--additional-args", "--password-file", "--additional-args", str(password.resolve()),
                          "--additional-args", "--sender", "--additional-args", ADMIN])

    def test_forge_sender_is_authenticated_but_never_forwarded_to_cast_derivation(self):
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-forge-args gateway deployer governor')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[-4:],
                         ["--additional-args", "--sender", "--additional-args", ADMIN])
        calls = [json.loads(line) for line in (self.root / "cast-args.jsonl").read_text().splitlines()]
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertEqual(call, ["wallet", "address", "--account", "v32-admin",
                                    "--password-file", str(self.password.resolve())])

    def test_account_mismatch_fails_before_broadcast(self):
        marker = self.root / "broadcast"
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args gateway deployer governor; '
                                'touch "$BROADCAST_MARKER"', BROADCAST_MARKER=str(marker),
                                CAST_ACCOUNT_ADDRESS="0x" + "33" * 20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("address/account mismatch", result.stderr)
        self.assertFalse(marker.exists())

    def test_external_backend_and_role_are_explicit_and_bounded(self):
        for backend in ("", "generated", "private-key", "keystore", "ledger", "aws"):
            with self.subTest(backend=backend):
                result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer', DEPLOYER_SIGNER=backend)
                self.assertNotEqual(result.returncode, 0)
        for role in ("operator", "blob_operator", "prove_operator", "execute_operator", "fee_account", "token_multiplier_setter"):
            with self.subTest(role=role):
                self.write_wallet({role: self.external()})
                result = self.run_shell(f'gl_authenticate_chain_wallet_roles gateway {role}')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("only for governor/deployer", result.stderr)

    def test_missing_empty_and_malformed_keys_never_fall_back(self):
        for key in ("", "broken", False, "0x" + "00" * 32):
            with self.subTest(key=key):
                self.write_wallet({"deployer": {"address": ADMIN, "private_key": key}})
                result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer')
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "cast-args.jsonl").exists())
        self.write_wallet({"deployer": {"address": ADMIN}})
        result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer')
        self.assertNotEqual(result.returncode, 0)

    def test_generated_administrators_and_operators_keep_private_key_authentication(self):
        self.write_wallet({role: self.generated() for role in ("deployer", "governor", "operator", "blob_operator")})
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-addresses gateway deployer governor operator blob_operator; '
                                'gl_authenticate_chain_wallet_roles --print-forge-args gateway deployer governor')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["|".join([GENERATED] * 4), "[]"])
        self.assertFalse((self.root / "cast-args.jsonl").exists())

    def test_one_global_selector_cannot_sign_for_another_role(self):
        self.write_wallet({"deployer": self.external(), "governor": self.generated()})
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args gateway deployer governor')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match every requested", result.stderr)

    def set_ecosystem_governor(self, entry):
        path = self.gateway / "configs/wallets.yaml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(yaml.safe_dump({"governor": entry}))
        path.chmod(0o600)

    def conversion_selector(self, marker):
        source = (ROOT / "scripts/gateway-launch/gateway-convert-settlement.sh").read_text()
        guard = next(line for line in source.splitlines() if line.startswith("gl_prepare_zkstack_admin_wallet_args "))
        return self.run_shell('GATEWAY_CHAIN_NAME=gateway; gl_zkstack_pty() { touch "$BROADCAST_MARKER"; }; '
                              + guard + '; gl_zkstack_pty zkstack chain gateway create-tx-filterer',
                              BROADCAST_MARKER=str(marker))

    def test_conversion_rejects_chain_governor_mismatch_before_any_wrapper_call(self):
        self.write_wallet({"deployer": self.external(), "governor": self.generated()})
        self.set_ecosystem_governor(self.external())
        marker = self.root / "broadcast"
        result = self.conversion_selector(marker)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match every requested", result.stderr)
        self.assertFalse(marker.exists())

    def test_conversion_rejects_ecosystem_governor_mismatch_before_any_wrapper_call(self):
        self.set_ecosystem_governor(self.generated())
        marker = self.root / "broadcast"
        result = self.conversion_selector(marker)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match every requested", result.stderr)
        self.assertFalse(marker.exists())

    def test_conversion_missing_chain_governor_cannot_fall_back_to_ecosystem(self):
        self.write_wallet({"deployer": self.external()})
        self.set_ecosystem_governor(self.external())
        marker = self.root / "broadcast"
        result = self.conversion_selector(marker)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing governor wallet entry", result.stderr)
        self.assertFalse(marker.exists())

    def test_conversion_all_matching_administrators_select_account(self):
        self.set_ecosystem_governor(self.external())
        marker = self.root / "broadcast"
        result = self.conversion_selector(marker)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-forge-args --conversion-actors gateway')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout),
                         ["--additional-args", "--account", "--additional-args", "v32-admin",
                          "--additional-args", "--password-file", "--additional-args", str(self.password.resolve()),
                          "--additional-args", "--sender", "--additional-args", ADMIN])

    def test_conversion_generated_only_mixed_identities_keep_empty_selectors(self):
        # Distinct generated private keys bind independently; no shared external
        # selector is emitted, so upstream retains each stage's proper role key.
        generated_second = {"address": "0x" + "66" * 20, "private_key": "0x" + "00" * 31 + "02"}
        cast = self.bin / "cast"
        source = cast.read_text()
        source = source.replace("print('0x'+'00'*12+os.environ['GENERATED_ADDRESS'][2:])",
                                "print('0x'+'00'*12+('66'*20 if args[1].startswith('0xc6047f') else os.environ['GENERATED_ADDRESS'][2:]))")
        cast.write_text(source)
        self.write_wallet({"deployer": self.generated(), "governor": generated_second})
        self.set_ecosystem_governor(self.generated())
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-forge-args --conversion-actors gateway')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])

    def set_ecosystem_administrators(self, deployer=None, governor=None):
        path = self.gateway / "configs/wallets.yaml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(yaml.safe_dump({"deployer": deployer or self.external(),
                                        "governor": governor or self.external()}))
        path.chmod(0o600)

    def migration_preflight(self, marker, **overrides):
        source = (ROOT / "scripts/gateway-launch/edge-chain-migrate-to-gateway.sh").read_text()
        guard = next(line.strip() for line in source.splitlines()
                     if line.strip().startswith("gl_prepare_zkstack_admin_wallet_args --migration-actors "))
        return self.run_shell('EDGE_CHAIN_NAME=gateway; ' + guard + '; touch "$BROADCAST_MARKER"',
                              BROADCAST_MARKER=str(marker), **overrides)

    def test_migration_four_matching_null_roles_select_one_account_before_pause(self):
        # This is an explicit synthetic role fixture, not the actual 81293 receipt.
        self.set_ecosystem_administrators()
        marker = self.root / "pause"
        result = self.migration_preflight(marker)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())
        calls = [json.loads(line) for line in (self.root / "cast-args.jsonl").read_text().splitlines()]
        self.assertEqual(len(calls), 4)
        selected = self.run_shell('gl_authenticate_chain_wallet_roles --print-forge-args --migration-actors gateway')
        self.assertEqual(selected.returncode, 0, selected.stderr)
        self.assertEqual(json.loads(selected.stdout)[-4:],
                         ["--additional-args", "--sender", "--additional-args", ADMIN])

    def test_migration_every_distinct_or_missing_actor_refuses_before_pause(self):
        for area, role in (("chain", "governor"), ("chain", "deployer"),
                           ("ecosystem", "governor"), ("ecosystem", "deployer")):
            with self.subTest(area=area, role=role):
                chain = {"deployer": self.external(), "governor": self.external()}
                ecosystem = {"deployer": self.external(), "governor": self.external()}
                (chain if area == "chain" else ecosystem)[role] = self.generated()
                self.write_wallet(chain)
                self.set_ecosystem_administrators(**ecosystem)
                marker = self.root / "pause"
                result = self.migration_preflight(marker)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("must match every requested", result.stderr)
                self.assertFalse(marker.exists())
        self.write_wallet({"governor": self.external()})
        self.set_ecosystem_administrators()
        result = self.migration_preflight(self.root / "pause")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing deployer", result.stderr)

    def test_migration_unsupported_external_backend_refuses_before_account_lookup(self):
        self.set_ecosystem_administrators()
        for backend in ("private-key", "keystore", "ledger", "trezor", "aws", "gcp"):
            with self.subTest(backend=backend):
                result = self.migration_preflight(self.root / "pause",
                                                  EDGE_GATEWAY_GOVERNOR_SIGNER=backend)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "pause").exists())
                self.assertFalse((self.root / "cast-args.jsonl").exists())

    def test_migration_generated_roles_keep_empty_selector(self):
        self.write_wallet({"deployer": self.generated(), "governor": self.generated()})
        self.set_ecosystem_administrators(self.generated(), self.generated())
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args --migration-actors gateway; '
                                'printf "%s\\n" "${#GL_ZKSTACK_ADMIN_WALLET_ARGS[@]}"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "0")
        self.assertFalse((self.root / "cast-args.jsonl").exists())

    def test_migration_distinct_generated_roles_keep_individual_keys(self):
        # Synthetic generated roles must not acquire a shared account selector.
        second = {"address": "0x" + "66" * 20, "private_key": "0x" + "00" * 31 + "02"}
        cast = self.bin / "cast"
        cast.write_text(cast.read_text().replace(
            "print('0x'+'00'*12+os.environ['GENERATED_ADDRESS'][2:])",
            "print('0x'+'00'*12+('66'*20 if args[1].startswith('0xc6047f') else os.environ['GENERATED_ADDRESS'][2:]))"))
        self.write_wallet({"deployer": self.generated(), "governor": second})
        self.set_ecosystem_administrators(second, self.generated())
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-forge-args --migration-actors gateway')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])
        self.assertFalse((self.root / "cast-args.jsonl").exists())

    def test_migration_fixed_group_cannot_omit_or_choose_roles(self):
        self.set_ecosystem_administrators()
        for args in ("--migration-actors gateway governor", "--migration-actors --conversion-actors gateway",
                     "--migration-actors --ecosystem-only governor"):
            with self.subTest(args=args):
                result = self.run_shell('gl_prepare_zkstack_admin_wallet_args ' + args)
                self.assertNotEqual(result.returncode, 0)

    def test_migration_and_finalize_actual_call_sites_forward_safe_selector(self):
        self.set_ecosystem_administrators()
        source = (ROOT / "scripts/gateway-launch/edge-chain-migrate-to-gateway.sh").read_text()
        guard = next(line.strip() for line in source.splitlines()
                     if line.strip().startswith("gl_prepare_zkstack_admin_wallet_args --migration-actors "))
        self.assertLess(source.index(guard), source.index('configure_gateway_rpc_url_in_chain_secrets "${EDGE_CHAIN_NAME}"'))
        self.assertLess(source.index(guard), source.index("zkstack chain pause-deposits "))
        for command, wrapper, output in (("migrate-to-gateway", "gl_zkstack_private_pty", "migrate_output"),
                                         ("finalize-chain-migration-to-gateway", "gl_zkstack_pty", "finalize_output")):
            with self.subTest(command=command):
                start = source.index(wrapper + " zkstack chain gateway " + command + " ")
                invocation = source[start:source.index('2>&1)', start)]
                marker = self.root / "migration-args.json"
                if marker.exists(): marker.unlink()
                record = '''
EDGE_CHAIN_NAME=gateway; GATEWAY_CHAIN_NAME=gateway; L1_RPC_URL=http://fixture.invalid; GATEWAY_RPC_URL=http://fixture-gateway.invalid
gl_zkstack_private_pty() { gl_zkstack_pty "$@"; }
gl_zkstack_pty() {
  python3 - "$@" <<'PY'
import json,os,sys
with open(os.environ['MIGRATION_ARGS_LOG'],'x') as output: json.dump(sys.argv[1:],output)
PY
}
'''
                result = self.run_shell(record + guard + '\n' + invocation + '\n', MIGRATION_ARGS_LOG=str(marker))
                self.assertEqual(result.returncode, 0, result.stderr)
                args = json.loads(marker.read_text())
                self.assertEqual(args[:4], ["zkstack", "chain", "gateway", command])
                self.assertIn("--additional-args", args)
                self.assertIn("--account", args)
                self.assertIn(str(self.password.resolve()), args)
                self.assertNotIn("--private-key", args)

    def test_migration_reauthenticates_fixed_group_after_every_funding_refresh(self):
        source = (ROOT / "scripts/gateway-launch/edge-chain-migrate-to-gateway.sh").read_text()
        for action in ("zkstack chain pause-deposits", "zkstack chain gateway migrate-to-gateway",
                       "zkstack chain gateway finalize-chain-migration-to-gateway"):
            with self.subTest(action=action):
                before = source[:source.index(action)]
                refresh = before.rindex('refresh_l1_admin_wallet_funding "${EDGE_CHAIN_NAME}"')
                selection = before.rindex('gl_prepare_zkstack_admin_wallet_args --migration-actors "${EDGE_CHAIN_NAME}"')
                self.assertGreater(selection, refresh)

    def test_relative_password_remains_bound_after_forge_changes_directory(self):
        relative = self.gateway / "admin.password"
        relative.write_text("selected-fixture\n")
        relative.chmod(0o600)
        contracts = self.root / "contracts"
        contracts.mkdir()
        decoy = contracts / "admin.password"
        decoy.write_text("wrong-fixture\n")
        decoy.chmod(0o600)
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args gateway deployer governor; '
                                'cd "$CONTRACTS_DIR"; cat "${GL_ZKSTACK_ADMIN_WALLET_ARGS[7]}"',
                                DEPLOYER_PASSWORD_FILE="admin.password",
                                EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE="admin.password",
                                CONTRACTS_DIR=str(contracts))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "selected-fixture")

    def test_generated_empty_selector_adds_no_cli_argument_under_nounset(self):
        self.write_wallet({role: self.generated() for role in ("deployer", "governor")})
        result = self.run_shell('gl_prepare_zkstack_admin_wallet_args gateway deployer governor; '
                                'set -- zkstack chain init ${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]+"${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]}"}; '
                                'printf "%s\\n" "$#"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "3")

    def deposit_command(self, action, **overrides):
        # SYSCOIN: Exercise the actual launcher call sites, but replace the PTY
        # boundary with an argv recorder; no zkstack, Forge or RPC is invoked.
        log = self.root / "deposit-args.json"
        if log.exists():
            log.unlink()
        source = (ROOT / "scripts/gateway-launch/edge-chain-migrate-to-gateway.sh").read_text()
        if action == "pause":
            body = source.split('  pause_output=""\n', 1)[1].split('  migrate_output=""\n', 1)[0]
        else:
            body = source.split("ensure_deposits_unpaused() {\n", 1)[1].split("\n}\n\nrefresh_l1_admin_wallet_funding()", 1)[0]
            body = 'ensure_deposits_unpaused() {\n' + body + '\n}\nensure_deposits_unpaused gateway\n'
        command = '''
EDGE_CHAIN_NAME=gateway; L1_RPC_URL=http://fixture.invalid
gl_l1_broadcast_preflight() { :; }
refresh_l1_admin_wallet_funding() { :; }
gl_zkstack_pty() {
  python3 - "$@" <<'PY'
import json,os,sys
with open(os.environ['DEPOSIT_ARGS_LOG'], 'x') as output:
    json.dump(sys.argv[1:], output)
PY
}
''' + body
        return self.run_shell(command, DEPOSIT_ARGS_LOG=str(self.root / "deposit-args.json"), **overrides)

    def test_pause_and_unpause_forward_authenticated_null_governor_account(self):
        self.set_ecosystem_administrators()
        for action in ("pause", "unpause"):
            with self.subTest(action=action):
                log = self.root / "deposit-args.json"
                if log.exists():
                    log.unlink()
                result = self.deposit_command(action)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(log.read_text()),
                    ["zkstack", "chain", action + "-deposits", "--chain", "gateway",
                     "--l1-rpc-url", "http://fixture.invalid", "--additional-args", "--account",
                     "--additional-args", "v32-admin", "--additional-args", "--password-file",
                     "--additional-args", str(self.password.resolve()), "--additional-args", "--sender",
                     "--additional-args", ADMIN, "-v"])

    def test_pause_and_unpause_reject_account_mismatch_before_pty(self):
        self.set_ecosystem_administrators()
        for action in ("pause", "unpause"):
            with self.subTest(action=action):
                result = self.deposit_command(action, CAST_ACCOUNT_ADDRESS="0x" + "33" * 20)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("address/account mismatch", result.stderr)
                self.assertFalse((self.root / "deposit-args.json").exists())

    def test_pause_and_unpause_generated_governor_preserve_empty_selector(self):
        self.write_wallet({"governor": self.generated(), "deployer": self.generated()})
        self.set_ecosystem_administrators(self.generated(), self.generated())
        for action in ("pause", "unpause"):
            with self.subTest(action=action):
                log = self.root / "deposit-args.json"
                if log.exists():
                    log.unlink()
                result = self.deposit_command(action)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(log.read_text()),
                    ["zkstack", "chain", action + "-deposits", "--chain", "gateway",
                     "--l1-rpc-url", "http://fixture.invalid", "-v"])

    def test_pause_and_unpause_do_not_allow_raw_key_backend(self):
        self.set_ecosystem_administrators()
        for action in ("pause", "unpause"):
            with self.subTest(action=action):
                result = self.deposit_command(action, EDGE_GATEWAY_GOVERNOR_SIGNER="private-key")
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "deposit-args.json").exists())

    def test_pause_fresh_group_refuses_inline_role_overridden_by_external_account(self):
        self.set_ecosystem_administrators(self.generated(), self.external())
        result = self.deposit_command("pause")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match every requested", result.stderr)
        self.assertFalse((self.root / "deposit-args.json").exists())

    def test_protected_account_password_files_reject_permissions_links_and_absence(self):
        for path in (self.account, self.password):
            with self.subTest(path=path.name):
                path.chmod(0o644)
                result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer')
                self.assertNotEqual(result.returncode, 0)
                path.chmod(0o600)
                backup = path.with_name(path.name + ".backup")
                path.rename(backup)
                path.symlink_to(backup)
                result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer')
                self.assertNotEqual(result.returncode, 0)
                path.unlink()
                backup.rename(path)
        result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer', DEPLOYER_PASSWORD_FILE="")
        self.assertNotEqual(result.returncode, 0)

    def test_validator_directory_and_ambient_wallet_selectors_cannot_redirect_account(self):
        result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer',
                                FOUNDRY_KEYSTORE_DIR=str(self.root / "other-keystores"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("default Foundry", result.stderr)
        for name in ("PRIVATE_KEY", "ETH_PRIVATE_KEY", "ETH_KEYSTORE", "ETH_KEYSTORE_ACCOUNT", "ETH_PASSWORD", "MNEMONIC"):
            with self.subTest(name=name):
                result = self.run_shell('gl_authenticate_chain_wallet_roles gateway deployer', **{name: "fixture"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("conflicting ambient", result.stderr)

    def test_ecosystem_scope_never_uses_default_chain_identity(self):
        ecosystem = self.gateway / "configs/wallets.yaml"
        ecosystem.parent.mkdir(parents=True)
        ecosystem.write_text(yaml.safe_dump({"deployer": self.external()}))
        ecosystem.chmod(0o600)
        self.write_wallet({"deployer": self.generated()})
        result = self.run_shell('gl_authenticate_chain_wallet_roles --print-addresses --ecosystem-only deployer')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), ADMIN)

    def test_read_only_governor_context_supports_external_and_generated_edge(self):
        bridgehub = "0x" + "44" * 20
        gateway_diamond = "0x" + "55" * 20
        root_contracts = self.gateway / "configs/contracts.yaml"
        root_contracts.parent.mkdir(parents=True)
        root_contracts.write_text(yaml.safe_dump({"core_ecosystem_contracts": {"bridgehub_proxy_addr": bridgehub}}))
        for name, chain_id in (("gateway", 57001), ("zksys", 57057)):
            directory = self.gateway / "chains" / name
            (directory / "configs").mkdir(parents=True, exist_ok=True)
            (directory / "ZkStack.yaml").write_text(yaml.safe_dump({"chain_id": chain_id}))
            (directory / "configs/contracts.yaml").write_text(yaml.safe_dump({
                "ecosystem_contracts": {"bridgehub_proxy_addr": bridgehub},
                "l1": {"diamond_proxy_addr": gateway_diamond if name == "gateway" else "0x" + "00" * 20},
            }))
        edge_wallet = self.gateway / "chains/zksys/configs/wallets.yaml"
        for entry, expected in ((self.external(), f"{ADMIN}|{ADMIN}|true"),
                                (self.generated(), f"{ADMIN}|{GENERATED}|false")):
            with self.subTest(expected=expected):
                edge_wallet.write_text(yaml.safe_dump({"governor": entry}))
                edge_wallet.chmod(0o600)
                result = self.run_shell('gl_edge_governor_reuse_context')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(result.stdout.startswith(expected + "|"), result.stdout)

    def test_wrapper_selectors_follow_all_normal_flags_and_precede_broadcasts(self):
        launch = ROOT / "scripts/gateway-launch"
        for script, calls in (
            ("gateway-deploy-l1.sh", ("zkstack ecosystem init \\", "zkstack ecosystem init-core-contracts \\")),
            ("gateway-chain-init.sh", ("zkstack chain init \\",)),
            ("gateway-convert-settlement.sh", ("zkstack chain gateway create-tx-filterer \\", "zkstack chain gateway convert-to-gateway \\")),
        ):
            source = (launch / script).read_text()
            for call in calls:
                with self.subTest(script=script, call=call):
                    start = source.index(call)
                    self.assertLess(source.index("gl_prepare_zkstack_admin_wallet_args "), start)
                    end = source.index('"${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]}"', start)
                    self.assertGreater(end, source.index("--l1-rpc-url ", start))
        edge = (launch / "edge-chain-create-init.sh").read_text()
        self.assertLess(edge.index('gl_prepare_zkstack_admin_wallet_args "${EDGE_CHAIN_NAME}"'), edge.index('if [ "${SKIP_FUND}"'))
        self.assertIn('init_args+=(${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]+"${GL_ZKSTACK_ADMIN_WALLET_ARGS[@]}"})', edge)


if __name__ == "__main__":
    unittest.main()
