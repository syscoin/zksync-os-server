# Version Restart Integration Test Design

## Summary

Replace the CI shell-driven version restart scenario with Rust integration tests that download historical release binaries, restart the server on a shared RocksDB directory, and verify batch settlement behavior across version changes.

## Goals

- Keep the scenario in the existing `integration-tests` crate instead of GitHub Actions shell.
- Download the previous patch and previous minor release binaries inside the test harness.
- Reuse the same RocksDB directory across server restarts.
- Drive a small amount of traffic via the existing Rust providers.
- Require each phase to settle 3 new finalized L1 batches before restart.
- Capture both the “previous patch succeeds” and “previous minor becomes unusable” behaviors.

## Chosen Approach

Add a `version_restart` test harness to `integration-tests` that:

1. Starts Anvil L1 from the existing local-chain fixture.
2. Spawns external `zksync-os-server` binaries using a shared RocksDB path.
3. Uses a small Rust transaction driver based on the existing L2 provider to send transfers.
4. Waits until 3 new batches are finalized on L1 in each successful phase.
5. Downloads release binaries from GitHub Releases and caches them locally.

## Test Semantics

### Patch Restart

- Start the previous patch binary.
- Settle 3 batches.
- Stop the process.
- Start the current binary on the same DB.
- Settle 3 more batches.
- Test passes only if the node remains operational across the restart.

### Minor Restart

- Start the previous minor binary.
- Settle 3 batches.
- Stop the process.
- Start the current binary on the same DB.
- Test passes only if the node fails to become operational enough to settle 3 more batches within timeout.

This is intentionally observational. The test should not rely on an explicit DB compatibility marker.

## Harness Structure

- `integration-tests/src/version_restart/mod.rs`
  - release selection / download / cache helpers
  - external server process runner
  - transaction + batch settlement helpers
- `integration-tests/tests/version_restart.rs`
  - patch success scenario
  - minor unusability scenario

## Runtime Design

- Keep Anvil running for the whole test.
- Allocate dedicated RPC and auxiliary ports for each external server process.
- Keep logs for each phase in a temp directory for failure diagnostics.
- Use the existing rich test wallet to send simple value transfers.
- Measure success by finalized batch advancement, not by raw transaction count.

## Risks

- Historical release binaries may diverge in startup assumptions from the in-process test harness.
- Downloading release binaries introduces network and rate-limit risk, mitigated by local cache and optional `GITHUB_TOKEN`.
- The “minor fails” condition may manifest as timeout / startup exit / partial health instead of one canonical failure mode, so the test should classify all of those as “not operational”.
