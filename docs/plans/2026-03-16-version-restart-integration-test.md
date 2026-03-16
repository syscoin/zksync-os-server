# Version Restart Integration Test Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Rust integration tests that download previous patch / previous minor server releases and validate restart behavior on one shared RocksDB directory by settling 3 L1-finalized batches per phase.

**Architecture:** Build a dedicated `integration-tests` harness for externally spawned server binaries. The harness will reuse the existing Anvil / provider utilities, download cached release binaries from GitHub Releases, drive a small amount of L2 traffic via the standard test wallet, and assert L1 finalization progress across restarts.

**Tech Stack:** Rust, `tokio`, `reqwest`, `tar`, `flate2`, existing `integration-tests` providers, GitHub Releases

---

### Task 1: Add the failing version-restart test skeleton

**Files:**
- Create: `integration-tests/src/version_restart/mod.rs`
- Create: `integration-tests/tests/version_restart.rs`
- Modify: `integration-tests/src/lib.rs`

**Step 1: Write the failing tests**

Add two ignored-or-failing integration tests:
- `restart_from_previous_patch_settles_three_batches`
- `restart_from_previous_minor_is_not_operational`

Both should call placeholder helpers from a new `version_restart` module.

**Step 2: Run the tests to verify RED**

Run: `cargo test -p zksync_os_integration_tests --test version_restart -- --nocapture`
Expected: compile failure or test failure due to missing harness implementation.

**Step 3: Expose the module minimally**

Wire the new module through `integration-tests/src/lib.rs` so the tests compile once the harness is added.

**Step 4: Re-run and confirm still RED**

Run the targeted test command again and confirm failure remains in the new scenario area.

### Task 2: Implement release selection and binary download

**Files:**
- Modify: `integration-tests/src/version_restart/mod.rs`
- Reference: `integration-tests/src/lib.rs`
- Reference: `Cargo.toml`

**Step 1: Write failing unit-level flow in the harness**

Add helper APIs for:
- current workspace version parsing
- selecting previous patch and previous minor tags
- downloading and extracting release binaries

Leave at least one path unimplemented so the tests remain red.

**Step 2: Run targeted test command**

Run the version-restart tests and confirm failure now comes from missing release/binary behavior.

**Step 3: Implement the minimal release helpers**

Reuse the integration-test download pattern:
- cache archives and extracted binaries locally
- use `GITHUB_TOKEN` if available
- support current test platform asset naming

**Step 4: Re-run targeted tests**

Confirm failures move forward into process startup / scenario execution instead of download plumbing.

### Task 3: Implement external server runner and settlement driver

**Files:**
- Modify: `integration-tests/src/version_restart/mod.rs`
- Reference: `integration-tests/src/lib.rs`
- Reference: `integration-tests/src/provider.rs`
- Reference: `integration-tests/src/assert_traits.rs`

**Step 1: Write failing process orchestration**

Add:
- external server process handle
- shared RocksDB tempdir management
- RPC readiness checks
- simple transaction sender using the existing rich wallet
- helper to wait until 3 finalized batches are added

Keep one scenario assertion failing until the full flow is wired.

**Step 2: Run the targeted tests and inspect failure**

Run the version-restart tests and verify the failure is now in the expected runtime behavior.

**Step 3: Implement minimal passing runtime behavior**

Complete the orchestration so:
- previous patch -> current settles 3 batches per phase
- previous minor -> current is classified as non-operational if it exits, never becomes ready, or cannot settle 3 batches in time

**Step 4: Re-run targeted tests**

Confirm both tests reach their expected outcomes locally, or capture the concrete failure mode if they do not.

### Task 4: Remove or simplify CI shell scenario

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify or delete: `.github/scripts/run-version-upgrade-test.sh`

**Step 1: Update CI to call the Rust integration tests**

Replace the shell-driven job with a targeted `cargo test` / `cargo nextest` invocation for the new test.

**Step 2: Run lightweight validation**

Run syntax / diff validation on the updated workflow and script state.

**Step 3: Remove obsolete shell code if unused**

Delete the old script or leave a minimal shim only if still needed elsewhere.

**Step 4: Re-run verification**

Confirm the repo is consistent and no stale references remain.

### Task 5: Final verification

**Files:**
- Modify: any touched files as needed

**Step 1: Run the focused integration tests**

Run the version-restart test target end-to-end.

**Step 2: Run repo-level hygiene checks**

Run `git diff --check` and any lightweight syntax validation still applicable.

**Step 3: Fix issues**

Apply the minimum corrections required by the verification results.

**Step 4: Summarize remaining risk**

Document any unresolved flakiness or environment assumptions, especially release download availability and runtime differences across historical binaries.
