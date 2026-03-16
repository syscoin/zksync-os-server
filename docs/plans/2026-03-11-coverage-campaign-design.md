# Coverage Campaign Design

**Goal:** Raise workspace coverage meaningfully by adding useful tests in the existing server style, while keeping the coverage run stable enough to measure progress after each batch.

## Objectives

- Improve test coverage with tests that protect behavior, not just inflate numbers.
- Prioritize high-impact internal logic over hard-to-maintain end-to-end additions.
- Measure progress after each batch using `cargo llvm-cov`.
- Report real bugs discovered during the process instead of masking them behind test-only changes.

## Constraints

- Keep the current testing style used by each crate.
- Do not make broad production refactors only for testability unless the lack of seams blocks valuable coverage.
- Disable `zksync_os_integration_tests::interop::test_interop_l2_to_l1_message_verification` only for the coverage profile for now.
- Preserve existing unrelated worktree changes.

## Strategy

Use an impact-first loop:

1. Stabilize coverage measurement.
2. Attack the largest high-value uncovered areas that are testable in isolation.
3. Re-run coverage after each slice.
4. Re-rank the next slice using the new report.

This avoids spending early time on low-signal or expensive-to-set-up modules when the same effort could cover critical branch-heavy logic elsewhere.

## Prioritization

### Wave 0: Coverage stability

- Exclude the long-running interop test from the coverage profile in `.config/nextest.toml`.
- Keep normal default profile behavior unchanged.

### Wave 1: RPC surface

Target `lib/rpc` first because it has a large uncovered footprint and many pure / component-level branches:

- `eth_impl`
- `ots_impl`
- `js_tracer::{tracer,host,types}`
- `monitoring_middleware`
- `zks_impl`

These files are rich in data-shaping, fallback, pagination, and error-path logic that should be covered with unit tests or narrow async tests.

### Wave 2: Sequencer and transaction types

Target logic with strong behavioral invariants:

- `lib/sequencer/src/execution/{block_executor,block_context_provider,utils}`
- `lib/types/src/transaction/{l1,l2,system}`
- selected receipt / encoding helpers

### Wave 3: Node entrypoints and prover handlers

Target `node/bin` modules where isolated behavior is testable without spinning up the whole server:

- config parsing / defaults
- prover API handlers / managers
- batcher decision logic

### Wave 4: Watchers and operational logic

- `lib/l1_watcher`
- `lib/base_token_adjuster`
- `lib/operator_signer`
- `lib/observability`

### Wave 5: Harder infrastructure gaps

- `lib/state`
- `lib/revm_consistency_checker`
- `lib/reth_compat`

These remain important, but are deferred until the easier high-value slices are harvested.

## Test Design Principles

- Prefer inline `mod tests` where the crate already uses them.
- Prefer direct helper / unit tests for pure logic and mapping code.
- Use `#[tokio::test]` only when async state or channels are essential.
- Reuse existing lightweight builders, mocks, and fixtures from nearby tests before introducing new shared harnesses.
- Add regression tests when a bug is found; keep the reproduction minimal.

## Measurement Loop

For each slice:

1. Add tests.
2. Run the narrow test target first.
3. Re-run coverage with the coverage profile.
4. Inspect the updated `lcov.info`.
5. Choose the next slice based on uncovered lines and behavioral importance.

## Expected Outcome

The near-term goal is not literal 100% immediately. The goal is to convert the highest-risk dark areas into well-covered modules while steadily pushing the aggregate report upward. This gives both better safety and better coverage metrics, rather than chasing one at the expense of the other.
