# Gateway Launch (Checkpointed Canonical Flow)

Gateway + edge launch is now a **single canonical command** with checkpointed resume and explicit repair.

## Host prerequisites

Install the host toolchain before starting the launcher. The launcher can
materialize the pinned `zksync-era` workspace and apply Syscoin patches itself,
but it expects the base build and signing tools to already be available.

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config libssl-dev clang lld cmake protobuf-compiler \
  libclang-dev git curl jq unzip zip ca-certificates python3 python3-pip \
  python3-venv tmux screen expect moreutils gnupg
```

If you are building the local Syscoin Core node on the same host, also install
the Unix build dependencies from Syscoin's `doc/build-unix.md`. Boost, libevent,
SQLite, and ZMQ are needed for the command-line node / wallet / DA RPC path used
by launch:

```bash
sudo apt-get install -y \
  libtool autotools-dev automake bsdmainutils libgmp-dev \
  libevent-dev libboost-dev libsqlite3-dev libzmq3-dev \
  libminiupnpc-dev libnatpmp-dev
```

Then build Syscoin Core:

```bash
cd /path/to/syscoin
./autogen.sh
./configure --without-gui
make -j"$(nproc)"
```

The launch flow assumes descriptor wallets backed by SQLite. Berkeley DB is only
needed if you intentionally enable legacy wallets; do not add it for the normal
Gateway launch path.

Install Rust and the RISC-V support used by the `v31` OS-server line:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default
export PATH="$HOME/.cargo/bin:$PATH"
rustup toolchain install nightly-2026-01-22
rustup default nightly-2026-01-22
rustup target add riscv32i-unknown-none-elf
rustup component add llvm-tools-preview rust-src
cargo install cargo-binutils
```

Install Node/Yarn:

```bash
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
sudo apt-get install -y nodejs
sudo corepack enable
corepack prepare yarn@stable --activate
```

Install **foundry-zksync**, not vanilla Foundry. `zkstack` checks that
`forge build --help` contains `ZKSync configuration`; a regular Foundry
installation does not satisfy this prerequisite.

```bash
curl -L https://raw.githubusercontent.com/matter-labs/foundry-zksync/main/install-foundry-zksync | bash
export PATH="$HOME/.foundry/bin:$PATH"
foundryup-zksync
```

Cache the Solidity compiler used by the v31 contracts before the first offline
launch. `run-gateway-launch.sh` defaults `FOUNDRY_OFFLINE=true`, so an empty
compiler cache will otherwise fail with `can't install missing solc 0.8.28 in
offline mode`.

```bash
tmp="$(mktemp -d)"
cd "$tmp"
cat > foundry.toml <<'EOF'
[profile.default]
src = "src"
out = "out"
libs = []
solc_version = "0.8.28"
EOF
mkdir -p src
cat > src/CacheSolc.sol <<'EOF'
// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;
contract CacheSolc {}
EOF
FOUNDRY_OFFLINE=false forge build
cd -
rm -rf "$tmp"
```

Import the L1 funding/deployment signer into Foundry's encrypted account store:

```bash
cast wallet import funder --interactive
cast wallet address --account funder
```

For unattended launches, create a `0600` password file and pass it via env:

```bash
umask 077
printf '%s' '<keystore-password>' > "$HOME/.foundry/funder.password"
export FUNDER_PASSWORD_FILE="$HOME/.foundry/funder.password"
```

If using a separate Gateway governor signer for migration repairs, import it as
another Foundry account (for example `governor`) and set
`EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME=governor`.

## Canonical command

Start local Syscoin RPC bridge first (Tanenbaum/Mainnet launcher expects local `L1_RPC_URL`):

```bash
./syscoind --testnet -server=1 -daemon=1 \
  -gethcommandline=--http \
  -gethcommandline=--http.addr=127.0.0.1 \
  -gethcommandline=--http.port=8545 \
  -gethcommandline=--http.api=eth,net,web3 \
  -gethcommandline=--http.vhosts=localhost,127.0.0.1
```

Keep the local RPC bound to loopback and do not enable wildcard CORS. Only add
extra namespaces such as `txpool` or `debug` for a short, trusted debugging
session, then restart the node with the restricted API list above.

Run from a `zksync-os-server` clone:

```bash
cd /path/to/zksync-os-server
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.tanenbaum.io
export PROVER_API_AUTH_PASSWORD=...
export FUNDER_SIGNER=account
export FUNDER_ACCOUNT_NAME=funder
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --migrate-edge
```

Mainnet:

```bash
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.syscoin.org
export PROVER_API_AUTH_PASSWORD=...
export FUNDER_SIGNER=account
export FUNDER_ACCOUNT_NAME=funder
bash scripts/gateway-launch/run-gateway-launch.sh --l1 mainnet --migrate-edge
```

`L1_RPC_URL` is written as the normal OS-server L1 provider and can point at the
local sysgeth used for live traffic. `GATEWAY_ARCHIVE_L1_RPC_URL` is written as
the archive L1 provider used only for historical committed-batch startup reads;
point it at an archive-capable Syscoin L1 RPC unless the local `L1_RPC_URL` node
was synced in archive mode from genesis.

Do not pass production private keys as raw command-line arguments. Import the
launch signer into Foundry's keystore first with
`cast wallet import funder --interactive`, or use the `keystore`, `ledger`,
`trezor`, `aws`, or `gcp` backend. On Tanenbaum/Mainnet, deployer and governor
signing default to the same funder signer/account unless `DEPLOYER_*` or
`EDGE_GATEWAY_GOVERNOR_*` overrides are set. `FUNDER_PRIVATE_KEY` and
`DEPLOYER_SIGNER=private-key` are local/disposable-network fallbacks only; on
Tanenbaum/Mainnet they are rejected unless
`GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true` is set explicitly.
The `--migrate-edge` flag is required for the normal full launch because it pauses deposits and finalizes the edge-chain migration to Gateway settlement. If omitted, the launcher stops after edge-chain initialization and can be resumed later with the same command plus `--migrate-edge`.

Optional log override:

```bash
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --log /tmp/gateway-launch.log
```

To intentionally continue from an existing ecosystem directory that already has
`ZkStack.yaml`, pass `--reuse-ecosystem`. Without this flag, the launcher fails
instead of silently bypassing wallet creation controls.

## Checkpoint model

State file:

```text
$GATEWAY_DIR/.gateway-launch/state.json
```

Ordered checkpoints:

1. `gl.workspace`
2. `gl.ecosystem`
3. `gl.wallets_funded`
4. `gl.l1_ecosystem_deployed`
5. `gl.gateway_chain_inited`
6. `gl.gateway_settlement`
7. `gl.os_configs_gateway`
8. `gl.edge_chain_inited`
9. `gl.migration`
10. `gl.os_configs_final`

Behavior:

- A checkpoint is skipped only when state says `passed` **and** probe validation passes.
- Any failure marks checkpoint `blocked` and stops the run.
- Resume is automatic on the same command after repair.
- Resume is rejected if fingerprinted launch context changed (network/chain/sha/env mismatch).

## Repair workflow

Inspect state:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum status
```

Repair one blocked checkpoint:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum repair gl.edge_chain_inited
```

Then rerun the canonical launcher command.

## What the launcher does

- Creates a new ecosystem, or reuses one only when `--reuse-ecosystem` is explicit.
- Funds required wallets.
- Deploys/initializes gateway contracts and chain.
- Converts gateway settlement.
- Creates/initializes edge chain.
- Runs edge migration to gateway settlement.
- Ensures DA pair correctness and deposit unpause via migration script guards.
- Generates final `os-server-configs` launchers.

## Configure replay archive before production start

Before the first mainnet node start, enable an independent replay archive in
each final OS-server config that you intend to rely on for recovery. External
nodes keep their own `block_replay_wal` once fully synced, but explorer EN
disks are operational redundancy, not a cold backup. Keep at least one
off-node replay archive for Gateway and zksys so replay records survive local
RocksDB loss, host rebuilds, or accidental EN database wipes.

For production, prefer encrypted S3 or another S3-compatible object store:

```yaml
replay_archive:
  type: S3WithCredentialFile
  bucket_base_url: syscoin-mainnet-replay-archive
  s3_credential_file_path: /etc/zksync-os/replay-archive-s3-credentials
  endpoint: null
  region: us-east-2
  encryption:
    type: AgeX25519
    recipient: age1...
```

The node only needs the age public recipient key. Store the corresponding
`AGE-SECRET-KEY-...` separately from the hot node and use it only for recovery.
If S3 is not ready at launch, enable a filesystem replay archive on a durable
path and back that path up off-host:

```yaml
replay_archive:
  type: FileSystem
  root_path: /var/lib/zksync-os/replay_archive
  encryption:
    type: AgeX25519
    recipient: age1...
```

Do not treat `replay_archive: { type: Noop }` as sufficient for mainnet
operations. Test recovery before relying on the archive: download the archive
objects, rebuild `db/block_replay_wal` with `replay_archive_recovery`, and
start a node from the recovered replay WAL using a known canonical anchor.

## Start nodes after successful launch

```bash
"$GATEWAY_DIR/os-server-configs/gateway/start-node.sh"
"$GATEWAY_DIR/os-server-configs/zksys/start-node.sh"
```

## Post-deploy opsec

After `gl.os_configs_final` passes and both final node config directories exist,
the hot runtime only needs the operational node configs and chain artifacts:

```text
$GATEWAY_DIR/os-server-configs/*/config.yaml
$GATEWAY_DIR/os-server-configs/*/start-node.sh
$GATEWAY_DIR/os-server-configs/*/contracts.yaml
$GATEWAY_DIR/os-server-configs/*/genesis.json
```

The `config.yaml` files contain the runtime operator keys:

```text
operator_commit_sk
operator_prove_sk
operator_execute_sk
```

Do not delete those final `config.yaml` files unless you are intentionally
rotating/rebuilding runtime operator keys. Keep the Syscoin DA wallet material
needed by the local Syscoin node if the node will publish blobs.

Before removing launch-time keys, make an encrypted operational backup on an
operator-controlled encrypted or off-host backup path. Do not create plaintext
secret archives under `/tmp`.

The operational backup should contain the chain-scoped Gateway and zksys wallet
files plus the final OS-server runtime configs. These are the management and
runtime keys that matter after launch:

```text
$GATEWAY_DIR/chains/gateway/configs/wallets.yaml
$GATEWAY_DIR/chains/zksys/configs/wallets.yaml
$GATEWAY_DIR/os-server-configs/gateway/config.yaml
$GATEWAY_DIR/os-server-configs/zksys/config.yaml
```

The final `config.yaml` files contain the operator private keys used by the
running nodes. The chain-scoped `wallets.yaml` files contain the deployer,
governor, fee, and operator wallet keys needed for repair/governance/migration
work. Avoid backing up root or duplicate wallet files such as
`$GATEWAY_DIR/configs/wallets.yaml`, `$GATEWAY_DIR.wallets.yaml`, hidden
`.*wallets.yaml` copies, or `*.backup` files unless you are intentionally making
a broader forensic archive; exporting extra unrelated keys increases confusion
and recovery risk.

```bash
(
set -euo pipefail
umask 077
BACKUP_DIR="${BACKUP_DIR:?set BACKUP_DIR to an encrypted/off-host backup directory}"
install -d -m 700 "$BACKUP_DIR"
backup_archive="$BACKUP_DIR/gateway-launch-secrets-$(date -u +%Y%m%dT%H%M%SZ).tar.gz.gpg"

secret_list="$(mktemp "$BACKUP_DIR/gateway-launch-secrets.XXXXXX.list")"
trap 'rm -f "$secret_list"' EXIT

shopt -s nullglob
secret_paths=(
  "$GATEWAY_DIR"/chains/gateway/configs/wallets.yaml
  "$GATEWAY_DIR"/chains/zksys/configs/wallets.yaml
  "$GATEWAY_DIR"/os-server-configs/gateway/config.yaml
  "$GATEWAY_DIR"/os-server-configs/zksys/config.yaml
  "$HOME"/.foundry/*.password
)

for path in "${secret_paths[@]}"; do
  printf '%s\n' "$path" >> "$secret_list"
done

if [ -d "$HOME/.foundry/keystores" ]; then
  printf '%s\n' "$HOME/.foundry/keystores" >> "$secret_list"
fi

sort -u -o "$secret_list" "$secret_list"
[ -s "$secret_list" ] || {
  echo "no launch-time secret files found; refusing to create an empty backup" >&2
  exit 1
}

tar -czf - -T "$secret_list" \
  | gpg --symmetric --cipher-algo AES256 \
      --output "$backup_archive"
chmod 600 "$backup_archive"

gpg --decrypt "$backup_archive" \
  | tar -tzf - >/dev/null
printf 'encrypted backup created: %s\n' "$backup_archive"
)
```

Keep the encrypted archive and its passphrase separated. The command exits on
backup or verification failure; do not delete source keys unless it completes
successfully and the encrypted backup has been copied to durable storage.

After copying that backup off the hot host, remove launch-time wallet files,
duplicate wallet copies, and Foundry signer material. Do not remove final
`config.yaml` files unless you are intentionally rotating/rebuilding runtime
operator keys.

```bash
rm -f "$GATEWAY_DIR".wallets.yaml
rm -f "$GATEWAY_DIR"/*.wallets.yaml
rm -f "$GATEWAY_DIR"/.*wallets.yaml
rm -f "$GATEWAY_DIR"/*wallets.yaml.*
rm -f "$GATEWAY_DIR"/.*wallets.yaml.*
rm -f "$GATEWAY_DIR"/configs/wallets.yaml
rm -f "$GATEWAY_DIR"/configs/wallets.yaml.*
rm -f "$GATEWAY_DIR"/chains/*/configs/wallets.yaml
rm -f "$GATEWAY_DIR"/chains/*/configs/wallets.yaml.*
rm -f "$GATEWAY_DIR"/os-server-configs/*/wallets.yaml
rm -f "$GATEWAY_DIR"/os-server-configs/*/wallets.yaml.*

rm -f "$HOME"/.foundry/*.password
rm -rf "$HOME"/.foundry/keystores
```

This is safe for normal node restarts, but it intentionally removes hot
repair/upgrade/admin capability. Restore the backup or re-import the relevant
signers before running checkpoint repair, governance, migration, or upgrade
commands.

For internet-facing prover access, keep the node prover APIs bound to
`127.0.0.1` and terminate HTTPS in the host nginx that already fronts RPC and
explorer traffic. The config generator writes nginx vhost files next to the node
configs:

```bash
"$GATEWAY_DIR/os-server-configs/gateway/prover-api.nginx.conf"
"$GATEWAY_DIR/os-server-configs/zksys/prover-api.nginx.conf"
```

If Gateway and zksys are on the same host, install the combined generated file:

```bash
"$GATEWAY_DIR/os-server-configs/install-prover-api-nginx.sh"
```

This uses the same host nginx TLS setup as RPC and explorer. Certificates for
the prover hostnames must already be provisioned by the host's normal nginx /
Let's Encrypt flow, with certs available at
`/etc/letsencrypt/live/<hostname>/`.

If they run on separate hosts, install the per-chain `prover-api.nginx.conf` on
the corresponding host instead. Override `GATEWAY_PROVER_API_DOMAIN` and
`EDGE_PROVER_API_DOMAIN` before generating configs if the prover hostnames are
not `prover-gw.dev11.top` and `prover-zk.dev11.top`.

Then start Airbender provers with a credentialed HTTPS sequencer URL, for
example `https://syscoin-prover:...@prover-gw.dev11.top` for Gateway and
`https://syscoin-prover:...@prover-zk.dev11.top` for zksys.

Generated configs bind the prover API to `127.0.0.1` by default. If you need
direct HTTP access instead of a proxy/VPN, set `PROVER_API_BIND_HOST=0.0.0.0`
when generating configs and give provers a URL such as
`http://syscoin-prover:...@node-host:3124`. This is not recommended over the
public internet because Basic Auth is not encrypted without HTTPS.

If the explorer runs on a separate host, keep sequencer/node RPC bound locally
or behind the host reverse proxy and allowlist only the explorer server's public
IP at nginx/firewall level. Do not expose unrestricted Gateway node RPC directly
to the internet.

For zksys public RPC, prefer running external nodes on the explorer host instead
of exposing the sequencer RPC directly. The normal topology is two zksys EN
processes on the explorer host:

- public EN: `rpc.enable_debug_namespace=false`, local nginx upstream for
  `rpc-zk.tanenbaum.io`
- internal/debug EN: `rpc.enable_debug_namespace=true`, used by Blockscout only

Each node process needs its own `network.secret_key`, `network.port`,
`general.rocks_db_path`, and `rpc.address`. Both ENs use the same trusted
sequencer boot node:

```text
network.boot_nodes = "enode://<zksys-main-peer-id>@<sequencer-host>:3060"
```

The sequencer zksys node must have p2p enabled first. Its private
`network.secret_key` stays on the sequencer; ENs only receive the public enode.
Use the sequencer helper to create/reuse that key and patch the zksys config:

```bash
cd scripts/explorer/blockscout

SEQUENCER_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
./enable-zksys-sequencer-p2p.sh
```

The helper prints `MAIN_NODE_ENODE=...`. By default it does not restart the
sequencer; set `RESTART_ZKSYS=1` after confirming the maintenance window.

The Blockscout helper can install the two zksys ENs after launch:

```bash
cd scripts/explorer/blockscout

REMOTE_HOST="ubuntu@<explorer-host>" \
SEQUENCER_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
MAIN_NODE_ENODE="enode://<zksys-main-peer-id>@<sequencer-host>:3060" \
./deploy-zksys-en-rpc.sh
```

The generated EN configs set `general.main_node_rpc_url` to the direct
sequencer RPC (`http://<sequencer-host>:3050` by default). After
`rpc-zk.tanenbaum.io` points at the public EN, do not use that DNS name for
`main_node_rpc_url`; otherwise EN transaction forwarding loops back into the EN
instead of reaching the sequencer.

Then install the public zksys RPC vhost on the explorer host, pointing at the
public EN and leaving the existing Gateway RPC vhost alone:

```bash
RPC_NGINX_REMOTE_HOST="ubuntu@<explorer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
ZKSYS_RPC_UPSTREAM="http://127.0.0.1:3050" \
RPC_NGINX_INCLUDE_ZKSYS=1 \
RPC_NGINX_INCLUDE_GATEWAY=0 \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

After the public zksys RPC has moved to the explorer host, remove the old zksys
RPC vhost from the sequencer while keeping Gateway RPC private/allowlisted:

```bash
RPC_NGINX_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
RPC_NGINX_INCLUDE_ZKSYS=0 \
RPC_NGINX_INCLUDE_GATEWAY=1 \
RPC_NGINX_REMOVE_ZKSYS=1 \
GATEWAY_RPC_ALLOWLIST="<explorer-host-ip-or-cidr>,<admin-ip-or-cidr>" \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

Also restrict the sequencer's raw zksys RPC port to the explorer host only; ENs
need this path for transaction forwarding, but it should not be publicly
reachable:

```bash
# Run on the sequencer host.
sudo iptables -I INPUT 1 -p tcp -s <explorer-host-ip> --dport 3050 -j ACCEPT
sudo iptables -I INPUT 2 -p tcp --dport 3050 -j DROP

# Persist the rules using your host firewall manager. On Ubuntu without another
# firewall manager, netfilter-persistent can save them:
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y iptables-persistent netfilter-persistent
sudo netfilter-persistent save
sudo systemctl enable netfilter-persistent
```

The Blockscout deployment helper includes a host nginx installer for the normal
Tanenbaum topology:

```bash
cd scripts/explorer/blockscout

# Deploy or refresh the explorer containers.
REMOTE_HOST="<ssh-user>@<explorer-host>" ./deploy-remote.sh zksys
REMOTE_HOST="<ssh-user>@<explorer-host>" ./deploy-remote.sh gateway

# Install RPC vhosts on the node host after rpc-zk/rpc-gw DNS points there.
RPC_NGINX_REMOTE_HOST="<ssh-user>@<node-host>" \
GATEWAY_RPC_ALLOWLIST="<blockscout-ip-or-cidr>,<admin-ip-or-cidr>" \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

`rpc-zk.tanenbaum.io` is installed as the public zksys RPC. `rpc-gw.tanenbaum.io`
is installed as a private Gateway RPC that only allows localhost plus
`GATEWAY_RPC_ALLOWLIST` entries, typically the Blockscout host and operator
admin IPs. The Gateway Blockscout env hides RPC docs/links and disables the
Next.js proxy so the public explorer API remains available without turning the
explorer into a public Gateway RPC passthrough.

The generated `start-node.sh` now preflights the open-file limit before starting the node:

- it tries to raise `ulimit -n` to `1048576`
- it warns if the resulting limit is below the recommended `131072`
- it fails fast if the resulting limit is below `65536`

If the script cannot raise the limit high enough, increase the shell / service hard limit first (for example via systemd `LimitNOFILE`, Docker `--ulimit`, or host user limits).

## Important env vars

| Variable | Purpose |
|---|---|
| `L1_RPC_URL` | Required HTTP(S) JSON-RPC endpoint used for broadcasts (expected: local Syscoin node/proxy, e.g. `http://127.0.0.1:8545`) |
| `GATEWAY_DIR` | Ecosystem workspace path (default `~/gateway`) |
| `REUSE_ECOSYSTEM` | Set to `true` only when intentionally reusing an existing `$GATEWAY_DIR/ZkStack.yaml`; equivalent to `--reuse-ecosystem` |
| `MIGRATE_EDGE` | Set to `true` only when intentionally pausing deposits and migrating/finalizing the edge chain; equivalent to `--migrate-edge` |
| `GATEWAY_ARCHIVE_L1_RPC_URL` | Recommended runtime archive RPC URL for gateway node + migration startup (if unset, falls back to `L1_RPC_URL`) |
| `PROVER_API_BIND_HOST` | Prover API bind host for generated node configs; defaults to `127.0.0.1` so public access should go through HTTPS/VPN/reverse proxy termination |
| `GATEWAY_PROVER_API_DOMAIN` / `EDGE_PROVER_API_DOMAIN` | Hostnames for generated nginx prover API vhosts; defaults are `prover-gw.dev11.top` and `prover-zk.dev11.top` |
| `PROVER_API_AUTH_USER` / `PROVER_API_AUTH_PASSWORD` | Basic Auth credentials for remote prover API access; password is required for generated configs |
| `FUNDER_SIGNER` | Funder signer backend: `account` (default on Tanenbaum/Mainnet), `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `FUNDER_ACCOUNT_NAME` | Foundry keystore account name when `FUNDER_SIGNER=account` (default `funder`) |
| `FUNDER_KEYSTORE` | Keystore file path when `FUNDER_SIGNER=keystore` |
| `FUNDER_PASSWORD_FILE` | Optional keystore password file passed to Cast without exposing the password in argv |
| `FUNDER_PRIVATE_KEY` | Local/disposable-network fallback for `FUNDER_SIGNER=private-key`; rejected on Tanenbaum/Mainnet by default |
| `GATEWAY_FUND_WALLETS_PATHS` | Optional extra `wallets.yaml` paths to fund (colon-separated) |
| `PROVER_MODE` | `gpu` (default) or `no-proofs` |
| `PROTOCOL_VERSION` | Default `v31.0` |
| `GATEWAY_CHAIN_ID` | Gateway / zkSYS chain id used by ecosystem and node config generation |
| `ZKSYS_L2_CREATE2_DEPLOYER` | Deterministic L2 CREATE2 deployer for canonical zkSYS; defaults to `0x4e59b44847b379578588920cA78FbF26c0B4956C` |
| `ZKSYS_L2_TOKEN_ADMIN_ADDRESS` | Required on mainnet and whenever `ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE=true`; initial token role admin and default owner of deterministic zkSYS `ProxyAdmin` contracts; also used during L1 launch to derive deterministic zkSYS L2 addresses and the v31 `zk_token_asset_id` for canonical L2 zkSYS |
| `ZKSYS_L2_PROXY_ADMIN_SALT` | Optional bytes32 salt for the deterministic zkSYS `ProxyAdmin` deployment |
| `ZKSYS_L2_TOKEN_IMPL_SALT` / `ZKSYS_L2_TOKEN_PROXY_SALT` | Optional bytes32 salts for deriving the canonical L2 zkSYS implementation/proxy addresses |
| `ZKSYS_L2_RPC_URL` | Required by `scripts/gateway-launch/zksys-l2-bootstrap.sh` to deploy the L2 zkSYS suite after the chain is live |
| `ZKSYS_L2_DEPLOYER_SIGNER` | L2 bootstrap signer backend: `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key`; defaults to `DEPLOYER_SIGNER`, then `FUNDER_SIGNER`, then `account` unless `ZKSYS_L2_DEPLOYER_PRIVATE_KEY` is set |
| `ZKSYS_L2_DEPLOYER_ACCOUNT_NAME` / `ZKSYS_L2_DEPLOYER_KEYSTORE` / `ZKSYS_L2_DEPLOYER_PASSWORD_FILE` | Optional L2 bootstrap signer inputs for account or keystore modes; fall back to the matching deployer/funder values when unset |
| `ZKSYS_L2_DEPLOYER_PRIVATE_KEY` | Local/disposable-network fallback for `ZKSYS_L2_DEPLOYER_SIGNER=private-key`; rejected on Tanenbaum/Mainnet unless `GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true` |
| `ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE` | Deploy the L1/NEVM registry bridge during `gateway-deploy-l1.sh`; defaults to `true` |
| `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS` | Optional owner for the L1 registry bridge `ProxyAdmin`; defaults to `ZKSYS_L2_TOKEN_ADMIN_ADDRESS` |
| `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_SALT` / `ZKSYS_L1_REGISTRY_BRIDGE_IMPL_SALT` / `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_SALT` | Optional bytes32 salts for deterministic L1 registry bridge proxy admin, implementation, and proxy deployments through the L1 CREATE2 factory |
| `ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK` | Syscoin `nNEVMStartBlock` used to convert absolute UTXO collateral heights into NEVM-local seniority age; defaults to `1317500` |
| `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1` / `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT2` | Effective post-NEVM seniority thresholds; default `210240` / `525600` |
| `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS` / `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS` | Seniority bonuses in basis points; default `0` / `0` for first-year launch without seniority multiplier |
| `ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS` | Optional L1 registry bridge address override for the L2 bootstrap. If unset/zero, `zksys-l2-bootstrap.sh` reads the address persisted by `gateway-deploy-l1.sh` in `configs/contracts.yaml` |
| `ZKSYS_L2_REGISTRY_IMPL_SALT` / `ZKSYS_L2_REGISTRY_PROXY_SALT` | Optional bytes32 salts for deterministic L2 membership fact registry implementation/proxy deployments; the proxy address is the operational registry address wired to the L1 bridge |
| `ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT` / `ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT` | Optional bytes32 salts for deterministic L2 reward weight registry implementation/proxy deployments; the proxy address is wired as the membership registry receiver |
| `ZKSYS_L2_ISSUER_IMPL_SALT` / `ZKSYS_L2_ISSUER_PROXY_SALT` | Optional bytes32 salts for deterministic L2 issuer implementation/proxy deployments; the proxy address receives the token minter role |
| `ZKSYS_L2_STAKING_VAULT_IMPL_SALT` / `ZKSYS_L2_STAKING_VAULT_PROXY_SALT` | Optional bytes32 salts for deterministic L2 native SYS staking vault implementation/proxy deployments; the proxy address receives the reward weight updater role |
| `ZKSYS_ISSUER_START_TIME` | Required by L2 bootstrap; UNIX timestamp when algorithmic zkSYS issuance periods begin |
| `ZKSYS_ISSUER_PERIOD_SECONDS` | Issuance period length; defaults to `86400`; must multiply with `ZKSYS_ISSUER_PERIODS_PER_YEAR` to exactly `365 days` |
| `ZKSYS_ISSUER_PERIODS_PER_YEAR` | Number of issuance periods in each schedule year; defaults to `365`; must multiply with `ZKSYS_ISSUER_PERIOD_SECONDS` to exactly `365 days` |
| `ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS` | Reward-weight activation delay for positive native stake and Sentry Node weight changes; defaults to `3` periods and must be `1..7` |
| `ZKSYS_L2_PAYMASTER_ADDRESS` | Optional known deterministic Pali paymaster address granted the token burn role during L2 bootstrap |
| `PAYMASTER_GRANT_BURNER_ROLE` | Pali paymaster deploy helper option; defaults to `true` and grants zkSYS `BURNER_ROLE` to the deployed paymaster, set `false` only if role wiring is handled separately before use |
| `ZKSYNC_ERA_PATH` | Optional custom era checkout; otherwise launcher manages pinned workspace |
| `ZKSYNC_OS_DEV_PATH` | Optional custom upstream `zksync-os` checkout to patch for the `v31` dev proving line; otherwise launcher manages it under `$GATEWAY_DIR/.gateway-launch/zksync-os/` |
| `ZKSYNC_OS_GIT_URL` | Optional override for the upstream `zksync-os` Git URL used when launcher materializes the patched `dev` workspace; the repo must contain the `Cargo.lock`-pinned commit |
| `GATEWAY_CREATE2_FACTORY_SALT` | Optional deterministic `create2_factory_salt` override for L1 deployment |
| `GATEWAY_WALLET_PATH` | Wallet file used for gateway ecosystem create (`in-file` if present, else random+persist) |
| `EDGE_WALLET_PATH` | Wallet file used for edge chain create (`in-file` if present, else random+persist) |
| `EDGE_GATEWAY_L1_DA_VALIDATOR_ADDR` | Optional override for DA validator used by edge migration repair on Gateway |
| `DEPLOYER_SIGNER` | Optional L1 deployer signer override for direct Forge deployment/retry broadcasts; defaults to `FUNDER_SIGNER` on Tanenbaum/Mainnet and supports `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `DEPLOYER_ACCOUNT_NAME` | Foundry keystore account name when `DEPLOYER_SIGNER=account` (default: `FUNDER_ACCOUNT_NAME`, then `funder`) |
| `DEPLOYER_KEYSTORE` | Keystore file path when `DEPLOYER_SIGNER=keystore` (default: `FUNDER_KEYSTORE`) |
| `DEPLOYER_PASSWORD_FILE` | Optional keystore password file passed to Forge without exposing the password in argv (default: `FUNDER_PASSWORD_FILE`) |
| `EDGE_GATEWAY_GOVERNOR_SIGNER` | Optional governor signer override for Gateway migration repairs; defaults to `FUNDER_SIGNER` and supports `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME` | Foundry keystore account name when `EDGE_GATEWAY_GOVERNOR_SIGNER=account` (default: `FUNDER_ACCOUNT_NAME`, then `funder`) |
| `EDGE_GATEWAY_GOVERNOR_KEYSTORE` | Keystore file path when `EDGE_GATEWAY_GOVERNOR_SIGNER=keystore` (default: `FUNDER_KEYSTORE`) |
| `EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE` | Optional keystore password file passed to Forge without exposing the password in argv (default: `FUNDER_PASSWORD_FILE`) |
| `BITCOIN_DA_RPC_URL` / `BITCOIN_DA_RPC_USER` / `BITCOIN_DA_RPC_PASSWORD` | DA connectivity for gateway blobs mode |

## Notes

- Default `FOUNDRY_EVM_VERSION` remains `shanghai`.
- `run-gateway-launch.sh` still enforces L1 chain-id preflight before broadcast steps.
- Migration safety guards remain in `edge-chain-migrate-to-gateway.sh` (DA bytecode checks, idempotent pause/unpause behavior).
- For Tanenbaum/Mainnet launches, keep `L1_RPC_URL` on local Syscoin RPC and set `GATEWAY_ARCHIVE_L1_RPC_URL` to the archive/public endpoint.
- On mainnet, `gateway-deploy-l1.sh` derives `ZKSYS_ZK_TOKEN_ASSET_ID` from the deterministic L2 zkSYS proxy address and the v31 L2 native token vault address `0x0000000000000000000000000000000000010004`, then exports it for zkstack CTM deployment. v31 uses this asset id only for InteropCenter's optional fixed zkSYS fee path; the default interop fee path remains base-token `msg.value` in SYS.
- Canonical zkSYS CREATE2 bytecode derivation uses `forge inspect --no-metadata` so Solidity metadata and local remapping paths do not affect deterministic addresses. Discard any zkSYS CREATE2 addresses or `ZKSYS_ZK_TOKEN_ASSET_ID` values calculated from metadata-bearing bytecode.
- Canonical Pali ERC-4337 infrastructure bytecodes are also generated without Solidity metadata; keep `pali-wallet` deployment constants and the committed Blockscout/Sourcify standard JSON inputs in sync when regenerating them. The SLH-DSA validator constructor pins the verifier runtime code hash.
- The prover API is plain HTTP in the node process. For internet-reachable provers, keep `PROVER_API_BIND_HOST=127.0.0.1` and expose it through HTTPS, VPN, or another trusted transport that forwards the Basic Auth header to the node.
- Changing `GATEWAY_CREATE2_FACTORY_SALT` resets checkpoint state automatically (new redeploy run context).
- After the chain is live, run `scripts/gateway-launch/zksys-l2-bootstrap.sh` to deploy the canonical L2 zkSYS `ProxyAdmin`, transparent proxy, implementation, membership fact registry, reward weight registry, and algorithmic issuer with deterministic CREATE2 salts, then wire issuer minting, membership-to-weight callbacks, weight-to-issuer callbacks, optional L1 registry bridge authority, and optional burn rights for the known deterministic Pali paymaster. The script verifies the final role and receiver wiring before exiting. The token admin receives role-admin authority for recovery and later governance transfer, but not direct `MINTER_ROLE` / `BURNER_ROLE`.
- The membership registry mirrors NEVM facts from the L1 `0x62` precompile and exposes the active Sentry Node address set for offchain diffing. The L1 registry bridge derives each Sentry Node's seniority-weighted reward weight from raw Syscoin collateral age (`nNEVMStartBlock + block.number - collateralHeight`) and sends that final weight to L2. For mainnet, use effective post-NEVM seniority thresholds `210240` and `525600` blocks with levels `3500` and `10000` bps. Native SYS staking is handled by the L2 staking vault.
- Reward weight increases are not active immediately: native SYS deposits, Sentry Node additions, and Sentry Node seniority increases are queued for `ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS` periods and require the account to call `activatePendingWeight()` after the delay. Weight decreases and removals apply immediately. This prevents a stake or Sentry weight increase submitted just before a period boundary from earning the completed period.
- The issuer uses a fixed remaining-cap curve: 20% in schedule year 1, 12% in year 2, 8% in year 3, then 5% per year afterward. Each annual amount is released pro-rata over `ZKSYS_ISSUER_PERIODS_PER_YEAR` periods, so scheduled issuance approaches but never exceeds the 210M zkSYS cap.
- If you switch prover mode (`PROVER_MODE` / effective `GATEWAY_PROVER_MODE`) between runs, clear checkpoint state first: `rm -rf "${GATEWAY_DIR:-${HOME}/gateway}/.gateway-launch"`.
- During `gl.l1_ecosystem_deployed`, launcher clears `os-server-configs/gateway/db` before redeploy to avoid stale replay assertion panics.
- During `gl.edge_chain_inited`, launcher clears `os-server-configs/zksys/db` (or configured edge chain name) before re-init for the same reason.
- For `v31.x`, `start-node.sh` runs through a launcher wrapper that copies the current `zksync-os-server` tree into `$GATEWAY_DIR/.gateway-launch/zksync-os-server/`, rewrites only the `*_dev` `zksync-os` deps to the patched upstream checkout, and uses that isolated workspace for `cargo run`.
- High-TPS runs can exhaust low default `nofile` limits; use at least `65536`, with `131072+` recommended.
