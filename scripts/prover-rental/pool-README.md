# Child and Gateway compute pool

`pool.py` coordinates child-chain and Gateway FRI/SNARK jobs through the same
bounded rental pool. It runs on a trusted operator host. Each execution lane has
its own identity, protected job journal, and budget; each lane/stage has its own
frozen image policy and Runpod controller. Global limits bound all four stages.
The pool never signs a service duty, chooses a wrapper, or awards credit. Those
remain the authenticated service dispatcher, operator, and settlement contracts'
responsibility. There is one native proof computation and one native acceptance
path per job.

The normal service path uses `acquisition: "external"`: the service dispatcher
has already picked and authorized work, keeps its genuine native lease, and
exports a stripped input with evidence. The operator validates that signed offer
and independent execution evidence using the service tools, then supplies the
input to this pool. The pool performs identity and integrity checks; it does not
independently authenticate a service manifest or replay an execution node. A
successful `enqueue` therefore means compute admission, never service authority.
External-only lanes have `auth_file: null` and retain no native credentials.
Every external SNARK stage additionally requires `service.keeper_config_file`
and a keeper-issued compute permit. Its keeper configuration is frozen at pool
initialization. FRI stages use `service: null`. The keeper verifies bootstrap or
the actual selected-wrapper turn before SNARK rental; see the
[keeper operator guide](../prover-service/keeper-operator-guide.md).

An explicitly configured `acquisition: "native"` stage instead lets `pick-next`
acquire jobs directly from its originating API. This supports native compute
operation without a service duty; use the external path for work governed by the
signed service dispatcher. Native picks rotate child FRI, Gateway FRI, child
SNARK, Gateway SNARK, skipping stages without capacity. A single call makes at
most one pick. An ambiguous pick blocks another pick in that stage, and continues
consuming its lane/global slot and budget.

## Configuration and initialization

Copy `pool-config.example.json` to a mode-0600 file and fill every null identity,
price, and release field. Read actual `chain_id`, `chain_address`,
`settlement_chain_id`, protocol, and VK from each configured chain's authenticated
evidence endpoint; then compare them with the intended deployment. Never infer
the execution chain from a VK or batch number: both chains may share them. The
example local ports are Gateway 3124 and child 3125. HTTPS endpoints and explicit
loopback HTTP tunnels are accepted. Requests disable redirects and environment
proxies. Object storage still requires HTTPS.

The pool requires distinct execution-chain IDs and endpoints. Frozen release
hashes must match each stage and lane VK. A native stage requires a private
`user:password` auth file, copied into that lane only. Configure
`native_lease_seconds` to at least the actual originating server lease lifetime;
the image runtime must be shorter. This value bounds native expiration recovery,
so a shorter-than-server value is invalid operational configuration. External
imports additionally require the actual signed offer/lease deadline from the
dispatcher, expressed as an absolute Unix timestamp.

```sh
python3 scripts/prover-rental/pool.py --state-dir /secure/zksys/pool \
  --execute init --config /secure/zksys/pool-config.json
```

Initialization freezes the configuration, releases, and four rental policies.
Budget reservations are permanent after admission, including failed work; a
definitive empty native response releases its reservation. Both per-lane and
global limits apply before a pick/import, and each stage's controller also
enforces its own limit. All launches using these controllers must pass through
the pool; launching directly through `runpod.py` would bypass pool-level limits.
Use a new reviewed pool configuration/journal for a new budget period, after
resolving the old pool's leases and allocations.

Run an independently supervised `runpod.py watchdog` for each of
`/secure/zksys/pool/{child,gateway}/{FRI,SNARK}/controller` before launching work.
For example, the Gateway FRI supervisor runs:

```sh
python3 scripts/prover-rental/runpod.py \
  --state-dir /secure/zksys/pool/gateway/FRI/controller \
  --api-key-file /secure/zksys/runpod-key --execute watchdog
```

Do not replace these independent watchdogs with a parent process waiting for a
proof. They own timeout cleanup if the pool or operator process exits. Provider
outages can still prevent timely deletion; configured budgets are admission
bounds, not an absolute provider billing guarantee.

## Import, rent, and return service work

The exported payload must use the exact lease-free v1 FRI/SNARK wire shape.
Evidence must name the configured execution identity and complete matching
batch range. The durable job ID should be the dispatch attempt ID; reusing it
with a different lane, payload, evidence, or deadline fails. Retrying the same
import recovers the same reservation and job directory without another pick.

```sh
python3 scripts/prover-rental/pool.py --state-dir /secure/zksys/pool --execute \
  enqueue --lane gateway --stage FRI --job-id gateway-duty-12-attempt-1 \
  --payload /secure/zksys/offer/payload.json \
  --evidence /secure/zksys/offer/evidence.json --deadline UNIX_OFFER_DEADLINE
```

For external SNARK work, obtain the keeper's permit for the exact native payload,
then include `--compute-permit /secure/zksys/compute-permit.json` in `enqueue`.
The pool rechecks its immutable lane/package binding, canonical RPC state and
remaining turn time immediately before creating a paid rental. Expired turns
cannot start fresh compute; recovery still discovers an existing allocation.

Read `OPERATION_ID` from the returned status, then publish that job's unique
scoped storage plan and rent its frozen image:

```sh
python3 scripts/prover-rental/pool.py --state-dir /secure/zksys/pool --execute \
  export --operation-id OPERATION_ID --storage-plan /secure/zksys/storage-plan.json
python3 scripts/prover-rental/pool.py --state-dir /secure/zksys/pool \
  --api-key-file /secure/zksys/runpod-key --execute launch --operation-id OPERATION_ID
python3 scripts/prover-rental/pool.py --state-dir /secure/zksys/pool \
  --api-key-file /secure/zksys/runpod-key --execute complete --operation-id OPERATION_ID
```

`complete` durably verifies the transport receipt and proof range, stops the
owned rental, and writes `returned-proof.json` in that lane's job directory.
It does not call the native API for an external input. The trusted operator uses
that proof in the signed service handoff, and the original dispatcher reattaches
its retained real lease for native acceptance. Neither that lease, node
credentials, nor any signing keys reach the rental. See
[`rental-handoff.md`](../prover-service/rental-handoff.md) for the service boundary.

The initial v1 rental manifests bind lane, execution/settlement identity, VK,
canonical evidence SHA-256, payload SHA-256, and an origin-endpoint hash. The
worker checks these fields against its release and downloaded payload. The
result receipt binds the entire manifest hash. The native proof verifier still
provides cryptographic validity; a chain label alone cannot make a proof valid.

## Native acquisition and recovery

For a stage explicitly configured `native`, use `pick-next` instead of `enqueue`.
The pool durably reserves capacity before POSTing. It retains the full pick
response, original endpoint and credentials, and original lease in that lane's
private job directory. It fetches `/FRI/{batch}/evidence` or
`/SNARK/{from}/{to}/evidence` and freezes matching identity before allowing export.
`complete` uses only that origin and exact lease for submission. A mismatch or
missing evidence cannot be exported or silently reclassified as another lane.

Use `recover-pick --operation-id ...` after a durable pick response survived an
interruption. A missing response remains uncertain and cannot be replaced by a
new pick. A launch retry discovers its already-journaled rental even if the
native deadline has since elapsed; it never creates a second allocation for the
same attempt. Unmarked native responses retain the exact submission for retry.
No status here proves reward eligibility or on-chain finality.

`expire --operation-id ...` releases a known job's scheduling slot only after its
deadline and after every rental for that job has a terminal provider state.
Native jobs conservatively wait a full configured lease duration after the pick
response was durably recovered. Unknown picks, mismatched evidence, and pending
native submissions require origin-side reconciliation and cannot be expired
blindly. Expiration never refunds the lifetime budget reservation.

`status` prints only lane, stage, mode, status, reservation, rental operation,
and native acceptance status. Each action defaults to dry-run without
`--execute`. The tests use mocked native/provider endpoints and a loopback native
worker adapter; they do not qualify real GPU performance or a live deployment.
