# Version Upgrade Test Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a GitHub Actions workflow that validates latest -> previous minor -> latest restarts against one shared RocksDB database using the published previous-minor binary.

**Architecture:** Introduce a dedicated workflow that builds the current server binary and `loadbase`, resolves and downloads the previous minor release artifact, and runs the three-phase restart scenario in one inline bash step. Reuse the existing local-chain fixture and use `loadbase` to generate transactions after each restart.

**Tech Stack:** GitHub Actions YAML, bash, `cargo build`, `gh`, `jq`, `tar`, `anvil`, `loadbase`

---

### Task 1: Add the workflow skeleton

**Files:**
- Create: `.github/workflows/version-upgrade-test.yml`
- Reference: `.github/workflows/ci.yml`
- Reference: `.github/actions/runner-setup/action.yaml`

**Step 1: Write the failing workflow**

Create a new workflow with:
- `pull_request` and `workflow_dispatch` triggers
- `contents: read` permission
- one Linux job using the standard runner setup action
- a release build step for `zksync-os-server`
- a placeholder compatibility step that exits non-zero

**Step 2: Verify the failure shape**

Run a YAML parse / inspection command locally and confirm the workflow file exists and the placeholder step would fail in CI.

**Step 3: Write the minimal workflow structure**

Replace the placeholder with env/default sections needed by the compatibility scenario, artifact upload on failure, and the shell step that will hold the orchestration logic.

**Step 4: Re-check syntax**

Run a local YAML validation command or at least a structured file read to confirm the workflow remains well-formed.

### Task 2: Implement version resolution and release download

**Files:**
- Modify: `.github/workflows/version-upgrade-test.yml`
- Reference: `Cargo.toml`
- Reference: `.github/workflows/release-bins.yml`

**Step 1: Write the failing logic**

Add shell code that:
- reads the current version from `Cargo.toml`
- computes the previous minor line
- intentionally exits if no tag is found yet

**Step 2: Verify failure logic conceptually**

Inspect the shell carefully against existing release artifact naming and GitHub CLI output assumptions.

**Step 3: Write minimal passing resolution**

Implement `gh release list` + `jq` filtering to choose the highest `v<major>.<minor>.<patch>` tag and download the Linux tarball for that release.

**Step 4: Verify the command wiring**

Check the final shell snippet for quoting, semver filtering, and artifact path correctness.

### Task 3: Implement the three-phase restart scenario

**Files:**
- Modify: `.github/workflows/version-upgrade-test.yml`
- Reference: `docs/src/setup/local_run.md`
- Reference: `local-chains/v30.2/default/config.yaml`

**Step 1: Write the failing runtime assertions**

Add shell functions for:
- waiting for RPC
- running `loadbase`
- checking block progress
- starting and stopping the server

Initially leave one assertion impossible so the logic is visibly red during drafting.

**Step 2: Verify the failure reasoning**

Inspect the runtime flow to ensure it would catch a server that fails to reopen the shared DB or stops making progress.

**Step 3: Write minimal passing orchestration**

Start `anvil`, create one shared RocksDB directory, then run:
- current binary + tx batch
- previous minor binary + tx batch
- current binary + tx batch

Store separate logs for each phase and stop processes cleanly between phases.

**Step 4: Verify flow coherence**

Read the completed shell step to confirm the RocksDB path is stable, the ports match the fixture, and cleanup is robust.

### Task 4: Verify the workflow locally as far as feasible

**Files:**
- Modify: `.github/workflows/version-upgrade-test.yml`

**Step 1: Run local verification**

Run feasible local checks such as:
- `python3 - <<'PY'` YAML parse if available
- `git diff --check`

**Step 2: Fix any issues**

Apply minimal corrections required by the verification output.

**Step 3: Re-run verification**

Repeat the checks until clean.

**Step 4: Summarize remaining CI-only risk**

Document any assumptions that can only be fully validated inside GitHub Actions, especially GitHub release discovery and artifact download.
