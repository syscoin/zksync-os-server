# Local Chains

This directory contains configuration files for running ZKsync OS nodes locally.

> **Canonical v32.0 fixture regeneration is pending.** No runnable V32 fixture is
> checked in. The prior V31 and stock V32 snapshots were removed because they do
> not bind the canonical Execution V7 / Proving V8 application, Security100 key,
> or Syscoin Era contracts. See
> [`v32.0/CANONICAL_V8_REGENERATION_REQUIRED`](./v32.0/CANONICAL_V8_REGENERATION_REQUIRED)
> for the atomic regeneration requirements.

## Directory Structure

```
local-chains/
├── README.md
└── v32.0/
    └── CANONICAL_V8_REGENERATION_REQUIRED
```

## Configuration Files

### `l1-state.json.gz`

No canonical snapshot is currently checked in. A fresh L1 state snapshot will
be documented after the complete V32 fixture is regenerated and the marker is
removed atomically.

### `config.yaml`

Node configuration file used to override the default values defined in the [config module](../node/bin/src/config).
Commonly modified values include:

- `genesis.chain_id` — Chain ID of the chain node operates on
- `genesis.bridgehub_address` — Address of the Bridgehub contract on L1
- `genesis.bytecode_supplier_address` — Address of the bytecode supplier contract
- `l1_sender.operator_commit_sk` — Private key for committing batches
- `l1_sender.operator_prove_sk` — Private key for proving batches
- `l1_sender.operator_execute_sk` — Private key for executing batches

### `genesis.json`

ZKsync OS genesis configuration with the following fields:

- `initial_contracts` -- Initial contracts to deploy in genesis. Storage entries that set the contracts as deployed and preimages will be derived from this field.
- `additional_storage` -- Additional (not related to contract deployments) storage entries to add in genesis state. Should be used in case of custom genesis state, e.g. if migrating some existing state to ZKsync OS.
- `execution_version` -- Execution version to set for genesis block.
- `genesis_root` -- Root hash of the genesis block, which is calculated as `blake_hash(root, index, number, prev hashes, timestamp)`. Please note, that after updating  `additional_storage` and `initial_contracts` this field should be recalculated. 

Default `genesis.json` has empty `additional_storage` and three contracts in `initial_contracts`: `L2ComplexUpgrader`, `L2GenesisUpgrade`, `L2WrappedBaseToken`.
If you are changing source code of any of the `initial_contracts` you should also update the `genesis.json` file with new bytecode 
(you can find it in the `deployedBytecode` field in `zksync-era/contracts/l1-contracts/out/<FILE_NAME>/<CONTRACT_NAME>.json`).

## Usage

Canonical fixture and GPU launch instructions are intentionally disabled while
[`CANONICAL_V8_REGENERATION_REQUIRED`](./v32.0/CANONICAL_V8_REGENERATION_REQUIRED)
exists. Do not bypass the marker or reuse an old Anvil snapshot. The sole
pre-keygen exception is a fresh `no-proofs` Gateway launch with the explicit
testnet verifier on localhost or Tanenbaum. It materializes the reviewed source
pair and may proceed past conversion only when the live Gateway target, compact
DA relay, and CREATE2 factory exactly match the app-bound candidate identities.
Any mismatch stops before edge creation for repinning and review. Runnable
canonical examples must still wait for the regenerated and attested V8 fixture.

## Adding a new protocol version

1. Create a new directory (e.g., `v32.1/`)
2. Use [upgrade scripts](https://github.com/matter-labs/zksync-os-scripts) to regenerate single and multi-chain configurations
3. Optionally add new scenario-specific subfolders if required
4. Update [protocol upgrade tests](../integration-tests/src/upgrade) to support the update to the new version
5. When upgrade is fully finalized, make sure:
   * The new default config in [main.rs](../node/bin/src/main.rs) is updated to point to the new version
   * `genesis.json` path in the [Dockerfile](../Dockerfile) is updated to point to the new version
   * `PROTOCOL_VERSION` constant in [default_protocol_version.rs](../node/bin/src/default_protocol_version.rs) is updated to the new version.
   * [`test-configs.sh`](../.github/scripts/test-configs.sh) script is updated to properly test the new version.

## Troubleshooting

### Anvil failed to start

- Check if port 8545 is already in use: `lsof -i :8545`
- Verify that decompressed `l1-state.json` exists and is valid JSON

### Chain fails to start

- Check for port conflicts between chains
- Verify all required config fields are present
- Check the terminal output for specific error messages

### Multiple chains: port conflicts

- Each chain config must specify unique ports. `rpc.address` - JSON-RPC port (e.g., 3050, 3051, 3052)
- Chains should be run in ephemeral mode or use unique directory paths for RocksDB and file storage to avoid interfering with one another.
