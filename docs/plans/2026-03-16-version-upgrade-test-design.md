# Version Upgrade Test Design

## Summary

Add a GitHub Actions workflow that validates database compatibility across a latest -> previous minor -> latest restart sequence while reusing the same RocksDB directory for every server instance.

## Goals

- Build the current branch's `zksync-os-server` binary.
- Download the latest patch release from the previous minor line from GitHub Releases.
- Start each binary against the same RocksDB path.
- Run `loadbase` in every phase and confirm block production continues after each restart.

## Chosen Approach

Use a single dedicated workflow with one inline shell orchestration step for the compatibility scenario.

The workflow will:

1. Build the current branch's Linux release binary.
2. Resolve the previous minor release tag from GitHub Releases.
3. Download the published Linux tarball for that tag.
4. Start `anvil` once from the existing `local-chains/v30.2` fixture.
5. Build `loadbase` once for use as the transaction driver.
6. Run three server phases against one explicit `general_rocks_db_path`:
   - current branch binary
   - previous minor release binary
   - current branch binary again
7. Run `loadbase` for 30 seconds in each phase and assert block height increases.

## Why This Approach

- Keeps the workflow implementation localized and fast to review.
- Uses the exact published release artifact for the downgrade phase, which matches the release-compatibility requirement.
- Avoids adding a new helper script before the behavior is proven useful.
- Keeps the transaction-generation logic out of the shell script.

## Runtime Design

- `anvil` is started once and remains running for the whole job.
- The server is always started with:
  - `--config ./local-chains/v30.2/default/config.yaml`
  - `general_rocks_db_path=<shared path>`
- The shared RocksDB directory lives under `${RUNNER_TEMP}` and is never deleted between phases.
- Each server instance is stopped cleanly before the next one starts.

## Previous Minor Resolution

- Read the current version from the workspace `Cargo.toml`.
- Compute the target minor line as `major.(minor - 1)`.
- Query GitHub Releases and select the highest semver tag matching `v<target minor>.*`.
- Download `zksync-os-server-<tag>-x86_64-unknown-linux-gnu.tar.gz`.

## Verification

- Wait for JSON-RPC to become reachable on port `3050`.
- Record the latest block number before sending transactions.
- Run `loadbase` against the phase RPC endpoint for 30 seconds.
- Poll until the latest block number increases.
- Fail if the server cannot reopen the existing RocksDB or if block production stalls.

## Failure Diagnostics

- Persist `anvil` and per-phase server logs as workflow artifacts on failure.
