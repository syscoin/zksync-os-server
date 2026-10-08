"""SYSCOIN: Pinned handler argument-flow regression; no CLI, Forge, keys or RPC."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "scripts/fixtures/zkstack-manage-deposits.upstream.rs"
TARGET = "zkstack_cli/crates/zkstack/src/commands/chain/manage_deposits.rs"


def patched_handler():
    # Exact upstream d1f file, not an invented public-handler approximation.
    original = FIXTURE.read_bytes()
    if hashlib.sha256(original).hexdigest() != "ff96bb908338346e48bd1e8b1b7ac57ef4e021859692c155b3e525ed8e4fa56f":
        raise AssertionError("Pinned upstream deposit-handler fixture differs")
    envelope = (ROOT / "scripts/patches/zksync-era-syscoin.patch").read_text()
    marker = f"diff --git a/{TARGET} b/{TARGET}\n"
    section = envelope.split(marker, 1)[1].split("\ndiff --git ", 1)[0]
    lines = original.decode().splitlines(keepends=True)
    result, cursor = [], 0
    for header, body in re.findall(r"(@@ -\d+(?:,\d+)? \+\d+(?:,\d+)? @@[^\n]*)\n(.*?)(?=\n@@ |\Z)", section, re.S):
        start, count = re.match(r"@@ -(\d+)(?:,(\d+))?", header).groups()
        count = int(count) if count is not None else 1
        position = int(start) - (1 if count else 0)
        removed = [line[1:] + "\n" for line in body.splitlines() if line.startswith("-")]
        added = [line[1:] + "\n" for line in body.splitlines() if line.startswith("+")]
        if cursor > position or len(removed) != count or lines[position:position + count] != removed:
            raise AssertionError("Exact upstream patch hunk mismatch")
        result.extend(lines[cursor:position]); result.extend(added); cursor = position + count
    result.extend(lines[cursor:])
    current = "".join(result)
    if hashlib.sha256(current.encode()).hexdigest() != "9d5c29e274c9ede98b2db8631198a8f8854d6361e7eb6f76e9d7b088ca4d5c0d":
        raise AssertionError("Deposit-handler postimage differs")
    return original.decode(), current


# The production async run() body is compiled verbatim. These surrounding types
# are explicit fixtures: they record argument flow, not CLI/onchain qualification.
HARNESS = r'''
#![allow(dead_code, private_interfaces)]
use std::{future::Future, path::{Path, PathBuf}, sync::Arc, task::{Context, Poll, Wake}};
mod anyhow { pub type Result<T> = std::result::Result<T, &'static str>; }
struct Shell;
#[derive(Default)] struct ForgeScriptArgs { additional_args: Vec<String> }
enum ManageDepositsOption { PauseDeposits, UnpauseDeposits }
struct ManageDepositsArgs { forge_args: ForgeScriptArgs, only_save_calldata: bool, l1_rpc_url: Option<String> }
struct Wallet { private_key: Option<u8> }
enum AdminScriptMode { OnlySave, Broadcast(Wallet) }
#[derive(Clone, Copy)] struct ChainId(u64);
impl ChainId { fn as_u64(self) -> u64 { self.0 } }
struct ZkStackConfig;
struct ChainConfig { chain_id: ChainId }
struct EcosystemContracts { bridgehub_proxy_addr: u64 }
struct ContractsConfig { ecosystem_contracts: EcosystemContracts }
struct WalletsConfig { governor: Wallet }
struct SecretsConfig;
impl ZkStackConfig { fn current_chain(_: &Shell) -> anyhow::Result<ChainConfig> { Ok(ChainConfig { chain_id: ChainId(57057) }) } }
impl ChainConfig {
 fn get_contracts_config(&self) -> anyhow::Result<ContractsConfig> { Ok(ContractsConfig { ecosystem_contracts: EcosystemContracts { bridgehub_proxy_addr: 1 } }) }
 fn path_to_foundry_scripts(&self) -> PathBuf { PathBuf::from("fixture-only") }
 fn get_wallets_config(&self) -> anyhow::Result<WalletsConfig> { Ok(WalletsConfig { governor: Wallet { private_key: None } }) }
 async fn get_secrets_config(&self) -> anyhow::Result<SecretsConfig> { Ok(SecretsConfig) }
}
impl SecretsConfig { fn l1_rpc_url(&self) -> anyhow::Result<String> { Ok("fixture-only".into()) } }
fn display_admin_script_output(_: ()) {}
fn selected(args: &ForgeScriptArgs, mode: AdminScriptMode) -> anyhow::Result<()> {
 if let AdminScriptMode::Broadcast(wallet) = mode {
  if wallet.private_key.is_none() && args.additional_args != ["--account", "fixture-account", "--password-file", "fixture-file", "--sender", "fixture-public-owner"] {
   return Err("Governor private key is not set");
  }
 }
 Ok(())
}
async fn pause_deposits_before_initiating_migration(_: &Shell, args: &ForgeScriptArgs, _: &Path, mode: AdminScriptMode, _: u64, _: u64, _: String) -> anyhow::Result<()> { selected(args, mode) }
async fn unpause_deposits(_: &Shell, args: &ForgeScriptArgs, _: &Path, mode: AdminScriptMode, _: u64, _: u64, _: String) -> anyhow::Result<()> { selected(args, mode) }
struct Noop;
impl Wake for Noop { fn wake(self: Arc<Self>) {} }
fn block_on<T>(future: impl Future<Output=T>) -> T {
 let waker = Arc::new(Noop).into(); let mut cx = Context::from_waker(&waker); let mut future = Box::pin(future);
 match future.as_mut().poll(&mut cx) { Poll::Ready(result) => result, Poll::Pending => panic!("Fixture future unexpectedly pending") }
}
fn main() {
 for option in [ManageDepositsOption::PauseDeposits, ManageDepositsOption::UnpauseDeposits] {
  let args = ManageDepositsArgs { forge_args: ForgeScriptArgs { additional_args: ["--account", "fixture-account", "--password-file", "fixture-file", "--sender", "fixture-public-owner"].iter().map(|s| s.to_string()).collect() }, only_save_calldata: false, l1_rpc_url: Some("fixture-only".into()) };
  assert_eq!(block_on(run(args, &Shell, option)), EXPECTED);
 }
}
'''


class DepositCliArgumentTests(unittest.TestCase):
    def test_exact_upstream_and_new_handler_preserve_both_selectors(self):
        old, current = patched_handler()
        self.assertEqual(old.count("&Default::default(),"), 2)
        self.assertNotIn("&Default::default(),", current)
        self.assertEqual(current.count("&_args.forge_args,"), 2)
        self.assertIn("// SYSCOIN:", current)

    def test_verbatim_async_handler_old_refuses_new_accepts_null_governor(self):
        if not shutil.which("rustc"):
            self.skipTest("rustc unavailable for isolated argument-flow fixture")
        available = subprocess.run(["rustc", "--version"], capture_output=True, timeout=15)
        if available.returncode:
            self.skipTest("Configured rustc unavailable; source postimage is checked separately")
        old, current = patched_handler()
        with tempfile.TemporaryDirectory(prefix="deposit-handler-flow-") as temporary:
            for label, source, expected in [("old", old, 'Err("Governor private key is not set")'), ("new", current, "Ok(())")]:
                unit = Path(temporary) / f"{label}.rs"; binary = Path(temporary) / label
                unit.write_text(HARNESS.replace("EXPECTED", expected) + "\npub async fn run(" + source.split("pub async fn run(", 1)[1])
                built = subprocess.run(["rustc", "--edition=2021", str(unit), "-o", str(binary)], capture_output=True, text=True, timeout=30)
                self.assertEqual(built.returncode, 0, built.stderr)
                result = subprocess.run([str(binary)], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
