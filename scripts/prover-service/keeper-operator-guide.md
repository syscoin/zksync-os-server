# Service compute and settlement keepers

`keeper.py` reads canonical contract state, prepares unsigned maintenance calls,
and issues a bounded SNARK compute permit. The external SNARK path in
[`pool.py`](../prover-rental/pool.py) independently revalidates that permit before
renting. It checks the reviewed gate/coordinator/priority-guard code, production
verifier and V8 key, native prover role, parent, committed native batch hashes,
frozen package, selected wrapper and turn, priority prefix witness, and remaining
runtime. Both child and Gateway work use this path with their own lane settings.

These commands do not deploy, sign, broadcast, rent, or start a supervisor.
Unsigned calls require the existing operator wallet and transaction supervisor.
The configured `expected_operator` is an assertion by the trusted host controlling
its rental account; it is not remote-worker authentication. Enrollment and FRI
requests use account/operator signatures through the shared dispatcher. Final
package acceptance still requires the selected wrapper and sequencer signatures
plus the native proof verifier.

## Pin the trusted host

Keep one mode-0600 keeper configuration per lane and phase:

```json
{
  "schema_version": 1,
  "lane": "child",
  "rpc_url": "https://trusted-settlement-rpc.example/",
  "settings": { "...": "the complete service.py configuration for this lane" },
  "policy": {
    "expected_operator": "0xREVIEWED_OPERATOR",
    "gate_code_hash": "0xREVIEWED_DEPLOYED_GATE_CODE_HASH",
    "coordinator_code_hash": "0xREVIEWED_DEPLOYED_COORDINATOR_CODE_HASH",
    "priority_guard_code_hash": "0xREVIEWED_DEPLOYED_PRIORITY_GUARD_CODE_HASH",
    "reserve_seconds": 120,
    "max_head_age_seconds": 60,
    "rpc_timeout_seconds": 15
  }
}
```

The placeholders must be replaced with reviewed deployment values. Hash actual
deployed code, including immutable constructor values. The RPC must support
EIP-1898 canonical block-hash calls. Every preflight uses one canonical block,
checks its age, then rereads its numbered header and chain ID. Remote RPC uses
HTTPS; loopback HTTP is permitted. Redirects and environment proxies are disabled.
Keep RPC credentials outside this URL; the keeper currently expects a trusted
credential-free endpoint or a local authenticated proxy.

Before the coordinator exists, use the zero address and zero coordinator code
hash together. Bootstrap permits require `expected_operator == sequencer`.
Service permits require the selected roster candidate's exact operator. Changing
phase, reviewed code, or operator requires a new reviewed pool configuration;
the pool freezes these values at initialization. No live deployment values or
production VK are supplied by the examples.

For an external SNARK stage, pool configuration must contain:

```json
{
  "acquisition": "external",
  "release_file": "/trusted/snark-release.json",
  "rental_policy_file": "/trusted/snark-rental-policy.json",
  "service": { "keeper_config_file": "/trusted/child-keeper.json" }
}
```

Missing or null service configuration is rejected for external SNARK. Ordinary
native FRI/SNARK acquisition remains available for explicitly configured native
compute. External FRI carries `service: null`; its offer authentication belongs
to the shared FRI dispatcher and operator handoff. Never expose the native API's
shared Basic Auth to enrolled operators: it would let an unselected party acquire
a SNARK lease and obstruct the selected wrapper.

## Inspect the phase before offering rewarded FRI work

```sh
python3 scripts/prover-service/keeper.py --config /trusted/child-keeper.json \
  --output /trusted/phase.json status
```

Inspect `control_work`, `opening_roster`, and any `frozen_package` before
dispatching opportunities. Bootstrap can carry qualified duties before gate
activation. After activation, work before the first service period and recovery
work using an older ready roster have `control_work: true`. Such packages require
`duties: []` and the empty report, even when their native batches contain real
transactions. Compute their FRI proofs through the explicit native/control path;
do not consume shared service quota opportunities on work that cannot earn credit.
Control packages still require a legal native aggregate of at least two batches,
the selected wrapper, the priority guard, and a current permit.

A package already frozen for an older period retains that period and control
status across repairs and clock changes. A new current-period package cannot pay
old-period slots. Publish rosters and perform draws before the configured cutoff.
Measure FRI, wrapping, delivery, and finality times when setting the period lead,
grace, and turn durations; the keeper cannot make an already late duty creditable.
Sparse demand remains unoffered work, not a fabricated success or failure.

## Keep rosters, draws, and priority snapshots moving

Each invocation below writes a simulated unsigned transaction containing its
exact chain ID, sender, target, calldata, value, and canonical anchor. Submit it
with the appropriate external wallet, wait for authenticated delivery, and reread
state before proceeding. Simulation is a point-in-time check, not a reservation.

* `prepare-roster --arguments period.json` and `record-roster --arguments period.json`
  use `{"period":5}` and the configured coordinator. Preparation requires an
  already authenticated published roster. Recording requires the authenticated
  future draw to have arrived.
* For a child draw relayed from Gateway to NEVM, run `request-draw` on the NEVM
  RPC using the canonical Gateway message inclusion proof. Its argument object
  contains `target`, reviewed `code_hash`, `value:"0x0"`, `commitment`,
  `batch_number`, `index`, `tx_number_in_batch`, and `proof` (bytes32 array).
  The immutable root draw contract verifies the message and fixes a future block.
* `capture-draw` takes `target`, `code_hash`, `value:"0x0"`, and `commitment`.
  Capture after the source's confirmation delay and before blockhash expiry.
  The root-settled Gateway coordinator requests its native draw during preparation;
  it does not need the child message-proof request step.
* `relay-draw` adds explicit `gas_limit`, `gas_per_pubdata_byte_limit`, and
  `refund_recipient` to the target/code/value/commitment fields. `relay-checkpoint`
  uses the same fields without `commitment`. All uint256 quantities, including
  value and gas, are canonical hexadecimal strings. Supply the reviewed source
  and actual required relay payment; the keeper does not guess fees. Use a keeper
  configuration whose settlement chain is NEVM for these root transactions.
* `refresh-priority --arguments empty.json`, with `{}`, prepares the sequencer's
  gate refresh call. The contract permits this only after the old work window
  expires and preserves the work identity; it does not reroll the wrapper turn.

For example:

```sh
python3 scripts/prover-service/keeper.py --config /trusted/child-keeper.json \
  --output /trusted/prepare-roster-call.json prepare-roster --arguments /trusted/period.json
```

At least one independent root keeper must capture every available draw. If all
keepers withhold an unfavorable draw until blockhash expiry, recovery allows
selective-abort bias. Native PoW block hashes also retain producer bias. These
contracts do not authenticate ChainLocks or guarantee that a keeper will run.

Before nonempty priority work can receive a permit, publish its ordered prefix
witness. The witness file has `batch_item_preimages` (one array of exact native
priority-item encoding hex strings per batch), `left_path`, and `right_path`.
The keeper hashes those preimages, reconstructs every native batch's rolling
priority hash, and simulates the contract's ordered tree-range proof:

```sh
python3 scripts/prover-service/keeper.py --config /trusted/child-keeper.json \
  --output /trusted/prefix-call.json prefix-witness \
  --evidence /trusted/native-evidence.json --witness /trusted/prefix-preimages.json
```

The caller must obtain native item preimages and paths from its authenticated
chain data; the keeper does not invent or fetch them. Empty priority counts need
the native empty rolling hash and no published witness. A stale checkpoint,
missing overdue prefix, or missing nonempty witness refuses compute.

## Preserve the real SNARK lease from pick through settlement

1. The trusted sequencer host picks SNARK work with the existing Sentry. Stage is
   read from the reviewed SNARK release, not supplied as a separate CLI flag:

   ```sh
   python3 scripts/prover-rental/sentry.py --job-dir /trusted/original-snark-lease \
     --auth-file /trusted/native-basic-auth --execute pick \
     --endpoint http://127.0.0.1:3125/ --release /trusted/snark-release.json \
     --job-id child-wrap-unique-attempt
   ```

   Retain this directory and the original evidence snapshot privately. Record a
   conservative native lease deadline from the pick start time and the server's
   configured lease duration. The API response does not attest that duration.
   Obtain `/prover-jobs/v1/SNARK/{from}/{to}/evidence` for the exact returned range.
   The native `payload.json` contains the actual FRI proofs selected by the node.

2. Collect the exact accepted FRI duties, signed manifest, and subscriptions for
   that range. Construct `request.json` with the four keys `proposal`, `manifest`,
   `subscriptions`, and `duties`. `proposal` uses the same shape as `service.py
   package`: `mode`, `accepted_package` without generated manifest/report/proof
   hashes, `candidate`, and `candidate_proof`. A control request uses empty duties.
   The chosen candidate must be a valid member of the committed roster; opening
   clears its wrapper fields and does not select a winner from supplied data.

   ```sh
   python3 scripts/prover-service/keeper.py --config /trusted/child-keeper.json \
     --output /trusted/open-call.json open --request /trusted/request.json \
     --evidence /trusted/native-evidence.json \
     --payload /trusted/original-snark-lease/payload.json
   ```

   The sequencer wallet submits the unsigned call. `open` normalizes proof hash,
   wrapper, beneficiary, and turn to zero. It uses `openBootstrapPackage` before
   coordinator installation and `openPackage` in service mode. `repair` prepares
   the corresponding repair call for changed range/report artifacts; the contract
   preserves parent, ordinal, entropy, turn clock, and priority work. After any
   repair, build a new permit. Never treat a repair as additional service credit.

3. Read the selected candidate/index/current turn and update the proposal for that
   exact candidate. Deliver only the stripped payload, signed report ingredients,
   and evidence to the selected wrapper through authenticated transport. Compare
   producer evidence with the wrapper's independent execution node before signing
   or spending compute. The keeper authenticates native committed hashes but does
   not run an execution node or a FRI verifier.

   ```sh
   python3 scripts/prover-service/keeper.py --config /trusted/child-keeper.json \
     --output /trusted/compute-permit.json permit --runtime-seconds 600 \
     --request /trusted/request.json --evidence /trusted/native-evidence.json \
     --payload /trusted/original-snark-lease/payload.json
   python3 scripts/prover-rental/pool.py --state-dir /trusted/pool --execute \
     enqueue --lane child --stage SNARK --job-id child-wrap-unique-attempt \
     --payload /trusted/original-snark-lease/payload.json \
     --evidence /trusted/native-evidence.json --compute-permit /trusted/compute-permit.json \
     --deadline CONSERVATIVE_MINIMUM_OF_NATIVE_LEASE_AND_COMPUTE_DEADLINE
   ```

   The runtime must equal the frozen rental policy's maximum runtime. The pool
   then uses its ordinary `export`, `launch`, and `complete` commands and separate
   watchdogs. It rechecks the exact permit, payload, evidence, candidate, frozen
   artifacts, and current chain state before `Controller.launch`. It rereads wall
   time after chain RPCs and after the provider price lookup, including immediately
   before the sole allocation POST. Runtime plus reserve must fit both the turn
   and priority window and the supplied native lease deadline. The permit stays
   on the trusted host; the rental receives the ordinary one-job native input.

4. After external pool completion, bind the verified returned result to the
   original trusted lease, then use the existing native submit path:

   ```sh
   python3 scripts/prover-rental/sentry.py --job-dir /trusted/original-snark-lease \
     --execute import-result --input-dir /trusted/pool/child/jobs/POOL_OPERATION_ID \
     --controller-state /trusted/pool/child/SNARK/controller \
     --operation-id RENTAL_OPERATION_ID --evidence /trusted/native-evidence.json --lane child
   python3 scripts/prover-rental/sentry.py --job-dir /trusted/original-snark-lease \
     --auth-file /trusted/native-basic-auth --execute submit \
     --controller-state /trusted/pool/child/SNARK/controller --operation-id RENTAL_OPERATION_ID
   ```

   Keep the same job ID and exact release bytes across pick and pool import.
   `import-result` verifies the external controller receipt, exact retained payload,
   release, range, evidence, and origin binding before attaching result metadata
   to the protected authority. The original lease is reattached only by `submit`.
   Neither Basic Auth nor `authority.json`, `picked-wire.json`, or `submission.json`
   may reach the selected wrapper or rental.

5. With node service publication mode explicitly configured, native SNARK intake
   writes its immutable `work.json` for this exact proof/range. Use `service.py
   package` and `complete` to obtain both package endorsements (sequencer only for
   bootstrap). The selected wrapper independently checks the native statement,
   report, proof, candidate, and current turn before signing. Use the durable
   [relay](relay-README.md) to publish the native gate transaction. Once confirmed:

   ```sh
   python3 scripts/prover-service/relay.py --config /trusted/service-config.json \
     --policy /trusted/relay-policy.json --state-dir /trusted/relay-state --execute \
     export-handoff --operation-id PACKAGE_HASH_WITHOUT_0X --work-dir /trusted/node-work-directory
   ```

   This writes `relay.json`. The node independently checks the canonical successful
   gate receipt, exact transaction input, package/range/proof, and confirmations
   before advancing normal execution. A local hint or
   rental receipt cannot authorize progress or service rewards. Read the node's
   service-publication configuration and relay recovery guide before enabling this
   mode; ordinary native proving remains the default.

All keeper and permit files are version 1. They are coordination artifacts, not
on-chain reservations. Reorgs, another accepted package, repairs, and turn changes
can invalidate a checked permit. Provider boot time, network delays, and signing
time must fit the configured reserve; a check immediately before POST cannot
guarantee the eventual provider allocation or chain inclusion time. A stale
attempt must be reconciled and stopped through its existing durable controller,
not relaunched under a new ID to evade its budget or timeout.
