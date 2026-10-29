# Proof gate deployment stage

`deploy_gate.py` is a post-deployment stage for **one** settlement lane. Run it
separately for the zkSYS gate on Gateway and the Gateway gate on NEVM, with a
separate protected journal and signer for each lane. It deploys the lane's
priority guard and proof gate together using the repository's actual constructor
bytecode. It does not deploy the checkpoint source/receiver, root sink, roster
receivers, draws, or coordinators, change timelock roles, or activate the service.
The component stage below can provision those components; all bindings and
remaining activation steps must still be verified separately.

Create a private JSON config and an existing owner-only journal directory. The
config is the complete object below; all addresses, hashes and values must come
from the intended deployment. The signer must be an owner-only Foundry keystore
with an owner-only password file. This signer remains the gate factory's owner:
it must be retained to install the coordinator after bootstrap.

```json
{
  "chain_id": 57,
  "genesis_hash": "0x<32-byte NEVM genesis hash>",
  "sender": "0x<factory owner and transaction signer>",
  "chain": "0x<native settlement diamond for this execution lane>",
  "chain_code_hash": "0x<32-byte runtime hash>",
  "timelock": "0x<native prover timelock>",
  "timelock_code_hash": "0x<32-byte runtime hash>",
  "checkpoint": "0x<root priority source or Gateway checkpoint receiver>",
  "checkpoint_code_hash": "0x<32-byte runtime hash>",
  "message_sink": "0x<NEVM service sink, or zero address for Gateway gate>",
  "message_sink_code_hash": "0x<32-byte runtime hash, or zero for Gateway gate>",
  "sequencer": "0x<service sequencer>",
  "policy_hash": "0x<32-byte service policy hash>",
  "production_vk_hash": "0x<32-byte production VK hash for this lane>",
  "child_chain_id": 57001,
  "max_snapshot_age": 600,
  "max_proof_work_seconds": 1200,
  "max_priority_per_package": 64,
  "factory_salt": "0x<unique 32-byte deployment salt>",
  "create2_code_hash": "0x<verified runtime hash of canonical CREATE2 factory>",
  "gas_limit": 30000000,
  "gas_price": 1000000000,
  "keystore": "/absolute/private/path/keystore.json",
  "password_file": "/absolute/private/path/password"
}
```

The numbers above are examples, not deployed policy recommendations. For the
Gateway lane, `chain_id` is the Gateway chain ID, `child_chain_id` is zkSYS,
`chain` is the zkSYS diamond on Gateway, `checkpoint` is the Gateway receiver,
and the sink address/hash are zero. For the NEVM lane, `chain_id` is the NEVM
chain ID, `child_chain_id` is Gateway, `chain` is the Gateway diamond on NEVM,
`checkpoint` is its NEVM priority source, and `message_sink` is the NEVM service
sink. Both lanes must use their own pinned native contracts, verifier and VK.

First compute the deterministic addresses without any on-chain action:

```sh
python3 scripts/prover-service/deploy_gate.py --config /private/root-gate.json --plan
```

The result is explicitly `unverified_plan`: no RPC or runtime checks are made.
The root sink must be deployed with its publisher set to the planned NEVM gate;
its child receiver must match the independently predicted zkSYS receiver. A plan
can use a placeholder sink address/hash while deriving the gate address because
the gate's CREATE address depends on its factory, not its constructor inputs.
Replace those placeholders with the actual sink and reviewed hash before sending.
Use the same reviewed compiler, owner and salt for planning and deployment.

Once the lane's prerequisite contracts exist, set
`ZKSYS_SERVICE_SETTLEMENT_RPC_URL` to its RPC URL (optionally set
`ZKSYS_SERVICE_SETTLEMENT_RPC_AUTHORIZATION`), and run:

```sh
python3 scripts/prover-service/deploy_gate.py --config /private/root-gate.json \
  --state-dir /private/root-gate-state
python3 scripts/prover-service/deploy_gate.py --config /private/root-gate.json \
  --state-dir /private/root-gate-state --send
```

The first command checks the chain and pinned prerequisites without sending.
Repeat the second command as needed: it submits at most one stage, saves signed
bytes before broadcast, waits for a finalized receipt on later runs, and retries
only those same bytes if they disappear from the mempool. The guard requires all
verified batches executed and the native priority queue drained when the guard
and gate are created. A failed or inconsistent transaction stops the stage;
retain its journal for diagnosis.

After actual bootstrap proof acceptance and coordinator deployment, pass
`--coordinator 0x... --coordinator-code-hash 0x... --send` with the same config
and journal to install it. The gate checks the coordinator's accepted parent,
next batch, lane, policy, VK, roster and sequencer. The script checks the
supplied coordinator runtime hash before submission. Resume an in-flight
installation with the same coordinator and hash.

This stage has not been exercised against live Gateway or NEVM infrastructure.
It does not alone establish a qualified roster or make proving available through
the gate. Both lanes, their supervised prover-role handoff, checkpoint and roster
relays, real duty reports and activation messages must be completed before the
service can start.

For the supervised fresh launch, stop the initial proof sender and reconcile all
of its submitted proof transactions before deploying the gate and guard. Use the
existing chain administrator to grant the gate the prover role and remove that
role from the initial sender. The gate requires itself to be the sole holder of
the role; confirm this and the guard's native batch and priority cursors before
starting gate submissions. Perform this for each proof lane. The Gateway role
administrator may be the aliased L1 ChainAdmin, so use the existing authorized
L1-to-Gateway administrator route and verify each finalized Gateway operation.
No new cutover contract is required.

## Priority checkpoints and root message sink

`deploy_component.py` provides the preceding stages for the NEVM priority
sources, Gateway checkpoint receiver, and NEVM service message sink. It deploys
one component per configuration and journal. Each uses a deterministic factory
whose first CREATE address is independent of the component's constructor data.
This permits the NEVM child-priority source and Gateway receiver to name each
other before either exists. It also lets the root sink name the planned NEVM
gate and the predicted zkSYS accepted-message receiver.

Each private JSON file has these common fields, with real deployment values:

```json
{
  "kind": "root_priority_source",
  "chain_id": 57,
  "genesis_hash": "0x<32-byte genesis hash>",
  "sender": "0x<component factory owner and transaction signer>",
  "factory_salt": "0x<unique 32-byte salt for this chain and component>",
  "create2_code_hash": "0x<verified canonical CREATE2 factory runtime hash>",
  "gas_limit": 9000000,
  "gas_price": 1000000000,
  "keystore": "/absolute/private/path/keystore.json",
  "password_file": "/absolute/private/path/password",
  "policy_hash": "0x<32-byte service policy hash>"
}
```

Add exactly the fields for the selected kind:

- `root_priority_source`, deployed on NEVM: `mailbox`, `mailbox_code_hash`,
  `bridgehub`, `bridgehub_code_hash`, `child_chain_id`, `gateway_receiver`,
  `gateway_chain_id`, and `inclusion_delay`. Deploy one source for the zkSYS
  priority queue, with `gateway_receiver` set to the planned Gateway receiver
  and `gateway_chain_id` set to Gateway. Deploy a second source for the Gateway
  queue with `child_chain_id` set to Gateway and both `gateway_receiver` and
  `gateway_chain_id` set to zero. Both sources require the corresponding native
  priority queue to be drained and all verified batches executed.
- `gateway_checkpoint_receiver`, deployed on Gateway: `root_source` is the
  planned NEVM zkSYS priority source, and `child_chain_id` is zkSYS.
- `root_message_sink`, deployed on NEVM: `publisher` is the planned NEVM proof
  gate and `gateway_gate` is the planned Gateway proof gate; `bridgehub` and
  `bridgehub_code_hash` pin the root Bridgehub;
  `gateway_chain_id`, `gateway_chain_address`, and `gateway_chain_code_hash`
  pin Gateway's native NEVM diamond; `registry_chain_id` and `registry_receiver`
  identify zkSYS and its predicted accepted-message receiver.

- `roster_receiver`: `child_chain_id`, `child_registry`, `message_sender`,
  `mailbox`, `mailbox_code_hash`, `mailbox_chain_id`, `start_time`,
  `period_seconds`, and `first_service_period`. Deploy one for the zkSYS roster
  on Gateway and one for the forwarded Gateway roster on NEVM. Pin the correct
  child mailbox and sender for each lane and use the same service clock.
- `root_draw_receiver`: `root_source` is the planned NEVM root draw address.
- `root_draw`: `mailbox`, `mailbox_code_hash`, `bridgehub`,
  `bridgehub_code_hash`, `gateway_coordinator`, `gateway_receiver`,
  `gateway_chain_id`, `future_blocks`, and `confirmations`. The coordinator and
  receiver are their planned Gateway addresses.
- `native_root_draw`: `coordinator`, `future_blocks`, and `confirmations`.
  The coordinator is the planned NEVM coordinator address.
- `coordinator`: `gate`, `gate_code_hash`, `root_source`,
  `root_source_code_hash`, `roster_source`, `roster_source_code_hash`,
  `sequencer`, `child_chain_id`, `child_chain_address`,
  `child_chain_code_hash`, `production_vk_hash`, `initial_parent`,
  `first_batch`, `first_service_period`, `turn_seconds`, and boolean
  `native_root_draw`. The last bootstrap accepted-package hash and the native
  diamond's next verified batch must still match at deployment and coordinator
  installation. Quiesce proof submission between those two operations; otherwise
  the gate rejects the installation and a new coordinator plan may be needed.

For each component, first use `--plan` with its config:

```sh
python3 scripts/prover-service/deploy_component.py --config /private/component.json --plan
```

The plan is unverified and performs no RPC calls. Initial plans may use nonzero
placeholder peer addresses; replace those with the corresponding planned peer
addresses before any submission. A component's planned address depends on its
owner, salt and factory bytecode, so changing a peer constructor address does
not change that plan. The init-code commitment does change. Use a distinct salt
and state directory for every component on a given chain.

After filling and independently checking all identities, point
`ZKSYS_SERVICE_SETTLEMENT_RPC_URL` at that component's settlement chain. Run
without `--send` to check prerequisites; run with `--send` and repeat to progress
through finalized factory and component deployment. The journal persists the
signed bytes and rebroadcasts only the same bytes. After finality the script
checks the factory commitment and every component binding before reporting the
address. The stage does not publish checkpoints or relay service messages.


## Prepare deployment configs together

`prepare_deployments.py` resolves planned peer addresses across the component and
proof-gate configurations, including reciprocal dependencies. Make a private
manifest with the following structure. Each `config` is the complete JSON object
for that kind described above; all other addresses, chain identities, hashes,
signer paths, gas settings, and policy settings are supplied explicitly by the
operator.

```json
{
  "schema_version": 1,
  "stages": [
    {"id": "root-source", "type": "component", "config": {"...": "..."}},
    {"id": "gateway-receiver", "type": "component", "config": {"...": "..."}},
    {"id": "gateway-gate", "type": "gate", "config": {"...": "..."}}
  ]
}
```

In a config, a peer address can be written as, for example,
`{"ref":"gateway-receiver.component"}` or `{"ref":"gateway-gate.gate"}`.
Component stages expose `factory` and `component`; gate stages expose `factory`,
`guard`, and `gate`. Use a distinct sender/salt factory for each stage on a chain.
Runtime code hashes are not predicted: provide the reviewed hashes once those
contracts are deployed, and prepare a new output directory for later stages.

```sh
python3 scripts/prover-service/prepare_deployments.py \
  --manifest /private/service-deployment-manifest.json \
  --output-dir /private/service-deployment-configs
```

The parent directory must already exist and be private. The new output directory
contains one private JSON config and one private state directory per stage, plus
`plans.json`. Follow the individual deployment commands above with the matching
config, state directory, and settlement RPC. These are unverified plans: this
command does not deploy contracts, check chain state, relay messages, or start a
prover service. Keep using the same config and state directory when resuming a
submitted stage.

## Diagnose the direct Gateway-message receiver

`prepare_message_relay.py` is a diagnostic for the existing direct verifier; it
does not prepare the NEVM sink route. A Gateway-origin direct-L1 batch proof is
not supported by the zkSYS shared-root verifier. The dual-lane service instead
uses the pinned NEVM sink's `relayChildAccepted` and `relayChildActivation`
with a Gateway batch proof and then tracks the resulting Bridgehub transaction
through finalized zkSYS execution. The proof extraction, fee-funded relay,
durable retry and destination confirmation remain manual operations and require
real Gateway-to-NEVM-to-zkSYS integration testing. Child-only configuration has
no NEVM sink and is not supported by this repair.

For the diagnostic only, supply a private config with these fields:

```json
{
  "gateway_chain_id": 57001,
  "child_chain_id": 57057,
  "gateway_genesis_hash": "0x<32-byte Gateway genesis hash>",
  "child_genesis_hash": "0x<32-byte zkSYS genesis hash>",
  "gateway_gate": "0x<Gateway proof-gate address>",
  "receiver": "0x<zkSYS accepted-message receiver>",
  "receiver_code_hash": "0x<reviewed receiver runtime hash>",
  "sender": "0x<address that would submit the receiver call>",
  "transaction_hash": "0x<Gateway gate transaction hash>",
  "transaction_log_index": 0,
  "proof_target": "l1BatchRoot"
}
```

The chain IDs and index here are examples. `transaction_log_index` is the
transaction-local L2-to-L1 log index, not the batch-wide proof index. Choose
`l1BatchRoot` or `messageRoot` according to the actual source topology; the
Gateway RPC rejects `messageRoot` for a chain settled directly on L1. An
activation request JSON is `{"kind":"activation","vk_hash":"0x<32-byte VK
hash>","roster_root":"0x<32-byte roster root>"}`. An acceptance request is
`{"kind":"accepted","accepted_package":{...},"duties":[...],"bootstrap":true}`;
use the exact package and duty objects produced for the submitted gate transaction.

Set `ZKSYS_SERVICE_GATEWAY_RPC_URL` and `ZKSYS_SERVICE_REGISTRY_RPC_URL`, with
optional corresponding `ZKSYS_SERVICE_GATEWAY_RPC_AUTHORIZATION` and
`ZKSYS_SERVICE_REGISTRY_RPC_AUTHORIZATION`, then run:

```sh
python3 scripts/prover-service/prepare_message_relay.py \
  --config /private/message-config.json --request /private/message-request.json \
  --output /private/message-preview.json
```

The preview checks the messenger log and source block, asks the Gateway RPC for
the proof, calls the existing zkSYS `0x10009` verifier, and simulates the
receiver against a pinned latest zkSYS block. It writes unsigned calldata only
if those checks succeed. This is a preview at that block, not a finality check;
state or canonicality can change before submission. This command does not
broadcast or establish that the service is ready. It has not been exercised
against a live Gateway or zkSYS, so the deployed RPC and real proof path still
need qualification.
