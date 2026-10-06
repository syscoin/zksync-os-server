"""SYSCOIN: Test the CLI-only self-pending direct ownership cleanup boundary."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
PATCH_PATH = REPO_ROOT / "scripts/patches/zksync-era-syscoin.patch"
ADMIN_PATH = "zkstack_cli/crates/zkstack/src/admin_functions.rs"


def added_admin_source() -> str:
    section = PATCH_PATH.read_text(encoding="utf-8").split(
        f"diff --git a/{ADMIN_PATH} b/{ADMIN_PATH}\n", 1
    )[1].split("\ndiff --git ", 1)[0]
    return "\n".join(
        line[1:]
        for line in section.splitlines()
        if line.startswith("+") and not line.startswith("+++")
    )


def function_block(source: str, name: str, indent: str = "") -> str:
    start = source.index(f"fn {name}(")
    end = source.index(f"\n{indent}}}", start) + len(indent) + 2
    return textwrap.dedent(source[start:end])


class DirectOwnerAcceptanceTests(unittest.TestCase):
    def test_direct_path_preserves_authenticated_conditional_call_and_postcheck(self) -> None:
        source = added_admin_source()
        direct = function_block(source, "accept_direct_owner")
        self.assertIn("direct_owner_acceptance_required(", direct)
        self.assertIn('"governanceAcceptOwnerConditional"', direct)
        self.assertIn("(governor.address, target_address)", direct)
        self.assertIn("WalletOwner::Governor", direct)
        self.assertIn("execute_owner_calldata(", direct)
        self.assertIn(
            ".await?;\n    ensure_owner_handoff_complete(target_address, governor.address, &l1_rpc_url).await",
            direct,
        )
        # SYSCOIN: Cleanup is direct-only; neither contract-owned acceptance
        # nor deployment transfer initiation admits this additional pre-state.
        self.assertNotIn(
            "direct_owner_acceptance_required(",
            function_block(source, "accept_owner_via_chain_admin"),
        )
        self.assertIn(
            "ensure_owner_state_complete(current_owner, pending_owner, expected_owner)",
            function_block(source, "ensure_owner_handoff_complete"),
        )

    def test_embedded_rust_state_regressions_compile_and_run(self) -> None:
        rustc = shutil.which("rustc")
        if rustc is None:
            self.skipTest("rustc is not installed")
        version = subprocess.run(
            [rustc, "--version"], capture_output=True, text=True, check=False
        )
        if version.returncode != 0:
            self.skipTest("the selected rustc toolchain is unavailable")

        source = added_admin_source()
        functions = "\n\n".join(
            function_block(source, name)
            for name in (
                "owner_acceptance_required",
                "direct_owner_acceptance_required",
                "owner_transfer_required",
                "ensure_owner_state_complete",
            )
        )
        tests = "\n\n".join(
            "#[test]\n" + function_block(source, name, "    ")
            for name in (
                "completed_owner_acceptance_is_a_noop_only_in_the_clean_state",
                "pending_owner_must_be_the_intended_governor",
                "direct_same_owner_pending_acceptance_is_not_a_noop",
                "direct_same_owner_pending_acceptance_rejects_an_aliased_governor",
                "successful_acceptance_receipt_still_requires_clean_owner_post_state",
                "deployer_transfer_only_runs_from_the_one_pristine_state",
            )
        )
        # SYSCOIN: Compile the actual patched pure predicates and actual Rust
        # regression bodies, not a Python reimplementation. These minimal
        # stand-ins preserve exact 20-byte address equality and Result errors;
        # this is not a substitute for building the whole CLI/provider graph.
        prelude = textwrap.dedent(
            """
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            struct Address([u8; 20]);
            impl Address {
                fn zero() -> Self { Self([0; 20]) }
                fn from_low_u64_be(value: u64) -> Self {
                    let mut bytes = [0; 20];
                    bytes[12..].copy_from_slice(&value.to_be_bytes());
                    Self(bytes)
                }
            }
            impl From<[u8; 20]> for Address {
                fn from(value: [u8; 20]) -> Self { Self(value) }
            }
            mod anyhow { pub type Result<T> = std::result::Result<T, String>; }
            macro_rules! bail { ($message:expr) => { return Err($message.to_owned()) }; }
            """
        )
        with tempfile.TemporaryDirectory(prefix="zkstack-direct-owner-") as temporary_dir:
            root = Path(temporary_dir)
            probe = root / "probe.rs"
            executable = root / "probe"
            probe.write_text(prelude + functions + "\n" + tests, encoding="utf-8")
            compiled = subprocess.run(
                [rustc, "--edition=2021", "--test", str(probe), "-o", str(executable)],
                capture_output=True,
                text=True,
                check=False,
                env=os.environ.copy(),
            )
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            executed = subprocess.run(
                [str(executable)], capture_output=True, text=True, check=False
            )
            self.assertEqual(executed.returncode, 0, executed.stdout + executed.stderr)
            self.assertIn("6 passed; 0 failed", executed.stdout)


if __name__ == "__main__":
    unittest.main()
