# Isolated V32 native witness smoke export

This opt-in ignored test generates a genuine native-execution / Merkle witness for one fresh
block containing only `SetSLChainId`. It is a proof smoke input, **not representative throughput**,
a release attestation, a canonical local-chain fixture, or a substitute for sequencer/settlement
end-to-end validation. Transfer and contract-call performance need a separate workload corpus.

Wait until the final owner-bound guest source tree, paired app artifacts, Security100 program
commitment, and freshly reproduced genesis are verified. The exporter does not authorize an
unreleased VK, populate `local-chains/v32.0`, send transactions, or contact a sequencer/L1 RPC.
The canonical regeneration marker and all production proving gates remain unchanged.

## Explicit inputs

Place the final genesis JSON, app `.bin` / `.text` siblings, and a reviewed configuration outside
the server checkout. Use absolute paths without symlink components or `..`; on macOS use resolved
paths such as `/private/tmp/...`, not the `/tmp` symlink. The output's parent must exist, but the
output directory itself must not. Do not mutate inputs or parent directories during the run.

Configuration schema (replace every angle-bracket placeholder before hashing):

```json
{
  "schema_version": 1,
  "genesis_path": "/absolute/external/final-genesis.json",
  "genesis_sha256": "<verified-lowercase-64-digit-SHA256>",
  "app_bin_path": "/absolute/external/multiblock_batch.bin",
  "app_bin_sha256": "<verified-lowercase-64-digit-SHA256>",
  "app_text_path": "/absolute/external/multiblock_batch.text",
  "app_text_sha256": "<verified-lowercase-64-digit-SHA256>",
  "guest_source_tree": "<reviewed-40-digit-patched-source-tree>",
  "expected_security100_program_commitment": "0x<verified-lowercase-64-digit-commitment>",
  "chain_id": 57057,
  "sl_chain_id": 57001,
  "pubdata_mode": "RelayedL2Calldata",
  "compact_da_commit_target": "0x<compiled-final-lowercase-40-digit-target>",
  "output_dir": "/absolute/external/fresh-witness-output"
}
```

The example IDs describe an edge settling on Gateway. Select the intended explicit chain and
settlement IDs; both must be nonzero and distinct. `pubdata_mode` accepts only `Blobs` or
`RelayedL2Calldata`; select the real deployment topology. The lane is fixed to protocol V32,
Execution V7, Proving V8 and Security100. Unknown, duplicate, missing, or incorrectly typed fields
fail. The source tree must match the reviewed pin compiled from the build wrapper, and the
compact target must match the compiled guest binding; arbitrary deployment rewrites are rejected.

## Run only through the patched-source wrapper

```sh
V8_PROVER_INPUT_CONFIG=/absolute/external/witness-config.json \
V8_PROVER_INPUT_CONFIG_SHA256='<independently-reviewed-config-SHA256>' \
scripts/cargo-with-patched-zksync-os.sh v32-witness-smoke -- \
  test --locked --release -p zksync_os_batch_verification \
  dump_v8_simplest_batch_prover_input --lib -- --ignored --nocapture
```

The exporter hashes the exact config, genesis, and app bytes. Native genesis construction uses
the already-hashed snapshot, and the persisted Merkle state must match its declared nonzero
genesis root. The block executes natively and gets a real Merkle update proof. Native batch PIG
then cross-checks replay output / post-state, and `build_batch_info` verifies the chain ID,
settlement ID, and reconstructed V8 public-input hash before any output directory is created.

Outputs are `v8_simplest_prover_input.le.bin` (raw little-endian `u32` words),
`v8_simplest_prover_input.hex` (concatenated eight-digit hex words), and `manifest.json`.
The manifest binds the witness SHA-256/word count, app hashes, expected native public input,
supplied Security100 program commitment, and detailed genesis/source/chain/DA provenance.
The independently run FRI harness must verify the actual program commitment and proof registers;
the exporter does not claim to have proven the witness. Its expected public-input registers are
the eight little-endian `u32` chunks of the 32-byte native public-input hash.

Output creation never overwrites existing files. A failed write retains partial output for
inspection; use another fresh directory after diagnosing it. Do not treat partial output or a
manifest alone as proof verification or production readiness.

Config/path/hash tests (no ignored witness execution):

```sh
scripts/cargo-with-patched-zksync-os.sh v32-witness-config-tests -- \
  test --locked -p zksync_os_batch_verification witness_export::config_tests --lib
```

## Two contiguous batches for real aggregation

Use a separate config with a fresh output directory and select the ignored test
`dump_v8_two_batch_prover_inputs` instead. It natively executes batch 1 with `SetSLChainId`,
then batch 2 with no transactions, carrying the actual storage writes, published preimages,
block hashes, timestamp, and persisted Merkle versions forward. Native batch generation checks
that batch 2 starts at batch 1's final state commitment. Neither batch is a copied proof or a
fabricated state; both remain minimal correctness inputs, not a throughput workload.

This mode writes `batch-1/` and `batch-2/`, each with the three files above, plus `sequence.json`.
The sequence record hashes both manifests/witnesses and records the expected combined public
input as `Keccak256(batch_1_public_input || batch_2_public_input)` using the two ordered 32-byte
hashes. A separate real carried-chain combiner must verify both FRI proofs and its combined
proof before that proof is passed to the SNARK wrapper with auxiliary-parameter checks enabled.
Do not substitute two GPU-preset runs of the same batch for two contiguous batch statements.
