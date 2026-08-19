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

For v31, `run_local.sh` derives the Syscoin DA commit target from
`../multi_chain/contracts_506.yaml`, applies the repository patch to the exact
official zksync-os revision in `Cargo.lock`, and runs the resulting prebuilt
binary. Custom fixtures without a matching `contracts_<chain-id>.yaml` must set
`SYSCOIN_EDGE_DA_COMMIT_TARGET` to their deployed validator timelock address.

## Wallets

For complete list of keys and wallet addresses, check [wallets.yaml](./wallets.yaml).

## Contract Addresses

For contract addresses, please refer to `genesis` section of the [config.yaml](./config.yaml).

## Versions

For information about how this config was created, check [version.yaml](../versions.yaml) file.
