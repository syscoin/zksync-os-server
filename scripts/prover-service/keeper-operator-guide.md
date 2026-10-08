# Service compute and settlement keepers

`keeper.py` reads canonical contract state, prepares unsigned maintenance calls,
and issues a bounded SNARK compute permit. The external SNARK path in
[`pool.py`](../prover-rental/pool.py) independently revalidates that permit before
renting. It checks the reviewed gate/coordinator/priority-guard code, production
verifier and V8 key, native prover role, parent, committed native batch hashes,
frozen package, selected wrapper and turn, priority prefix witness, and remaining
runtime. Both child and Gateway work use this path with their own lane settings.

The low-level `keeper.py` commands do not deploy, sign, broadcast, rent, or start
a supervisor. The automated coordinator below passes its validated calls to the
durable transaction supervisor and existing signing-only wallet under `--execute`.
The configured `expected_operator` is an assertion by the trusted host controlling
its rental account; it is not remote-worker authentication. Enrollment and FRI
requests use account/operator signatures through the shared dispatcher. Final
package acceptance still requires the selected wrapper and sequencer signatures
plus the native proof verifier.

Every subscriber uses `services: 3`, so FRI workers and selected SNARK wrappers
come from the same enrolled pool. The wrapper watcher runs on that operator's
trusted controller beside its existing warm compute supervisor. It does not
require an EN: it authenticates canonical native commitments, verifies the exact
FRIs on CPU, checks the returned native SNARK, and audits signed dispatch history
before endorsing. Selection and wrapping add no duty points, fee entitlement, or bonus weight.

## Pin the trusted host

Keep one mode-0600 keeper configuration per lane, phase, and enrollment journal:

```json
{
  "schema_version": 1,
  "lane": "child",
  "rpc_url": "https://trusted-settlement-rpc.example/",
  "settings": { "...": "the complete service.py configuration for this lane" },
  "enrollment": {
    "registry_rpc_file": "/trusted/child-registry-rpc.json",
    "block_hash": "0xREVIEWED_CANONICAL_ENROLLMENT_BLOCK_HASH"
  },
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

Every keeper configuration must contain `enrollment` with exactly
`registry_rpc_file` and `block_hash`. The first
is an absolute private connection-file path with `url` and `authorization`; the
second is the independently reviewed canonical enrollment block for that journal.
Use the same pin as its dispatch audit trust. This trusted configuration enables
canonical historical account consent for the full roster in keeper, coordinator,
wrapper and pool permit revalidation, including mixed EOA and ERC-1271 membership
accounts. Fresh operator, sequencer, and wrapper signatures remain required.
The coordinator authenticates that pin and the dispatcher's lane before a new
native SNARK pick. It may reuse the verified immutable snapshot while running;
it never obtains authority from a serialized journal or work-envelope flag.
Existing uncertain native leases retain their reconciliation path.
Offline `service.py` helpers and `keeper.prepare` retain their EOA account-signature
checks when canonical context is omitted. Contract operators still require a
separately implemented adapter.

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

## Supervise the automatic service workflow

`coordinator.py` and `wrapper.py` automate service-mode handoffs after the reviewed
coordinator has been installed and service activation has completed. Bootstrap
still uses the explicit low-level keeper/Sentry path below. Run one coordinator
per execution lane on the trusted sequencer host. Both lanes read the same current
dispatcher journal, whose enrollment and quota are shared across chains. Each
selected operator runs a wrapper watcher per configured lane and uses its existing
external-input pool and warm supervisor.

The coordinator retains the private native SNARK pick, evidence and conservative
lease deadline. It signs a frozen dispatcher manifest and complete audit snapshot,
opens the package through `transactions.py`, and delivers a signed work envelope
for the exact selected operator and turn. The wrapper authenticates the complete
roster, audit, canonical batch commitments, and native FRI proofs before renting
or reusing compute. On completion it calls the production native SNARK verifier
at a canonical block and signs the package only if the exact work and its turn
remain current. A valid proof returned after a turn change can be retained without
reusing the stale endorsement.

The sequencer independently verifies the signed result and native SNARK, submits
its original lease, and waits for the node's matching publication `work.json`.
It then adds the sequencer endorsement, advances the durable relay journal, and
exports the confirmed `relay.json` to that publication directory. The node still
performs its own receipt, calldata, code-pin and confirmation checks. An ambiguous
native pick, wallet signature, send or nonce does not authorize a replacement;
the journals retain the original work for reconciliation. A known expired lease
is renewed with paired `snark_batch_from` / `snark_batch_to` bounds. The native
picker must return the exact frozen range and payload; it cannot silently enlarge
the package as more FRIs arrive. Unavailable ranges remain unleased and are retried.
A protocol-boundary constraint can require an explicit package repair through the
existing keeper workflow; renewal never invents a different report.

Priority refresh and prefix-witness transactions are scoped to the guard, work
identity and frozen checkpoint. The coordinator persists each invocation before
staging its transaction. A new checkpoint gets a new invocation even when the
calldata is identical; retries of an unresolved invocation retain its nonce and
signed transaction. A canonically confirmed revert permits a new retry generation.
Keep the transaction journal and coordinator state together across restarts.
Legacy refresh or witness records that already reserved a nonce without a
checkpoint scope can recover their original receipt, but are not rebroadcast
under a new checkpoint.
The maintenance journal retains at most 256 operations; reaching that limit
blocks new entries. This limit includes completed refreshes and witnesses.

### Configuration and external inputs

Configuration files and handoffs are owner-only files (mode 0600) in absolute,
owner-only directories (mode 0700), without symlinks. Configure real reviewed
deployment values; the examples' zero VK/code/address placeholders remain invalid.
The automatic coordinator configuration has these exact top-level fields:

| Fields | Required input |
| --- | --- |
| `schema_version`, `keeper` | Version `1` and the complete lane keeper object. Its `expected_operator` is the sequencer. |
| `endpoint`, `native_auth_file`, `release_file`, `native_lease_seconds` | Native prover API, private Basic Auth file, reviewed SNARK release, and a conservative lease duration matching the node. The API does not attest its lease duration. |
| `dispatcher_dir`, `roster_dir` | The current shared dispatcher journal and complete authenticated roster files named `<period>.json`. |
| `operator_inboxes` | Map of registered lowercase operator addresses to private handoff directories. |
| `priority_witness_dir`, `publication_dir` | Authenticated priority witnesses named `<work_id_without_0x>.json`, and this lane's node service-publication directory. |
| `sequencer_wallet_file`, `relay_wallet_file` | Private connection files for the sequencer signing/maintenance wallet and a separate proof-relay wallet. |
| `transaction_policy`, `relay_policy` | Complete [relay-policy](relay-policy.example.json) objects with matching reviewed code pins and explicit gas/fee/confirmation limits. Maintenance uses the sequencer account; proof relay must use a different nonce account. |
| `sequencer_beneficiary`, `poll_interval_seconds` | The fixed nonzero sequencer payee and a polling interval from 1 to 60 seconds. |

The wrapper configuration has exactly `schema_version: 1`, `keeper`, `pool_dir`,
`inbox_dir`, `roster_dir`, `audit_trust_dir`, `wallet_file`, `registry_rpc_file`,
`fri_verifier`, and `poll_interval_seconds`. Its keeper uses the registered local
operator as `expected_operator`, and must exactly match the pool lane's external
SNARK service configuration. `fri_verifier` contains the absolute `executable`,
its lowercase `sha256`, and `timeout_seconds` from 1 to 1800. Use a reviewed CPU
build of `zksync_os_snark_prover` containing `verify-fri`; no GPU or SNARK CRS is
needed for this local verification. The reviewed release binds its nonzero VK
and program commitment. The GPU worker retains its own release/VK startup gates.

Wallet and registry RPC connection files contain exactly
`{"url":"https://trusted-endpoint.example/","authorization":null}`; use the real
endpoint and authorization value. Typed signing uses `eth_signTypedData_v4`.
Transaction signing uses signing-only `eth_signTransaction`; the journals persist
the exact raw transaction before sending it to the chain RPC. These files and
wallet capabilities never enter rental inputs.

Build each complete roster artifact from the sorted registry candidates:

```sh
python3 scripts/prover-service/roster.py build --period 5 \
  --candidates /trusted/candidates.json --output /trusted/rosters/5.json
```

The output includes its root, count and all Merkle proofs. The watcher authenticates that complete artifact against the
canonical on-chain roster. Populate `audit_trust_dir/<journal_id_without_0x>.json`
from the wrapper's own enrollment-block pin and retained checkpoint/readiness/duty
observations as described in the [audit guide](dispatch-audit.md). Do not derive
those independent trust pins from the current untrusted work envelope.

The coordinator writes `work.json` and `payload.json` inside a work-hash directory
under the selected operator's inbox; the wrapper returns a signed `result.json`
there. On separate machines, provide authenticated bidirectional synchronization
that preserves exact bytes, private permissions and publication order. No public
transport service is started by these scripts. Never synchronize the coordinator's
native authority, lease, credentials, transaction journal or signing files.

Roster publication, both future-draw registrations and cross-chain deliveries,
and priority checkpoint relayers remain external prerequisites. Supply actual
native priority preimages and tree paths when the coordinator reports
`await_authenticated_priority_witness`; it will validate and publish that witness,
but cannot construct it from a transaction count. Rotate the dispatcher snapshot
only under its period/phase rules and preserve outstanding journals; an old
snapshot is not silently rewritten for a new period.

### Start and inspect

Initialize with complete private configurations, then inspect a single step:

```sh
export ZKSYNC_AIRBENDER_PROVER_DIR=/trusted/zksync-airbender-prover
python3 scripts/prover-service/coordinator.py --state-dir /trusted/child-coordinator \
  --execute init --config /trusted/child-coordinator-config.json
python3 scripts/prover-service/wrapper.py --state-dir /trusted/child-wrapper \
  --execute init --config /trusted/child-wrapper-config.json
python3 scripts/prover-service/coordinator.py --state-dir /trusted/child-coordinator run --once
python3 scripts/prover-service/wrapper.py --state-dir /trusted/child-wrapper run --once
```

`run` without `--execute` inspects the next action without picking, signing,
renting or broadcasting. Run both loops under the existing process supervisor
when authorized:

```sh
python3 scripts/prover-service/coordinator.py --state-dir /trusted/child-coordinator --execute run
python3 scripts/prover-service/wrapper.py --state-dir /trusted/child-wrapper --execute run
```

Use `status` or `run --once` to inspect progress. Separately keep the prover-owned
warm supervisor and watchdogs running against this same external pool, following
the [pool guide](../prover-rental/pool-README.md). It prioritizes eligible wrapping
before starting another FRI assignment and processes one job at a time; an active
FRI is not proof of an idle GPU. Missing handoffs, stale turns, insufficient lease
time, unavailable witnesses and uncertain transactions produce wait/recovery
states instead of synthetic work or service credit. No production end-to-end
deployment or live GPU run is established by the local regression tests.

`dispatcher_dir` may name a single initialized period journal, or a private
parent containing one initialized journal per period (`5/`, `6/`, and so on).
The coordinator selects the frozen/opening roster period before acquiring new
work and taking its signed snapshot. Its keeper pin must match that journal.
Retain older journals, coordinator/wrapper state, and pool configurations until
their pending work and uncertain allocations are reconciled. For a new period,
create its journal through the enrollment workflow, prepare new reviewed keeper,
coordinator/wrapper and pool configurations in fresh state directories, and supply
the matching independently reviewed audit trust pin to the operators. The
immutable configuration does not adopt a new journal's pin automatically. Do not
run competing dispatchers or rewind old state to change a pin.

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

## Native proof checks for a manual handoff

The automatic wrapper creates its CPU verifier inputs after authenticating
canonical commitments. For manual operation, prepare `fri-expected.json` with
exactly `schema_version: 1`, `protocol_version: 32`, `proving_version: 8`,
`security_level: 100`, `from_batch_number`, `to_batch_number`, `vk_hash`,
`program_commitment`, `statements`, and `payload_sha256`. Take the program/VK
binding from the reviewed release, derive the ordered statements from the
canonically authenticated batch/output preimages, and hash the exact payload
file bytes for `payload_sha256` (lowercase hex without `0x`). Hashes and statements
otherwise use canonical lowercase `0x` hex. This trusted expected file is not a
worker-supplied claim; the standalone CPU command does not authenticate an RPC or
register a new program/VK association.

```sh
/trusted/bin/zksync_os_snark_prover verify-fri \
  --payload /trusted/original-snark-lease/payload.json \
  --expected /trusted/fri-expected.json --output /trusted/fri-verified.json
python3 scripts/prover-service/proof_check.py --config /trusted/child-keeper.json \
  --evidence /trusted/native-evidence.json \
  --payload /trusted/original-snark-lease/payload.json \
  --proof /trusted/returned-snark.json --output /trusted/snark-verified.json
```

The first command verifies every proof and its program/statement binding before
creating a new private result. The second authenticates the deployed native V32
verifier, current canonical commitments and proof range, then checks the SNARK
through a pinned-block `eth_call`. Neither command signs or submits a transaction.
The wrapper also needs the complete signed dispatch audit and current selection
checks; these verification records alone are not a service receipt or permission
to sign a later turn.

## Manual inspection and recovery of the native SNARK lease

The commands below expose the same underlying primitives for controlled manual
operation. Do not run a second picker or transaction supervisor over a lease or
nonce already owned by the automatic coordinator.

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
   audit bundle, and evidence to the selected wrapper through authenticated
   transport. Authenticate the audit against independently retained trust inputs,
   check the native committed hashes, and run the pinned CPU `verify-fri` command
   against the exact ordered payload and reviewed program commitment before
   spending SNARK compute. The automated wrapper performs these checks; `keeper.py`
   alone authenticates commitments and permits but does not execute a FRI verifier.

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
   signed dispatch audit, exact FRIs, report, proof, candidate, and current turn
   before signing. `proof_check.py` verifies the native SNARK against canonical
   production-verifier state; a rental result hash is insufficient. Use the durable
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
