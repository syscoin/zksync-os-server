# Coverage Campaign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Increase workspace coverage in measured batches by adding high-value tests and stabilizing the coverage run.

**Architecture:** Work in small coverage slices. First stabilize the coverage profile, then add tests to the largest high-impact uncovered modules using each crate's current testing style. After every slice, rerun targeted tests and coverage, then re-rank the next slice from the updated report.

**Tech Stack:** Rust, cargo-nextest, cargo-llvm-cov, inline crate tests, tokio async tests

---

### Task 1: Stabilize the coverage profile

**Files:**
- Modify: `.config/nextest.toml`
- Test: `cargo llvm-cov --ignore-run-fail --lcov --output-path lcov.info nextest --workspace --release --profile coverage`

**Step 1: Narrow the coverage profile filter**

- Extend `[profile.coverage].default-filter` to exclude `test_interop_l2_to_l1_message_verification`.
- Keep the default and fast profiles unchanged.

**Step 2: Run the coverage command**

Run:

```bash
cargo llvm-cov --ignore-run-fail --lcov --output-path lcov.info nextest --workspace --release --profile coverage
```

Expected:

- `lcov.info` is generated.
- The interop coverage timeout no longer dominates the run.

### Task 2: Raise coverage in `lib/rpc`

**Files:**
- Modify: `lib/rpc/src/eth_impl.rs`
- Modify: `lib/rpc/src/ots_impl.rs`
- Modify: `lib/rpc/src/js_tracer/tracer.rs`
- Modify: `lib/rpc/src/js_tracer/host.rs`
- Modify: `lib/rpc/src/js_tracer/types.rs`
- Modify: `lib/rpc/src/monitoring_middleware.rs`
- Modify: `lib/rpc/src/zks_impl.rs`
- Test: `cargo test -p zksync_os_rpc --release`

**Step 1: Expand existing inline tests in `eth_impl`**

- Cover `block_by_id_impl` full vs hash-only responses.
- Cover missing transaction / metadata paths.
- Cover mempool-first lookup behavior in transaction retrieval helpers.
- Cover `None` behavior for unknown blocks and indices.

**Step 2: Add focused tests for `ots_impl`**

- Cover pagination boundaries.
- Cover empty-page behavior.
- Cover filtering / truncation edge cases.

**Step 3: Add unit tests for JS tracer helper logic**

- Cover request / result shaping helpers.
- Cover unsupported or malformed cases.
- Cover host-side conversion / defaulting branches.

**Step 4: Add tests for monitoring and ZKS helpers**

- Cover middleware size / method accounting branches.
- Cover ZKS namespace helper error mapping and optional-field behavior.

**Step 5: Run crate tests**

Run:

```bash
cargo test -p zksync_os_rpc --release
```

Expected:

- New tests pass.
- No existing RPC tests regress.

### Task 3: Raise coverage in `lib/sequencer` and `lib/types`

**Files:**
- Modify: `lib/sequencer/src/execution/block_executor.rs`
- Modify: `lib/sequencer/src/execution/block_context_provider.rs`
- Modify: `lib/sequencer/src/execution/utils.rs`
- Modify: `lib/types/src/transaction/l1.rs`
- Modify: `lib/types/src/transaction/l2.rs`
- Modify: `lib/types/src/transaction/system/mod.rs`
- Modify: `lib/types/src/receipt/envelope.rs`
- Test: `cargo test -p zksync_os_sequencer -p zksync_os_types --release`

**Step 1: Add sequencer branch coverage**

- Cover invalid-tx skip handling.
- Cover descendant skipping logic.
- Cover upgrade / context edge cases in block context provider.

**Step 2: Add transaction / receipt serialization tests**

- Cover encode / decode roundtrips beyond current happy paths.
- Cover optional-field and variant behavior.
- Cover malformed or unsupported inputs where code already handles them.

**Step 3: Run targeted tests**

Run:

```bash
cargo test -p zksync_os_sequencer -p zksync_os_types --release
```

Expected:

- The new invariants are protected by tests.

### Task 4: Raise coverage in `node/bin`

**Files:**
- Modify: `node/bin/src/main.rs`
- Modify: `node/bin/src/prover_api/prover_server/v1/handlers.rs`
- Modify: `node/bin/src/prover_api/fri_job_manager.rs`
- Modify: `node/bin/src/batcher/mod.rs`
- Modify: `node/bin/src/config/{mod.rs,util.rs}`
- Test: `cargo test -p zksync_os_server --release`

**Step 1: Add config and helper tests**

- Cover parsing / defaulting helpers.
- Cover branches that build runtime components from config.

**Step 2: Add prover handler tests**

- Cover request validation branches.
- Cover success and failure responses with local fakes.

**Step 3: Add batcher decision tests**

- Cover already-executed block handling.
- Cover verification / skip decisions.

**Step 4: Run targeted tests**

Run:

```bash
cargo test -p zksync_os_server --release
```

Expected:

- Handler behavior is covered without requiring a full server launch.

### Task 5: Re-measure and re-rank

**Files:**
- Modify: `lcov.info` (generated)
- Reference: `docs/plans/2026-03-11-coverage-campaign-design.md`

**Step 1: Re-run coverage**

Run:

```bash
cargo llvm-cov --ignore-run-fail --lcov --output-path lcov.info nextest --workspace --release --profile coverage
```

**Step 2: Re-rank gaps**

- Aggregate by crate and file.
- Pick the next slice from:
  - `lib/l1_watcher`
  - `lib/base_token_adjuster`
  - `lib/operator_signer`
  - `lib/state`
  - `lib/revm_consistency_checker`
  - `lib/reth_compat`

**Step 3: Report bugs**

- If a new test exposes a production bug, stop and report it with file / line references before continuing.

### Task 6: Iterate until diminishing returns

**Files:**
- Modify: targeted crate test files
- Test: crate-specific `cargo test` plus full coverage rerun

**Step 1: Keep slice size small**

- One crate or one coherent subsystem at a time.

**Step 2: Keep tests meaningful**

- Prefer behavior / invariant coverage over synthetic line chasing.

**Step 3: Stop when the next slice requires design discussion**

- If progress shifts from “add targeted tests” to “large refactor for testability”, pause and re-evaluate with the user.
