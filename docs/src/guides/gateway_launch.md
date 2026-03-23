# Gateway Launch (Syscoin + OS Server)

This is the canonical guide for launching the Gateway topology with:

- upstream `zksync-era` control plane (`zkstack`)
- OS-chain mode (`--zksync-os`)
- `zksync-os-server` runtime
- Airbender proving

The goal topology is:

- Gateway chain: `rollup` pricing + `Blobs` runtime pubdata mode
- Child chain: `rollup` pricing + `RelayedL2Calldata` runtime pubdata mode

## Repos and prerequisites

- `zksync-era` (upstream; linked to your contracts fork)
- `era-contracts`
- `zksync-os-server`
- `zksync-airbender` and `zksync-airbender-prover`
- Syscoin L1 RPC and Bitcoin DA/PoDA connectivity

Update the existing `zksync-era/contracts` subrepo to your `era-contracts` `zkOS` branch
before building `zkstack` (local repo workflow).

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
cd "${ZKSYNC_ERA_PATH}"
git submodule update --init --recursive contracts

# local-only update: repoint contracts subrepo to syscoin/era-contracts and move to zkOS branch
cd contracts
git remote set-url origin git@github.com:syscoin/era-contracts.git
git fetch --all
git checkout zkOS
```

Build `zkstack` from your `zksync-era` checkout:

```bash
# Apply Syscoin/Tanenbaum compatibility patch to upstream era first.
bash /path/to/zksync-os-server/scripts/apply-zksync-era-syscoin-patch.sh /path/to/zksync-era

curl -L https://raw.githubusercontent.com/matter-labs/zksync-era/main/zkstack_cli/zkstackup/install | bash
zkstackup --local
```

## Environment

```bash
export ZKSYNC_ERA_PATH=/path/to/zksync-era
export SYSCOIN_L1_CHAIN_ID=5700   # Tanenbaum; use 57 on mainnet
export L1_RPC_URL=http://localhost:8545

export FOUNDRY_EVM_VERSION=shanghai
export FOUNDRY_CHAIN_ID=${SYSCOIN_L1_CHAIN_ID}
```

`--prover-mode gpu` is the source of truth for non-mock proving in this flow.
`zkstack` derives `testnet_verifier` from prover mode during deploy-input generation
(`gpu => false`, `no-proofs => true`).

## 1) Create Gateway ecosystem

```bash
export GATEWAY_CHAIN_ID=57001
export GATEWAY_PROVER_MODE=gpu
export GATEWAY_COMMIT_MODE=rollup

zkstack ecosystem create \
  --ecosystem-name gateway \
  --l1-network tanenbaum \
  --link-to-code ${ZKSYNC_ERA_PATH} \
  --chain-name gateway \
  --chain-id ${GATEWAY_CHAIN_ID} \
  --prover-mode ${GATEWAY_PROVER_MODE} \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode ${GATEWAY_COMMIT_MODE} \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default true \
  --evm-emulator false \
  --start-containers false \
  --zksync-os
```

## 2) Deploy ecosystem contracts

```bash
cd gateway
# set weth address by updating yaml files before you deploy
# Tanenbaum WETH
# configs/initial_deployments.yaml -> token_weth_address: 0xa66b2E50c2b805F31712beA422D0D9e7D0Fd0F35
# Mainnet WETH
# token_weth_address: 0xd3e822f3ef011Ca5f17D82C956D952D8d7C3A1BB

zkstack dev contracts

zkstack ecosystem init \
  --zksync-os \
  --update-submodules true \
  --l1-rpc-url ${L1_RPC_URL} \
  --deploy-ecosystem true \
  --deploy-erc20 false \
  --deploy-paymaster false \
  --ecosystem-only \
  --no-genesis \
  --observability false
```

## 3) Initialize Gateway chain

```bash
zkstack chain init \
  --chain gateway \
  --no-genesis \
  --deploy-paymaster false \
  --l1-rpc-url ${L1_RPC_URL}
```

## 4) Convert to Gateway settlement layer

```bash
zkstack chain gateway create-tx-filterer --chain gateway
zkstack chain gateway convert-to-gateway --chain gateway
```

## 5) Create and initialize child chain

```bash
export CHILD_CHAIN_NAME=zksys
export CHILD_CHAIN_ID=57057

zkstack chain create \
  --chain-name ${CHILD_CHAIN_NAME} \
  --chain-id ${CHILD_CHAIN_ID} \
  --prover-mode gpu \
  --wallet-creation random \
  --l1-batch-commit-data-generator-mode rollup \
  --base-token-address 0x0000000000000000000000000000000000000001 \
  --base-token-price-nominator 1 \
  --base-token-price-denominator 1 \
  --set-as-default false \
  --evm-emulator false \
  --zksync-os

zkstack chain init \
  --chain ${CHILD_CHAIN_NAME} \
  --no-genesis \
  --deploy-paymaster false \
  --skip-priority-txs \
  --l1-rpc-url ${L1_RPC_URL}
```

## 6) Migrate child to Gateway

```bash
zkstack chain gateway migrate-to-gateway \
  --chain ${CHILD_CHAIN_NAME} \
  --gateway-chain-name gateway \
  -v

zkstack chain gateway finalize-chain-migration-to-gateway \
  --chain ${CHILD_CHAIN_NAME} \
  --gateway-chain-name gateway
```

## Runtime presets and overrides

Runtime defaults are code-defined in:

- `node/bin/src/config/mod.rs`

Operational presets are in this repo (review and modify):

- `config-presets/testnet-gateway.yaml`
- `config-presets/mainnet-gateway.yaml`
- `config-presets/testnet-child.yaml`
- `config-presets/mainnet-child.yaml`

Use presets first, then per-chain generated config, then local override:

```bash
# Gateway (testnet)
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-gateway.yaml

# Child (testnet)
cargo run --release -- \
  --config /path/to/zksync-os-server/config-presets/testnet-child.yaml
```

Use `mainnet-gateway.yaml` / `mainnet-child.yaml` on mainnet.

## Provers

- Run Airbender stacks separately for Gateway and child.
- Do not use Era-only `zkstack prover` path for this topology.

## Known gotchas

- Always pass explicit flags (`--l1-network`, booleans like `--set-as-default true`).
- Do not use `-a` on `ecosystem create` / `chain create`.
- Keep deployer/governor sufficiently funded before each `init`; low-balance prompt paths can panic in non-interactive sessions.
