# Trusted service signing packages

`service.py` prepares explicit, reviewable EIP-712 wallet requests and a strict
`ProverServiceSidecarV1` for the opt-in V1 proof gate. It never accepts signing
keys, broadcasts transactions, rents compute, or enables the service mode.
Run it on the trusted Sentry or sequencer host with Python 3 and Foundry `cast`.
The rental image does not contain this tool, signing requests, subscriptions,
real prover leases, or wallet credentials.

The module is a staged integration using the reproduced V32/V8 guest and key.
Service hardware qualification, real enrolled-roster dispatcher exercises, independent EN export
and deployment closure of alternative proof paths remain launch gates. The
authenticated trusted-host dispatcher is implemented below; it does not replace
these production checks.
The zero values in `config.example.json` deliberately fail validation.

Use the [keeper operator guide](keeper-operator-guide.md) for package opening,
selected-wrapper compute permits, both rental queues, and the private SNARK lease
handoff. The [node publication guide](node-publication.md) covers the opt-in
sender configuration and receipt recovery; the [relay guide](relay-README.md)
covers wallet signing and durable broadcast.

## Trust and validation

The checker reconstructs the native V32 chain configuration hash, packed
`BatchOutput` commitment, complete per-batch statement and transaction count
from an **independently synchronized EN's** stored-batch and output preimages.
Its evidence file must be exported from that trusted EN; copying the sequencer's
claims into this file does not establish independent validation. An authentic
exporter and its connection to independently replayed state are required before
launch. No `verified: true` property is accepted as evidence.

For a FRI duty, it requires the protected node's durable `accepted` authority
from `../prover-rental/sentry.py`, rechecks the exact accepted submission hash,
and binds the current manifest assignment to `keccak256(realLeaseTokenBytes)`.
The lease token is read only from the private local authority file and is never
included in a signing request. The node's existing native FRI verifier is the
cryptographic admission authority. Local files must be protected from an
untrusted host; an attacker who can rewrite trusted authority files defeats this
trust boundary.

For a package, it checks account-signed subscriptions, a sequencer-signed
manifest, operator-signed current duties, exact FRI artifact hashes, EN statement
and output preimages, duplicate batch/slot credit, complete retry chains, the
wrapper's Merkle membership and fixed beneficiaries. The native SNARK proof is
encoded as `0x01 || abi.encode(previous, batches, [0x802, 0, 44 proof words])`.
The proof gate and production verifier make the final native validity decision;
this Python checker does not implement the FRI or SNARK cryptographic verifier.
The wrapper must independently verify the native FRIs before signing.

Current signatures are verified with `cast wallet verify --no-hash` and require
canonical 65-byte EOA signatures. Contracts support ERC-1271, but this offline
tool deliberately rejects smart-wallet signatures until a trusted, anchored
chain-call verifier is integrated. Wallet signing happens outside this program;
use the generated `typed_data` object or exact `digest`, never add the Ethereum
personal-message prefix to that digest.

Signatures authenticate these records. They do not prove physical computation,
private offer or delivery times, fairness, completeness of omitted work, or
independent ownership of operator keys. The snapshot only checks signed
subscription contents; live registration, seniority, eligibility, selected
wrapper index/turn, frozen parent and roster must be checked against the settled
contracts by the operator before endorsement and are enforced where applicable
by the contracts. A producer-authenticated cursor/retry exporter, fair dispatch
policy, priority deadlines/source freezing and live pipeline enrollment are
separate launch work.

## Domain and JSON conventions

`config.example.json` must be populated separately for zkSYS and Gateway from the
reviewed deployment. `registry_chain_id` identifies their shared child registry;
`execution_chain_id` identifies the chain whose batches are being proved, and
`settlement_chain_id` identifies the chain accepting that proof. The
registry lives on the child chain, so subscription and duty requests use
`ZkSysProverService`, version `1`, the **child** chain ID and registry address.
Work-manifest authentication uses `ZkSysServiceManifest`, version `1`, with the
same child-chain domain, and a single `WorkManifestV1(bytes32 manifestHash)`
message. This transport commitment is separate from contract-defined types.

Service packages use `ZkSysWrapperCoordinator`, version `1`, the **settlement**
chain ID and coordinator address. Bootstrap packages use `ZkSysProofGate` and
the proof-gate address on that settlement chain. `AcceptedPackageV1.chainId`
is the execution chain ID: zkSYS for its Gateway proof gate, or Gateway for its
NEVM proof gate. Native statements use that same execution ID. A Gateway job
must never change the registry signing domain to Gateway or NEVM.

Solidity struct fields use exact camelCase names; sidecar top-level fields use
snake_case. Addresses, bytes and hashes use lowercase `0x` hex. `uint256` values
use canonical hex strings (`"0x0"`, `"0x23a"`); smaller uints use JSON numbers.
JSON inputs reject duplicate and unexpected fields. All output files are created
exclusively with mode 0600 and fsynced; use a new path for a changed request.

The full synthetic `vector.json` shows every contract struct and a Rust-loadable
signed sidecar. Its proof bytes and trivial test keys are solely ABI fixtures;
they are not production proofs, subscriptions or identities.

## Subscription

Prepare a subscription from the current on-chain nonce and an upcoming period:

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/subscription-request.json subscription \
  --subscription /trusted/subscription.json
```

Have the account wallet sign `typed_data`. Store the subscription and returned
signature as `{"subscription": {…}, "signature": "0x…"}`. A subscription snapshot
is a JSON array of these records, with unique account/operator identities; its
commitment is Keccak-256 of canonical JSON (sorted keys, compact separators,
UTF-8, no newline). The same canonicalization is used for all JSON commitments.
Registration still requires the registry's `subscribe` transaction.

## Wrapper renewal during idle periods

Completing the quota records historical capability for that account/sequencer.
Thereafter any keeper can call `renewWrapper(subscriptionHash, period)` using
an active signed wrapper subscription. The registry rechecks membership and
seniority and installs a future wrapper candidate. Renewal itself does not
admit reward bonus weight, mint credit, or fabricate new completed duties; idle
periods can retain proof availability without paying for nonexistent work.

Use `registry-snapshot.example.json` to export the registry's clock and source
identity at one canonical child-chain block through a **trusted RPC**. Include
`startTime`, `periodSeconds`, `firstServicePeriod` and
`rosterPublicationLeadSeconds` as their snake_case fields. These must match the
reviewed deployment and the clock authenticated by the Gateway roster receiver.
Include the source block number, hash and timestamp. The offline helper does
not authenticate a caller-created snapshot or claim that an account is already
verified. The keeper must check `verifiedForSequencer`, registration, membership,
freshness and the canonical block before using it.

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/renewal-transaction.json renew-wrapper \
  --signed-subscription /trusted/signed-subscription.json \
  --registry-snapshot /trusted/registry-snapshot.json
```

The helper reproduces `nextAdmissionPeriod()`, including the first-service-period
floor and exact roster-cutoff boundary. It verifies the account signature and
that the subscription includes wrapper service for the selected period, then
emits an **unsigned** zero-value transaction to the child registry plus the
source block and submission cutoff. Before sending through the trusted wallet
or keeper, re-read `nextAdmissionPeriod()` and `rosterCutoff(period)` at a current
canonical block. A stale period reverts instead of renewing a different one.
The helper sends no RPC requests, transactions, roster messages or system
messages. Roster publication/relay and root draw registration remain separate
authenticated operations after renewal.

## Authenticated work manifest

The portable manifest is an envelope
`{"payload": {…}, "sequencer_signature": "0x…"}`. Its payload contains exactly:

```json
{
  "schema_version": 1,
  "chain_id": "0x23a",
  "chain_address": "0x1111111111111111111111111111111111111111",
  "sequencer": "0x2222222222222222222222222222222222222222",
  "period": 5,
  "previous_cursor": "0x…32 bytes…",
  "subscription_snapshot_hash": "0x…32 bytes…",
  "assignments": [],
  "retries": []
}
```

Each assignment contains `assignment_id`, `account`, `subscription_hash`,
`batch_number`, `statement_hash`, `lease_commitment`, `attempt`, `slot`, `period`.
Assignments start at attempt 1. A retry contains `assignment_id`,
`next_assignment_id`, and `reason` (`expired`, `invalid`, or `unavailable`). It
must link the same batch to the next attempt with a different lease commitment.
No fork, omitted intermediate attempt, duplicate ID or multiple current
assignments for a batch is allowed. Retired assignments cannot earn a duty.
All statements must match the current EN snapshot; a changed execution history
requires new dependent evidence and authorization.

The trusted-host `dispatcher.py` journal supplies the previous cursor, enrolled
snapshot and complete assignments/retries. `service.py` checks those inputs and
does not infer missing work. Prepare the sequencer's manifest request with:

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/manifest-request.json manifest-request \
  --payload /trusted/manifest-payload.json
```

After external signing, put the signature in the envelope. Package
`manifestHash` commits the complete canonical envelope, including that signature.
The previous cursor must be authenticated and checked against prior accepted
history externally; a nonzero value alone is not proof of scheduling continuity.

## EN evidence and duty requests

The EN evidence file contains exactly `schema_version`, `chain_id`,
`chain_address`, `settlement_chain_id`, `protocol_version`, `vk_hash`,
`previous_batch`, and `batches`. `previous_batch` is the gate's `StoredBatch`;
each element of `batches` is `{"stored": StoredBatch, "output": BatchOutput}`.
The complete stored/output fields and expected hashes appear in `vector.json`.
The range is consecutive and contains 1–100 batches. Version is exactly 32,
with a nonzero reviewed production VK.

After `sentry.py submit` receives a native accepted disposition, prepare a duty:

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/duty-request.json duty \
  --evidence /trusted/independent-en.json --manifest /trusted/manifest.json \
  --subscriptions /trusted/subscriptions.json \
  --authority /trusted/duty-attempt/authority.json \
  --proof /trusted/collected-fri-artifact.json
```

The proof artifact is the stripped native submit payload
`{batch_number,vk_hash,proof}` with base64 native proof bytes. Sign the resulting
request using the registered operator wallet and append the returned
`operatorSignature` to the request's `typed_data.message`. A report is a JSON
array of these completed `DutySuccessV1` objects. Empty transaction batches do
not qualify for duties. A lost native acceptance response needs canonical node
reconciliation; this tool will not promote a pending authority to accepted.

## Package requests and Rust sidecar

The proposal file contains `mode` (`service` or `bootstrap`),
`accepted_package`, `candidate`, and `candidate_proof`. Supply every
`AcceptedPackageV1` field except `manifestHash`, `reportHash`, and `proofHash`;
the tool computes those three. Service proposals include a `WrapperCandidateV1`
and Merkle branch. Bootstrap requires candidate `null`, empty branch, zero
wrapper/beneficiary/roster and turn 0. A live service proposal must use the
current on-chain selected candidate, turn, frozen range and accepted parent.

The SNARK proof input is the stripped native submit payload
`{from_batch_number,to_batch_number,vk_hash,proof}`. The FRI input is the stripped
SNARK pick payload `{from_batch_number,to_batch_number,vk_hash,fri_proofs}`;
both carry native bytes in base64, and neither includes a real lease.

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/package-request.json package \
  --evidence /trusted/independent-en.json --manifest /trusted/manifest.json \
  --subscriptions /trusted/subscriptions.json --duties /trusted/duties.json \
  --proposal /trusted/proposal.json --proof /trusted/snark-artifact.json \
  --fri-payload /trusted/stripped-snark-pick.json
```

Independently review and sign `sequencer_request` and `wrapper_request`; both
commit the same digest. Save each returned signature as plain `0x` text. For
bootstrap, only `sequencer_request` exists. Assemble the Rust-loadable sidecar:

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/service-sidecar.json complete \
  --prepared /trusted/package-request.json \
  --sequencer-signature /trusted/sequencer.sig \
  --wrapper-signature /trusted/wrapper.sig
```

Omit the wrapper signature argument for bootstrap. `complete` rehashes the
package and report, verifies signatures, and outputs exactly the Rust sidecar
fields. `proof_data` stays in the review artifact; the server recomputes it from
its own canonical batches and proof. A different proof, turn, report, parent or
range requires fresh signatures. Use the opt-in server integration only after
all deployment and independent-source gates are satisfied.

## Durable relay

For dispatcher-assigned work, use the [lease-less rental handoff](rental-handoff.md):
the Sentry exports only compute input, checks the returned artifact and prepares
an `offered-duty` request while the real lease stays on the dispatcher host.

[`relay.py`](relay.py) stages the completed sidecar and exact native payload,
checks the deployed gate/coordinator state, and uses a trusted wallet RPC only
under `--execute`. Its journal persists signed raw transactions before broadcast
and reconciles ambiguous sends without replacing authorization. See
[`relay-README.md`](relay-README.md) for code/fee pinning, the read-only schedule
view, bootstrap and priority-guard prerequisites, nonce ownership and recovery.

## Verification

```sh
python3 -m unittest discover -s scripts/prover-service -p 'test_*.py' -v
```

Tests compare ABI encoding and EIP-712 digests to `cast`, exercise full signed
sidecar preparation, and reject missing authentication, changed native accepted
bytes, wrong leases, altered EN outputs/statements, changed FRI artifacts,
missing retry history, wrong signers and domain mutation. The shared vectors
also feed Rust/Solidity regression tests. These are protocol tests with synthetic
proofs; they do not constitute production proving or live-chain validation.

## Live trusted-host FRI dispatcher

`dispatcher.py` connects the signed service workflow to the existing native FRI
pick/submit API. It is opt-in (`--execute` is required), runs on the trusted
sequencer host, and never receives wallet private keys. Keep one protected journal
per sequencer, period, and bootstrap/service phase. Never run competing journals
or rewind a journal from a backup: the durable cursor and per-account slots are the
allocation authority. The adapter supports up to 256 enrolled accounts and 2,000
retained assignment attempts per journal; its commands hold an exclusive process
lock. Initialization requires the full eligible account count times its shared
quota to fit that limit, counting the child/Gateway quota once. Deployment sizing
must also leave retry headroom and stay within the enrollment bound. Fair
allocation assumes the trusted sequencer preserves authenticated
readiness requests and delivers its signed offers; the journal does not prove
that a malicious sequencer has withheld neither. This is a trusted dispatcher,
not a decentralized availability protocol.

Initialization verifies account signatures and every subscription, operator
binding, fresh senior membership, policy, chain address, quota, and period clock
against the same finalized child-chain RPC block using EIP-1898 `blockHash` with
`requireCanonical`. The RPC must support these methods; there is no latest-block
fallback. This adapter currently accepts canonical EOA signatures. The contracts
also support ERC-1271 accounts, which require a separate contract-wallet adapter.
The supplied account list must exactly match the on-chain FRI subscriber
enumeration at that block after explicit fresh-membership eligibility checks.
Removed or stale entries need no supplied subscription, and wrapper-only
subscriptions are excluded from FRI enumeration. The 256-account bound applies
to enumerated registrations, including ineligible ones.

The account snapshot is fixed for that journal, so finish bootstrap enrollment
before initializing it. A later bootstrap applicant waits for the next assessment
snapshot with no failure mark. Never reset or create a competing same-period
journal to add applicants: that would reuse duty slots and readiness nonces.

```sh
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  init --config service-config.json --subscriptions subscriptions.json --period 5 \
  --rpc https://trusted-child-rpc.example/ --endpoint http://127.0.0.1:3125/ \
  --gateway-config gateway-service-config.json --gateway-endpoint http://127.0.0.1:3124/
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  work-request --account 0xACCOUNT --expires-at UNIX_SECONDS --output work-request.json
```

The enrolled operator signs the exact `typed_data` in `work-request.json` with its
external wallet and returns `{"signature":"0x..."}`. The request binds the journal,
account, subscription, period, nonce, and deadline. Its journal commitment includes
both complete lane configurations; `work_scopes` exposes their identities for
operator review. Registration or readiness earns
no credit. Authenticated ready accounts receive nonempty native batches in sorted
account round-robin order, one outstanding assignment per account. No job,
unregistered identities, expired readiness, and empty work consume no service
opportunity. A signed offer consumes one opportunity; `report` distinguishes the
number offered from native successes and says whether the whole quota was offered.
Sparse demand is not an asserted service failure and never creates synthetic
successes or reward weight.

Both queues use one protected journal and one account/period quota. The dispatcher
rotates between them, including after an empty pick, while keeping batch/retry
identities, native endpoints and credentials separate. Equal batch numbers on the
two chains are distinct work. A quota slot consumed in one lane cannot be allocated
again in the other. Both lane bindings are checked against `supportedLane` at the
same finalized child-registry snapshot. The single-sequencer launch uses the same
account/operator subscriptions and sequencer identity on both lanes. Registry
deployment requires a nonzero `sharedSequencer` even in child-only mode; every
subscription and accepted package must match it. Never run independent service
journals for the two chains in the same period.

```sh
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  ready --request work-request.json --signature operator-work-signature.json
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  --auth-file /private/child-prover-basic-auth --gateway-auth-file /private/gateway-prover-basic-auth pick
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  offer-request --operation job-000000 --output offer-request.json
# The sequencer wallet signs offer-request.json's typed_data.
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  authorize --operation job-000000 --signature sequencer-offer-signature.json
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  export --operation job-000000 --output-directory /private/worker-handoff-000000
```

The exported `payload.json` is the ordinary native FRI input, accompanied by the
signed work manifest, subscription snapshot, and producer metadata. Deliver this
export to the selected operator through the operator's existing authenticated
transport. The adapter is a file handoff CLI; it does not expose a new public HTTP
server. `GET /prover-jobs/v1/FRI/{batch}/evidence` supplies pending producer
metadata, and `nonempty_only=true` on FRI pick applies the transaction-count filter
inside the atomic lease predicate. Ordinary picks default to the existing behavior.
Neither metadata endpoint attests independent execution or accepted proof work.
The export also contains its exact lane `config.json`; use that configuration for
operator duty signing and package construction. Remote endpoints require HTTPS;
plain HTTP is limited to loopback native APIs.

The shared Runpod [pool](../prover-rental/pool-README.md) can enqueue this stripped
payload and evidence in its external-input mode. It returns a proof artifact
without acquiring or submitting another native lease. The dispatcher remains the
sole owner of its origin lease and signed duty slot.

The worker returns the ordinary FRI proof JSON. The operator signs the generated
`DutySuccessV1` request, which binds that exact proof hash and the native statement.
The host then submits the original private lease to the existing native verifier:

```sh
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  proof-request --operation job-000000 --proof fri-proof.json --output duty-request.json
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  --auth-file /private/child-prover-basic-auth --gateway-auth-file /private/gateway-prover-basic-auth \
  submit --operation job-000000 \
  --proof fri-proof.json --signature operator-duty-signature.json
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 report
```

Only the native verifier's accepted disposition, or recovery of exactly the same
retained proof and canonical metadata, produces `duty.json`. The private job folder
also retains the exact submission and `authority.json` accepted evidence consumed
by `service.py prepare-duty`. Never export that authority, `picked-wire.json`,
`submission.json`, the BasicAuth secret, or the state journal: they contain live
capabilities. `export` copies an allowlist and includes the signed duty only after
native acceptance. Native FRI acceptance remains separate from on-chain package
acceptance and service admission; final credit still requires the accepted native
SNARK package and settlement delivery.

Retry uncertain submissions with the same proof and signature. Changed bytes are
rejected. An ambiguous submission cannot be expired or reassigned; if retained
proofs have already been pruned, the adapter conservatively remains unresolved and
requires operator investigation. `recover-pick` resumes a fully retained pick after
a crash. If the pick response was lost before it could be retained, the journal
stops automatic picking. `abandon-unexported-pick` can explicitly release only a
pending reservation whose private job directory is still empty, with no retained
response, assignment, offer, authority, or submission. It durably records the
abandonment, preserves readiness and cursor, and creates no opportunity. Any
unknown native lease simply expires; its absent capability cannot be exported or
used for a submission. The next pick is fresh work, never replay of that request.
A definite rejection or an
expired signed offer can be retried only when the ordinary node returns a fresh
lease; the manifest preserves the previous assignment, increasing attempt number,
and retry reason. Use `expire --operation ...` only after the signed readiness
deadline. An expired unsigned reservation consumes no opportunity.

For a multi-batch package, `manifest-request --operations job-000000 job-000001
--output package-manifest-request.json` returns the exact combined payload and
sequencer signing request, including retry history. Assemble its signed manifest
as `{"payload": ..., "sequencer_signature": "0x..."}` and pass it with the retained
accepted duties and independent execution comparison to the existing package
builder. Do not count an older assignment as a second duty for the same batch.
All operations in one package must belong to the same execution lane; the tool
rejects a mixed-chain manifest. Each lane uses its own gate, parent, native proof,
selected wrapper and settlement transaction journal, while their accepted duties
feed the same child-registry slots and bonus accounting.
The adapter never splits native batches, manufactures work, or claims that a proof
reveals which physical GPU produced it. Packaged canonical fixture certification
remains separately gated; materializing the reviewed real-VK sources does not
certify either that fixture or this service's deployed message and recovery paths.

The opt-in [node publication mode](node-publication.md) connects confirmed service
receipts to the native batch pipeline, including restart recovery, without a
node proof signing key.
