# Single Chain (v31.0)

Default single-chain configuration for running ZKsync OS against L1 for protocol version v31.0.

## Chains

| Config            | Chain ID | RPC Port |
|-------------------|----------|----------|
| `config.yaml`     | 506      | 3050     |

## Quick Start

```bash
# Use script to launch in-memory L1 and the node for one chain
./run_local.sh ./local-chains/v31.0/default
```

For v31, `run_local.sh` applies the repository patch to the exact official
zksync-os revision in `Cargo.lock` and runs the resulting prebuilt binary with
the same edge-DA and gas-tank constants as the hash-pinned V7 proving app. The
older local fixture has a different validator-timelock address, so it is useful
for general node development but not for exercising compact edge-DA commits.

## Wallets

For complete list of keys and wallet addresses, check [wallets.yaml](./wallets.yaml).

## Contract Addresses

For contract addresses, please refer to `genesis` section of the [config.yaml](./config.yaml).

## Versions

For information about how this config was created, check [version.yaml](../versions.yaml) file.
