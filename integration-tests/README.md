## Integration Tests

This directory contains a fairly simple framework designed to test ZKsync OS node's behavior e2e on a local setup.

Main design considerations:
* **Isolation**. Every test initializes its own node, L1, persistence resources etc. Meaning you can run every test independently of each other.
* **Speed**. Pre-initialized L1 state avoids repeating deployments for each test. Startup time still depends on fixture size and replay work.
* **ZKsync-agnostic**. Test try to use general Ethereum tooling where this makes sense (e.g., base `alloy`) to enforce our compatibility with Ethereum.
* **Rust-first**. No reliance on external tooling to run the tests, regular Rust flows should work as expected. `RUST_LOG`/`RUST_BACKTRACE` get propagated to all relevant components (node, prover).

Known limitations:
* No support for node restarts - not a fundamental issue, can be added in the future
* No L1 logs - `alloy` provider swallows `anvil` logs by default with no option to disable this behavior, but this can be solved by writing our own `anvil` spawner
* No graceful shutdown - support is blocked by the node itself not doing graceful shutdown yet

### Run

The ordinary node/RPC/transaction tests use an explicit **AnvilComponentOnly**
V32/V8 fixture at `local-chains/anvil-component-only/v32.0`. Protocol identity is
still V32, Execution7 / Proving8 / Security100 and the unchanged app-bound VK;
this is not Syscoin Core/NEVM consensus, live Bitcoin DA or real-proof release
qualification. All original ordinary cases remain selected. Enabling
`prover-tests` selects the canonical fixture purpose instead, so the real prover
test cannot silently inherit the component lane.

The component topology has three physical L2 chains: Gateway57001 settling on
Anvil31337, and edges6565/6566 settling on Gateway. The `default` layout is the
direct-L1 test view of that same Gateway57001 fixture, not a fourth chain12345.
Each test creates its own Anvil and node instances. The two Gateway config views
use separate test ports but share the honestly identified Gateway deployment
and replay archive; generation never starts duplicate producers for one chain.
The default view retains Gateway's transaction filter and is not an unrestricted
user-asset chain. Ordinary ERC20 deposit/transfer/withdrawal and priority
system-contract pubdata tests select `CURRENT_TO_GATEWAY`, exercising the edge
through the real Root-to-Gateway relay and recursive Root withdrawal proof.
Their transaction, balance, proof, and sealing assertions remain enforced.

The checked-in component package was freshly generated, restored into three
new databases, and checked with actual RPC/transaction receipts on each chain.
Its 250 public files are authenticated by the descriptor hash in
`scripts/fixtures/v32-component-registration.json`. Generation and package review
passed; the original full suite and config-smoke jobs must still pass on the
current consumers before this change qualifies ordinary CI. The canonical
`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED` and the five-role release
fixture gate remain unchanged.

#### Regenerating the component fixture

1. In a disposable isolated workspace, bind the current server, exact reviewed
   Syscoin-patched zkstack postimage and Era generated-verifier overlay. The stock
   `zksync-os-scripts` V30/V31 generator is incompatible: it also exports random
   wallets and rewrites the VK. Do not run it unchanged. A retained older zkstack
   binary is not automatically the current patched-source build.
2. Deploy a fresh **localhost-only Anvil** L1, Gateway57001 and edges6565/6566 with
   root chain31337, current protocol0.32.0 and the guest-bound critical target/relay.
   No fork/private FULL/DB snapshot or old V31 state may be promoted. Any special
   Anvil impersonation used for component ownership must be explicit in the
   deployment record; it is not real governance authentication. The sole explicit
   code-install exception is a source-built strict32-byte availability mock at
   the fresh Root Anvil's empty 0x63, qualified with actual 1400-gas calls and
   malformed-input controls before and after restore. It assumes availability
   for all32-byte identifiers, not real Bitcoin DA/finality. No other contract
   code or storage substitution may hide an incompatible deployment.
3. Reuse the accepted genesis SHA `5adf0dd1…5055a95` and existing Security100 VK
   `c1ab3d65…b7388fe`; no guest/key regeneration or source-pin rewrite. Read back the
   deployed verifier/addresses/version and record the exact source/tool/artifact
   identities and actual initialization/deposit receipts.
4. Use only public insecure development signers, with exclusive sender accounts
   per settlement endpoint. CLI deployment/admin uses scalar-3; Gateway Root
   commit/prove/execute use scalars 1/7/2, edge6565 uses 1/account0/2 on Gateway,
   and edge6566 uses 4/5/6 on Gateway. Account0 remains the test wallet/reverter,
   separate from the live Root prove sender. Normal deployment/migration and
   owner-authorized APIs supply actual operator and REVERTER_ROLE grants;
   chain-scoped roles, funding and canonical grant receipts are checked.
   Publish no generated `wallets.yaml`, keystore,
   password, cookie, private operator configuration, DB, log, or key-bearing CLI
   output. The test reverter derives the already-public source constant, not a
   packaged wallet. Configs are JSON (valid YAML) so public-field validation is
   unambiguous; local RPC only and no secret/auth fields.
5. Package only the exact roles in `fixture_component.rs::PUBLIC_FILES`, plus
   the fresh Zstandard-compressed Anvil state and its exact decompressed identity.
   Historical state is preserved byte-for-byte, not filtered for size. Include
   each seed node's public `database_identity.json` for its fresh replay restore;
   the ordinary deployment-identity check remains enforced. Include
   honest `deployment.json`, `source-identities.json`, `proof-identities.json`
   and `test-signers.json`; these records must identify component mocks and may
   not claim real consensus/proof/DA/release qualification. Produce the exact
   typed `anvil-component.json` descriptor, review every public postimage, then
   issue its SHA in the separate source-controlled registration. A package
   sidecar, environment variable or missing registration cannot authorize it.
6. Run the unchanged full workspace nextest selection (all110 ordinary runnable
   integration cases), then `test-configs.sh --anvil-component-only` with real
   Anvil/server startup, RPC and transaction/receipt checks. The independent
   canonical real-prover/five-role release gate remains pending; component test
   success is never a canonical release-green claim.

Pre-requisites:
* Make sure you have `cargo-nextest` version `0.9.101` or later installed in your system as we use some recently added features.
* Make sure you have `forge` installed to be able to build test contracts (see [README](./test-contracts/README.md)).
* Install `zstd` when running the component config-smoke script or regenerating its fixture. Cargo decodes registered fixtures using the pinned Rust library.
* Use `RUST_BACKTRACE=1` (or `RUST_BACKTRACE=full`) to enable backtraces.
* Configure logging level via `RUST_LOG=debug` (or `RUST_LOG=info,zksync_os_sequencer=debug` for specific components).

#### Examples

```shell
# All tests (excluding prover)
./scripts/cargo-with-patched-zksync-os.sh integration-all -- \
  nextest run --locked -p zksync_os_integration_tests

# Single test with exact name match
./scripts/cargo-with-patched-zksync-os.sh integration-basic -- \
  nextest run --locked -p zksync_os_integration_tests -E 'test(=basic_transfers)'
# If you want logs to be printed during the test:
./scripts/cargo-with-patched-zksync-os.sh integration-basic-logs -- \
  nextest run --locked -p zksync_os_integration_tests -E 'test(=basic_transfers)' --no-capture
# If you want all tests containing a substring:
./scripts/cargo-with-patched-zksync-os.sh integration-call -- \
  nextest run --locked -p zksync_os_integration_tests -E 'test(~call)'

# All tests inside `./tests/call.rs`
./scripts/cargo-with-patched-zksync-os.sh integration-call-binary -- \
  nextest run --locked -p zksync_os_integration_tests -E 'binary(call)'

# Run prover tests (CPU prover)
# !! Note `--release`, important to avoid stack overflow and low performance in prover !!
# SYSCOIN: Set SYSCOIN_V8_PROVER_BIN, SYSCOIN_V8_APP_BIN, and COMPACT_CRS_FILE to the
# reviewed app-bound combined prover, canonical V8 app, and compact CRS respectively.
# SYSCOIN_V8_PROVING_TIMEOUT_SECS optionally overrides the four-hour live-proof deadline.
./scripts/cargo-with-patched-zksync-os.sh integration-prover-cpu -- \
  nextest run --locked --release -p zksync_os_integration_tests \
  --features prover-tests -E 'binary(prover)'

# Run prover tests (GPU prover)
# !! Note `--release`, important to avoid stack overflow and low performance in prover !!
# !! Requires a compatible NVIDIA GPU and CUDA toolkit 12.x installed !!
# Qualify capacity for the selected V32 prover and memory mode; this example
# does not establish a 24GB VRAM or whole-pipeline host-memory requirement.
# SYSCOIN: The same three app-bound artifact variables above are mandatory.
./scripts/cargo-with-patched-zksync-os.sh integration-prover-gpu -- \
  nextest run --locked --release -p zksync_os_integration_tests \
  --features gpu-prover-tests -E 'binary(prover)'
```
