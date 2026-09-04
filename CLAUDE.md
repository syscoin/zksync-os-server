# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and Development Commands

### Basic Commands
- **Build**: `scripts/cargo-with-patched-zksync-os.sh dev-build -- build --locked` or `scripts/cargo-with-patched-zksync-os.sh dev-build-release -- build --locked --release`
- **Format**: `cargo fmt --all -- --check`
- **Lint**: `scripts/cargo-with-patched-zksync-os.sh dev-clippy -- clippy --locked --all-targets --all-features --workspace --exclude zksync_os_integration_tests -- -D warnings`
- **Unit tests**: `scripts/cargo-with-patched-zksync-os.sh dev-test -- nextest run --locked --workspace --exclude zksync_os_integration_tests`
- **Integration tests**: `scripts/cargo-with-patched-zksync-os.sh dev-integration -- nextest run --locked -p zksync_os_integration_tests --profile no-pig` (no live anvil needed — each test manages its own L1/node; `--profile no-pig` disables Prover Input Generation for faster runs)

The wrapper checks out the official Matter Labs zksync-os revision pinned by this
workspace, applies the checked-in Syscoin patch locally, and builds a disposable
rewritten server workspace. Do not use plain Cargo for commands that compile
`multivm`; its build script deliberately rejects the unpatched upstream source.

### Local Development Setup

The canonical v32.0/V8 local-chain fixture is pending atomic regeneration. The
historical files were removed; do not populate or consume a canonical fixture under
`local-chains/v32.0` while `CANONICAL_V8_REGENERATION_REQUIRED` exists. The sole
pre-regeneration launch path is the explicitly gated no-proofs localhost/Tanenbaum
flow described by that marker: it may materialize only the reviewed source pins and
must not publish, authorize, or run the absent canonical/GPU artifacts. Canonical
setup commands will be restored with the regenerated fixture.

## Submitting a PR

### PR title

PR titles must follow the [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) specification:

```
<type>(<scope>): <short description>
```

Examples: `feat(eth_sender): Support new transaction type`, `fix(state_keeper): Correctly handle edge case`, `ci: Add new workflow for linting`

### Breaking changes

If the PR title uses the breaking-change marker (`feat!: ...`, `fix!: ...`), you **must** uncomment and fill in the **Breaking Changes** and **Rollout Instructions** sections in the PR description (see `.github/pull_request_template.md`).

### Wire format immutability

Do **not** modify the contents of existing versioned wire format files under
`lib/network/src/wire/replays/v*.rs`. Add a new versioned file instead. An obsolete format may be
deleted only in an explicit breaking retirement after its deployed network is retired; never reuse
its protocol number or message IDs.

### Comments
Comment **why**, not **what**. The code shows what it does; comments explain intent, invariants, and non-obvious decisions. No comments on self-evident code.

✅ **Comment when:**
- Non-obvious behavior or edge cases
- Performance trade-offs
- Safety requirements (unsafe blocks must always be documented)
- Limitations, constraints, assumptions or gotchas
- Why simpler alternatives don't work

❌ **Don't comment when:**
- Code is self-explanatory
- Just restating the code in English
- Describing what changed in this PR
