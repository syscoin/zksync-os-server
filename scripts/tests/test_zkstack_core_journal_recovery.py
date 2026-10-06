"""SYSCOIN: Reachability and fail-closed boundaries for opt-in core recovery."""

from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
PATCH = REPO_ROOT / "scripts/patches/zksync-era-syscoin.patch"


def added(path: str) -> str:
    section = PATCH.read_text(encoding="utf-8").split(
        f"diff --git a/{path} b/{path}\n", 1
    )[1].split("\ndiff --git ", 1)[0]
    return "\n".join(
        line[1:] for line in section.splitlines()
        if line.startswith("+") and not line.startswith("+++")
    )


class CoreJournalOnlyTests(unittest.TestCase):
    def test_dispatch_preflight_and_both_source_writers_exclude_strict_mode(self) -> None:
        dispatch = added("zkstack_cli/crates/zkstack/src/commands/ecosystem/mod.rs")
        self.assertIn("core_args.validate_core_journal_only_options()?;", dispatch)
        self.assertEqual(dispatch.count("!args.core_journal_only"), 2)
        options = added("zkstack_cli/crates/zkstack/src/commands/ecosystem/args/init.rs")
        for required in (
            "!self.forge_args.resume", "!self.common.zksync_os",
            "self.common.update_submodules", "!self.common.skip_contract_compilation_override",
            "self.deploy_erc20 != Some(false)", "self.ownership_only", "self.dev",
            "core_journal_only: false",
        ):
            self.assertIn(required, options)

    def test_core_recovery_never_reaches_fresh_input_erc20_or_ctm_deployment(self) -> None:
        source = added("zkstack_cli/crates/zkstack/src/commands/ecosystem/init_core_contracts.rs")
        start = source.index("async fn resume_confirmed_core_journal(")
        end = source.index("\n}", start)
        recovery = source[start:end]
        for forbidden in (
            "deploy_l1_core_contracts(", "deploy_erc20(", "complete_ctm_owner_handoffs(",
            "create_initial_deployments_config(", "deploy_config.save(", "forge build",
        ):
            self.assertNotIn(forbidden, recovery)
        for required in (
            "params.input(&foundry_path)", "params.output(&foundry_path)",
            "ensure_confirmed_core_journal(&journal)?;",
            ".with_core_journal_only_resume()", "complete_core_owner_handoffs(",
            "core input or output bytes changed during journal-only recovery",
        ):
            self.assertIn(required, recovery)
        branch = source.split("if final_ecosystem_args.core_journal_only {", 1)[1].split("\n    }", 1)[0]
        self.assertIn("persist_recovered_core_config_new(&contracts, &ecosystem_config.config)?;", branch)
        self.assertIn("return Ok(());", branch)

    def test_strict_forge_returns_every_resume_error_before_fresh_fallback(self) -> None:
        source = added("zkstack_cli/crates/common/src/forge.rs")
        self.assertIn("core_journal_only_resume: false", source)
        self.assertIn("if self.core_journal_only_resume || !res.resume_not_successful_because_has_not_began()", source)
        self.assertIn("missing_core_journal_error_never_falls_back_to_fresh_broadcast", source)
        self.assertIn("assert_eq!(calls.lines().count(), 1)", source)
        self.assertIn("core_journal_only_rejects_noncore_or_nonresume_before_spawning", source)
        for required in (
            '.env("FOUNDRY_ROOT", &self.base_path)',
            '.env("FOUNDRY_PROFILE", "default")',
            '.env("FOUNDRY_BROADCAST", self.base_path.join("broadcast"))',
            "forbids Forge context override {flag}",
            '"--chain"', '"--tc"', '"--config-path"', '"-f"',
            'flag.len() == 2 && arg.starts_with(flag)',
        ):
            self.assertIn(required, source)

    def test_journal_local_completeness_is_not_mislabeled_as_provenance(self) -> None:
        source = added("zkstack_cli/crates/zkstack/src/commands/ecosystem/init_core_contracts.rs")
        self.assertIn("complete local journal is necessary, not authoritative provenance", source)
        for required in (
            "transactions.is_empty()", "transactions.len() != receipts.len()",
            "!pending.is_empty()", "!expected.insert(hash)", "!expected.remove(&hash)",
            'receipt["status"].as_str() != Some("0x1")',
            "missing_empty_or_symlink_core_inputs_are_never_generated",
        ):
            self.assertIn(required, source)

    def test_final_core_config_publication_is_exclusive_and_canonical(self) -> None:
        source = added("zkstack_cli/crates/zkstack/src/commands/ecosystem/init_core_contracts.rs")
        start = source.index("fn persist_recovered_core_config_new(")
        writer = source[start:source.index("\n}", start)]
        self.assertIn("CoreContractsConfig::get_path_with_base_path(base_path)", writer)
        self.assertIn("serde_yaml::to_string(contracts)?", writer)
        self.assertLess(writer.index("serde_yaml::to_string"), writer.index(".open(path)"))
        self.assertIn("options.write(true).create_new(true)", writer)
        self.assertIn("options.mode(0o600)", writer)
        self.assertIn("file.write_all(content.as_bytes())", writer)
        self.assertIn("file.sync_all()", writer)
        self.assertNotIn(".save_with_base_path", writer)
        for regression in (
            "core_config_save_is_create_only_and_matches_canonical_yaml_bytes",
            "concurrent_regular_empty_or_malformed_core_config_is_preserved",
            "concurrent_core_config_symlink_or_broken_link_is_preserved",
        ):
            self.assertIn(regression, source)
        forge = added("zkstack_cli/crates/common/src/forge.rs")
        self.assertIn("crate::config::init_global_config(crate::config::GlobalConfig", forge)


if __name__ == "__main__":
    unittest.main()
