# Proof gate deployment stage

`deploy_gate.py` is a post-deployment stage for **one** settlement lane. Run it
separately for the zkSYS gate on Gateway and the Gateway gate on NEVM, with a
separate protected journal and signer for each lane. Each factory deploys one
proof gate from the compiled constructor bytecode. Use the component installer
for the NEVM message sink, roster receivers, root draws and coordinators.

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
  "message_sink": "0x<NEVM service sink, or zero address for Gateway gate>",
  "message_sink_code_hash": "0x<32-byte runtime hash, or zero for Gateway gate>",
  "sequencer": "0x<service sequencer>",
  "policy_hash": "0x<32-byte service policy hash>",
  "production_vk_hash": "0x<32-byte production VK hash for this lane>",
  "child_chain_id": 57001,
  "factory_salt": "0x<unique 32-byte deployment salt>",
  "create2_code_hash": "0x<verified runtime hash of canonical CREATE2 factory>",
  "gas_limit": 30000000,
  "gas_price": 1000000000,
  "keystore": "/absolute/private/path/keystore.json",
  "password_file": "/absolute/private/path/password"
}
```

Use the actual chain IDs and gas settings for each deployment. On Gateway,
`chain` is the zkSYS diamond, `child_chain_id` is zkSYS, and the sink address and
hash are zero. On NEVM, `chain` is the Gateway diamond, `child_chain_id` is
Gateway, and `message_sink` is the NEVM service sink. Pin each lane's native
contracts, production verifier and VK independently.

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
only those same bytes if they disappear from the mempool. A failed or inconsistent
transaction stops the stage. Retain its journal for diagnosis.

After actual bootstrap proof acceptance and coordinator deployment, pass
`--coordinator 0x... --coordinator-code-hash 0x... --send` with the same config
and journal to install it. The gate checks the coordinator's accepted parent,
chain, draw topology, policy, VK and sequencer. The script checks the
supplied coordinator runtime hash before submission. Resume an in-flight
installation with the same coordinator and hash.

For each proof lane, verify the gate is the sole `PROVER_ROLE` member of its
configured validator timelock. Verify the independent authority for ordinary
native proof publication separately. The gate anchors
itself to the diamond's verified frontier at deployment. Compare its
`observedNativeBatch()` against the expected native verified height and check
the gate's policy, production VK and sink binding. Exercise both publication
routes and receipt delivery before activating service rewards. Confirm that
bootstrap receipts from both lanes reached the registry before installing the
coordinators. Use the existing authorized L1-to-Gateway administrator route
where the Gateway role administrator is the aliased L1 ChainAdmin.

## Service message sink, roster and coordinator deployment

`deploy_component.py` deploys one service component per configuration and journal.
Each component uses a deterministic factory whose first CREATE address is
independent of the component's constructor data. The root sink can therefore name
the planned NEVM gate and the predicted zkSYS accepted-message receiver.

Each private JSON file has these common fields, with real deployment values:

```json
{
  "kind": "root_message_sink",
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
  diamond's next batch must match at coordinator deployment. The coordinator
  subsequently reads the next batch from the native verified frontier. Pause
  bootstrap service submissions during installation to preserve the pinned
  accepted-package parent.

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
address. Use the service receipt workers for message delivery.


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
    {"id": "root-sink", "type": "component", "config": {"...": "..."}},
    {"id": "root-gate", "type": "gate", "config": {"...": "..."}},
    {"id": "gateway-gate", "type": "gate", "config": {"...": "..."}}
  ]
}
```

In a config, a peer address can be written as, for example,
`{"ref":"root-sink.component"}` or `{"ref":"gateway-gate.gate"}`.
Component stages expose `factory` and `component`. Gate stages expose `factory`
and `gate`. Use a distinct sender/salt factory for each stage on a chain.
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

## Service receipt delivery

The zkSYS claim worker and Gateway root publisher use the authenticated
Gateway batch root route. Gateway-native messages also have a separate native
relay for activation and reports that are not bundled with a root. Follow
[service-receipt-workers.md](service-receipt-workers.md) for configuration and
startup. The existing shared-root verifier does not authenticate a
Gateway-origin direct-L1 message and is not a fallback for this route.
