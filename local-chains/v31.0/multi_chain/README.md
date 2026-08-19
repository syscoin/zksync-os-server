# Multiple Chains (v31.0)

Configuration for running multiple ZKsync OS chains against a shared L1.

## Chains

| Config            | Chain ID | RPC Port |
|-------------------|----------|----------|
| `chain_6565.yaml` | 6565     | 3050     |
| `chain_6566.yaml` | 6566     | 3051     |

## Quick Start

```bash
# Use script to launch in-memory L1 and two nodes for both chains
./run_local.sh ./local-chains/v31.0/multi_chain
```

For v31, `run_local.sh` applies the repository patch to the exact official
zksync-os revision in `Cargo.lock`, builds once, and runs that prebuilt binary
for both chains with the edge-DA and gas-tank constants bound to the published
V7 proving app. These older fixtures use different deployment addresses, so
they are useful for general multi-node development but not compact edge-DA or
zkSYS gas-tank execution tests.

## Wallets

For complete list of keys and wallet addresses, check:
* [wallets_6565.yaml](./wallets_6565.yaml)
* [wallets_6566.yaml](./wallets_6566.yaml)
  for the corresponding chain.

## Contract Addresses

For contract addresses, please refer to `genesis` section of:
* [chain_6565.yaml](./chain_6565.yaml)
* [chain_6566.yaml](./chain_6566.yaml)
  for the corresponding chain.

## Versions

For information about how this config was created, check [version.yaml](../versions.yaml) file.
