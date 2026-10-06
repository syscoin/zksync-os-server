<!-- SYSCOIN: Operator-specific v32 public Tanenbaum service handoff. This is
staged evidence, not authorization to stop, install, reload or start anything. -->

# Staged sequencer service handoff

Status: templates and deterministic adapter recipe prepared; **not installed,
not started**. Fresh canonical node configs/start scripts and the edge binary
are still pending. No generated adapter hash, completed deployment receipt,
live acceptance or issuance timestamp is asserted here.

These templates are for the approved **Tanenbaum mock-proving testnet only**.
Do not copy their chain IDs, network, no-proofs flags or addresses to mainnet.
The reviewed source pin is
`f638db92e5c5ea31507e089b61f122fd95cf9083`, checkout
`/home/ubuntu/zksync-os-server-v32-public-20261006`, runtime
`/home/ubuntu/gateway_v32_public_20261006`, staging
`/home/ubuntu/v32-services-prep-20261006`.

## Required inputs and boundary

1. Parent approves the fresh live root graph, canonical app identity and exact
   operator-selected old-v31 retirement boundary. Legacy intake is **not**
   represented as paused without mined pause receipts. A deliberate disposable
   testnet exception leaving old contracts accepting manual deposits requires
   explicit operator approval and disclosure; the decision is still pending.
2. Parent fences old L2 consumers and nodes in the coordinated order. Preserve
   custody, TLS and external/native collateral, L1/Sepolia/R2/shared bridge
   history. No old-v31 rollup DB/index backup is required. A NativeIngress
   implementation upgrade is not approved or completed by this staging task.
3. Canonical `run-gateway-launch.sh` owns its temporary Gateway process during
   edge initialization and migration. Complete/revalidate the migration and
   final OS-config checkpoints using that lifecycle before service handoff.
   It refuses an independent Gateway listener and must never be given a
   forged ownership PID or forced to adopt the systemd node. Confirm its owned
   Gateway, validator and foreground jobs have exited and ports are free.
4. Require the generated fresh `os-server-configs/{gateway,zksys}` config,
   contracts, private wallets, genesis and `start-node.sh`; source/receipt
   bindings and `generate-os-server-configs.sh --check-only` must pass against
   the same protected launch inputs. No config copied from v31 is valid.
5. Gateway's immutable genesis stamp must already have been established by the
   canonical owned first-start attestation. Persistent service re-attestation
   uses `allow_genesis_stamp_creation=false`; do not create it from a systemd
   PID or fabricate a stamp. Restart/recovery must never replay a partially
   broadcast deployment or reset its journal.

## Canonical binaries and adapter generation

Current independently read Gateway artifact (not a future build guarantee):

| Field | Value |
| --- | --- |
| Bytes | 161921304 |
| SHA256 | `1b159028d092bb5145d528a7d2391cdcb0ea73e8c67e4119141e6b14def833cf` |
| Context stamp | `3e24100f3991e821ebed9efa8747a2a08fddcf4e8a175fe9aba866babee51477` |

Its canonical f638 `build-prebuilt` and `exec-prebuilt -- --help` passed. The
launcher may rebuild it during migration; re-record the **actual final**
artifact/stamp and verify normally, without manually recomputing/restamping.
Copied dependency caches never substitute for a final binary/stamp.

After fresh edge contracts/configs exist, use the same protected launcher
environment, with the static-context bypass unset. These are **future commands,
not executed by this staging task**:

```bash
# SYSCOIN: Failed custody/source preconditions must stop this command sequence.
set -euo pipefail
# SYSCOIN: Set this to the exact protected launch.env.sh from the reviewed
# deployment. No filename/value is inferred from shell history or this example.
: "${V32_LAUNCH_ENV_FILE:?set the approved absolute protected launch.env.sh path}"
[[ "${V32_LAUNCH_ENV_FILE}" = /* ]]
test -f "${V32_LAUNCH_ENV_FILE}"
test ! -L "${V32_LAUNCH_ENV_FILE}"
test -O "${V32_LAUNCH_ENV_FILE}"
test "$(stat -c %a "${V32_LAUNCH_ENV_FILE}")" = 600
source "${V32_LAUNCH_ENV_FILE}"
export ZKSYNC_OS_SERVER_PATH=/home/ubuntu/zksync-os-server-v32-public-20261006
cd "${ZKSYNC_OS_SERVER_PATH}"
# SYSCOIN: Bind the same reviewed inputs; account passwords/private keys are
# never copied into the persistent service environment or reference assets.
export GATEWAY_DIR=/home/ubuntu/gateway_v32_public_20261006
export GATEWAY_CHAIN_NAME=gateway EDGE_CHAIN_NAME=zksys
export GATEWAY_CHAIN_ID=57001 EDGE_CHAIN_ID=57057 PROTOCOL_VERSION=v32.0
export PROVER_MODE=no-proofs GATEWAY_PROVER_MODE=no-proofs EDGE_PROVER_MODE=no-proofs
export SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=true L1_NETWORK=tanenbaum L1_CHAIN_ID=5700
unset ZKSYNC_OS_STATIC_BUILD_CONTEXT CARGO_BUILD_TARGET
CARGO_NET_GIT_FETCH_WITH_CLI=true CARGO_BUILD_JOBS=4 \
  bash scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh zksys -- build-prebuilt
bash scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh zksys \
  -- exec-prebuilt -- \
  --config /home/ubuntu/gateway_v32_public_20261006/os-server-configs/zksys/config.yaml --help
bash scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh gateway \
  -- exec-prebuilt -- \
  --config /home/ubuntu/gateway_v32_public_20261006/os-server-configs/gateway/config.yaml --help
PYTHONDONTWRITEBYTECODE=1 python3 \
  /home/ubuntu/v32-services-prep-20261006/generate-edge-prebuilt-adapter.py
```

The recipe refuses wrong/dirty source, changed canonical helper hashes,
unsafe/missing generated files, ambiguous context/lock/cookie anchors or any
change to the exact final config argv. It copies the canonical edge prefix
byte-identically and changes only the final runner mode
`run --release --` to `exec-prebuilt --`. It neither changes source/configs
nor builds, installs, starts, signs or writes a native build stamp. Output is
owner-only under `generated/`; no existing output is overwritten. Review:

- `generated/zksys-start-node-canonical.sh` (preserved original)
- `generated/zksys-start-node-prebuilt.sh` (only terminal mode changed)
- `generated/edge-adapter-generation.json` (source, generator and both hashes)

The canonical cookie refresh, generated context, exact config arguments and
execute-operator advisory lock remain unchanged. The lock stays on FD9 across
exec under the canonical runtime `.gateway-launch-locks` directory; do not
unlink it, invent a second lock or run an extra edge sender. Normal runner
source/binary/protocol/app/context checks still execute on every start. No
static context or handwritten stamp is allowed. Gateway directly uses its
unmodified canonical `start-node.sh`.

## Future approved installation and acceptance

**Stop here until parent explicitly authorizes installation after all gates.**
Before copying, confirm these new service names are not occupied by unrelated
units. Compare the reviewed staging file hashes with `service-stage-record.json`.
Do not enable/start units while a canonical migration/repair lifecycle owns a
Gateway process. An `After=` ordering relationship is not a readiness probe:
start/attest Gateway first, then edge.

```bash
# SYSCOIN: Do not overwrite a pre-existing unrelated unit after a failed check.
set -euo pipefail
cd /home/ubuntu/v32-services-prep-20261006
test ! -e /etc/systemd/system/zksys-v32-gateway.service
test ! -e /etc/systemd/system/zksys-v32-edge.service
systemd-analyze verify zksys-v32-gateway.service zksys-v32-edge.service
sudo install -m 0644 zksys-v32-gateway.service /etc/systemd/system/zksys-v32-gateway.service
sudo install -m 0644 zksys-v32-edge.service /etc/systemd/system/zksys-v32-edge.service
sudo systemctl daemon-reload
sudo systemctl start zksys-v32-gateway.service
# SYSCOIN: Parent now authenticates exact systemd MainPID executable/config,
# listening socket, chain ID, root graph and canonical live postimages, with
# the existing immutable genesis stamp. Do not proceed on chain ID alone.
# Once Gateway identity/readiness passes:
sudo systemctl start zksys-v32-edge.service
# SYSCOIN: Parent now authenticates edge MainPID + FD9 lock, fresh genesis,
# root/Gateway settlement graph, protocol/VK/security profile, queue progress,
# required bootstrap contracts and the native bridge acceptance before public
# completion. Raw journal/config output may contain secrets; redact it.
# Only after acceptance, persist boot startup:
sudo systemctl enable zksys-v32-gateway.service zksys-v32-edge.service
```

For the Gateway identity probe, obtain `MainPID` from `systemctl show` and use
canonical `gl_assert_gateway_runtime_identity "$pid" false` against the exact
generated local RPC in the protected parent environment. This is a read-only
re-attestation, not permission to mark the PID migration-owned. Verify native
binary/config argv and socket ownership before/after the probe. Require edge
GasTank presence once canonical bootstrap persists its deployment marker;
retain the normal first-boot policy before that, not a permanent bypass.

Units run as ubuntu, use exact source cwd, a minimal nonsecret PATH/HOME/mock
environment, explicit existing Syscoin cookie path, UMask0077 and nofile1048576.
They explicitly remove inherited static-build and migration-owned PID/FD
context, do not source deployment account passwords, and do not introduce any
Core/Geth service dependency. Control-group stop applies only to each unit's descendants;
never kill Core/Geth or broadly kill matching cargo/node names. Before any
future canonical migration repair, explicitly coordinate an approved stop of
these persistent units so the lifecycle can own its exact local Gateway again.
