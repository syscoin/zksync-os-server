# Runpod proving compute controller

`runpod.py` is an opt-in Python 3.10+ / Unix controller for a single, bounded compute
job per pod. It uses the Runpod **v2** API and is dry-run by default. No credentials,
rentals, running services, provider accounts or chain configuration are created by
checking in these files or running the tests.

The controller is an integration boundary, not a replacement prover. It neither
picks a sequencer lease nor signs, verifies, submits or rewards a proof. The trusted
Sentry process must retain the exact leased input and token, validate the returned
proof through the existing proving pipeline, and complete its durable submission
workflow. A copied artifact's SHA-256 establishes byte integrity, **not** proof
validity, computation attribution or availability credit.

For a shared child-chain and Gateway pool, use
[`pool.py`](pool-README.md). It adds separate chain identities and journals,
per-lane and global budget caps, and import/return of already dispatched work
without acquiring another native lease. Both FRI and SNARK stages use the same
worker adapter and receipt checks described here.

## Configure without guessing hardware

Copy `policy.example.json` to a private operator file, replace every placeholder,
and set mode `0600`. The deliberately invalid template contains no assumed RAM,
VRAM, image, CUDA, duration or price defaults. Qualify the complete stage on the
selected hardware first, including cold setup, retained host caches, merge,
compression, wrapping and artifact return. Choose a reviewed, public container
image by immutable SHA-256 digest; mutable tags and templates are rejected.

`gpu` uses Runpod v2 field names: exact GPU ID/count, minimum host RAM and vCPUs per
GPU, and a tested `major.minor` minimum CUDA version. `min_vram_gb` checks catalog
VRAM separately. A minimum CUDA version is a host selection constraint, not an
attestation of driver compatibility; the image's worker must validate the actual
runtime before proving. Choose explicit allowed data centers and ephemeral disk
capacity. This controller does not attach, create or delete network volumes,
persistent mounts, registry credentials or reusable caches.

Prices are decimal USD strings. `additional_hourly_usd` is an operator-supplied
allowance for storage/other hourly charges beyond GPU compute. Catalog price times
GPU count plus that allowance must fit `max_hourly_usd` before creation. After
creation, the reported pod rate plus the allowance is checked again. Unknown or
excessive rates trigger failure cleanup.

`max_operation_usd` must cover the configured maximum hourly rate times maximum
runtime. Every launch permanently reserves this amount against
`lifetime_budget_usd`, even if refused or terminated early; there is no automatic
budget reset/refund. Active and uncertain creates occupy concurrency slots. These
limits apply to **one persistent controller directory**, not to all rentals in a
provider account. All launchers for this fleet must use that same directory.

Runpod v2 creation has no documented atomic maximum-price condition. Catalog
quotes can change before allocation, and provider outages can prevent termination.
The controller therefore bounds intended spending and actively enforces deadlines;
it cannot guarantee an absolute provider invoice ceiling. Configure provider-side
billing controls and monitor the independently supervised watchdog. Extra storage,
egress, pricing changes and termination delay require an operator budget margin.

## One-job image contract

The image ENTRYPOINT must accept these literal argv arguments (no shell expansion):

```text
--operation-id UUID_HEX
--job-id OPERATOR_UNIQUE_DUTY_ATTEMPT_ID
--manifest-url HTTPS_INPUT_MANIFEST_URL
--manifest-sha256 SHA256_OF_EXACT_INPUT_MANIFEST_BYTES
--runtime-limit-seconds SECONDS
```

The trusted caller supplies a `0600` job JSON file:

```json
{
  "schema_version": 1,
  "job_id": "chain-stage-duty-attempt",
  "manifest_url": "https://inputs.example/one-job-manifest",
  "manifest_sha256": "REPLACE_WITH_TRUSTED_64_LOWERCASE_HEX_DIGITS",
  "result_manifest_url": "https://results.example/one-job-result-manifest",
  "result_artifact_url": "https://results.example/one-job-proof"
}
```

The included `sentry.py` prepares a manifest for one immutable v1 FRI/SNARK job,
retaining the real lease and endpoint locally. The included `worker.py` image
adapter authenticates the manifest and payload hashes, checks the image's frozen
guest/worker/CRS and V32/V7/V8 Security100 identity, and invokes the existing
standalone stage binary for one iteration. It uploads exact proof bytes first and
publishes the result manifest **last**.

The native binary talks only to an isolated loopback one-job v1 API; its synthetic
local token has no authority on the real sequencer. The adapter's acknowledgment
means only that output bytes were fsynced, never chain acceptance or reward credit.
No arbitrary executable, shell command or command-line flags arrive in a manifest.

The result manifest contains exactly:

```json
{
  "schema_version": 1,
  "operation_id": "UUID_HEX_FROM_ARGV",
  "job_id": "chain-stage-duty-attempt",
  "manifest_sha256": "INPUT_MANIFEST_SHA256",
  "artifact_sha256": "RETURNED_PROOF_BYTES_SHA256",
  "artifact_bytes": 123
}
```

The controller reads only the two operator-configured result URLs. It never
follows a URL or local path supplied by the untrusted result manifest, follows
redirects, or extracts an archive. HTTP reads have byte/deadline bounds. A missing
result manifest (`404`) means pending until the rental deadline. Use unique
object locations and restrict capabilities to these objects, with expiration
long enough for crash recovery; an unreadable result is not successful receipt.

The rental receives no blockchain, staking, endorsement, sequencer or Runpod
account keys. Only operation/controller identifiers are injected as environment
variables. Presigned URLs are limited job capabilities and are visible to the
provider; keep them out of logs. Do not put account keys in the image, manifest or
URLs. Input manifests and storage capabilities are the caller's responsibility.

## Build each stage image and hand off a real lease

Create one reviewed `release.json` per stage using `release.example.json`. It must
contain the nonzero production app-bound VK, Security100 program commitment and
SHA-256 hashes of the **actual** guest `.bin`/`.text`, standalone native worker,
and GPU compact CRS for SNARK. FRI uses `crs_sha256: null`. These are immutable
image inputs, not runtime fields a rental may choose. Both build-time and startup
checks hash those files. The native workers additionally enforce their compiled
app/program/VK constants. The server's registered V8 app-bound VK is
`0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe`,
bound to the reviewed guest tree
`6935489bdbc7b1ed31e608677d1b2418b10691b5`; see the
[registered proving identity](../../lib/types/src/protocol/proving_version.rs)
and [validated source release](../releases/era-v32/README.md).
Use stage binaries built for that identity and hash their actual image contents.
The source release does not qualify a rental image, selected hardware, live
service deployment, or the still-absent canonical local-chain fixture.

The base image must already contain `/usr/bin/zksync_os_fri_prover` or
`/usr/bin/zksync_os_snark_prover`, `/multiblock_batch.bin`, and
`/multiblock_batch.text`; GPU SNARK also needs `/setup_compact.key` and a standalone
worker built with its GPU backend. This does not relabel the existing CPU SNARK
image as GPU capable or modify the prover repository. Use the qualified optimized
build when available. `build-image.py` requires the base's immutable registry
digest, installs Python during image build, copies only adapter code/release
metadata into a temporary build context and runs file-identity validation:

```sh
python3 scripts/prover-rental/build-image.py \
  --base-image REGISTRY/QUALIFIED_STAGE_IMAGE@sha256:EXACT_DIGEST \
  --release /absolute/release.json --tag REGISTRY/rental-stage:RELEASE

# Add --execute to build. --execute --check performs Docker's static checks only.
```

Build and publish through the normal release workflow, then put the final
published adapter image **digest** in the rental policy. The template metadata
cannot be built as a production release until all placeholders are filled; the
checks never generate a VK or bypass release gates. Local protocol tests simulate
the native binary's actual v1 HTTP exchange, not its cryptographic computation.

Keep `sentry.py` on the trusted Sentry host. It uses the existing protected node
API and never sends the real lease or API credentials to storage or the rental.
Its commands are dry-run unless `--execute` is present. Supply `user:password`
through a mode-`0600` `--auth-file`, or `PROVER_RENTAL_BASIC_AUTH` in the local
service environment. The endpoint is the HTTPS **base** URL before
`prover-jobs/v1/`.

```sh
# Reserves a NEW private job directory before the lease-changing request.
python3 scripts/prover-rental/sentry.py --job-dir /absolute/new-duty-attempt \
  --auth-file /absolute/node-api-auth --execute pick \
  --endpoint https://protected-prover.example/ --release /absolute/release.json \
  --job-id CHAIN-STAGE-DUTY-ATTEMPT

python3 scripts/prover-rental/sentry.py --job-dir /absolute/new-duty-attempt \
  --execute export --storage-plan /absolute/scoped-storage-plan.json
```

The owner-only storage plan contains exactly eight HTTPS URLs: `payload_put_url`,
`payload_get_url`, `manifest_put_url`, `manifest_get_url`, `artifact_put_url`,
`artifact_get_url`, `result_manifest_put_url`, `result_manifest_get_url`. Configure
one immutable object for each payload, manifest, proof and result manifest, with
short-lived capabilities limited to that object's needed method. A PUT must
publish a complete object atomically; this is the required object-store contract.
The controller and adapter never create buckets, grant IAM permissions or get
storage-account keys. The trusted operator's existing storage service issues
these capabilities.

Export uploads the stripped exact v1 payload first and the hash-bound manifest
last. FRI payloads contain `batch_number`, `vk_hash`, `prover_input` (base64
little-endian u32 bytes); SNARK payloads contain `from_batch_number`,
`to_batch_number`, `vk_hash`, and 2–100 original base64 FRI proofs. A real
`lease_token` field is explicitly rejected at the rental boundary. The helper
produces `<job-dir>/controller-job.json` for the `launch --job` command below.

The returned artifact is the native worker's exact **wire proof payload**, stripped
of its synthetic local token. In particular, SNARK's `proof` is the existing
base64 EVM-word encoding, not a guessed serialization of its Rust proof object.
After the controller collects it, submit from the same trusted host:

```sh
python3 scripts/prover-rental/sentry.py --job-dir /absolute/new-duty-attempt \
  --auth-file /absolute/node-api-auth --execute submit \
  --controller-state /absolute/rental-state --operation-id UUID_HEX
```

This checks the job/manifest, local receipt hash, VK and exact batch range, then
reattaches the locally retained real token. Before any network submission it
fsyncs the exact `submission.json` body. Retries reuse those exact bytes. Only the
existing manager's marked accepted/rejected disposition retires the attempt;
unmarked proxy responses and transport failures leave it pending. The protected
node performs its native FRI verification or on-chain SNARK preflight before
acceptance. If a previous acceptance response was lost and a retry reports stale
authority, reconcile canonical node progress before assessing participation.

A pick timeout/crash retains its reservation; never issue another pick in that
directory. When complete `picked-wire.json` was fsynced before the crash,
`sentry.py --job-dir ... --execute recover-pick` reconstructs its authority and
stripped payload without another lease request. With no complete wire response,
recover against the node/lease timeout before starting a new attempt. Preserve
the entire job directory until its actual disposition is resolved.

The actual node lease starts **before** rental provisioning. Its deadline must
cover allocation, image download, cold setup, proof generation, upload and trusted
submission; this adapter does not renew leases or extend contract turns. Trigger
only an eligible real duty. Membership, contract-selected wrapper authorization,
independent EN validation and endorsement remain on the trusted Sentry side.

## Operate

Run these commands from the repository root. Paths below are placeholders. The
state directory must be a new absolute path on durable local Unix storage with
working `flock`, rename and file/directory `fsync`; ephemeral container layers,
object mounts and unqualified network filesystems are unsuitable. Protect its
backups as secrets because the journal contains job URLs. Do not delete or reset
it to resolve ambiguous provider operations.

```sh
chmod 600 /absolute/policy.json /absolute/job.json /absolute/runpod-key

# Validate policy with no state changes or network access.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  init --config /absolute/policy.json

# Initialize the immutable policy and private durable journal locally.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --execute init --config /absolute/policy.json

# Run separately under systemd/another service supervisor on the trusted host.
# This process monitors rentals; it never launches a new one itself.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --api-key-file /absolute/runpod-key --execute watchdog

# Preview a launch; no API credential or provider request is used.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  launch --job /absolute/job.json

# Explicitly authorize this bounded rental. A fresh, separately running watchdog is required.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --api-key-file /absolute/runpod-key --execute launch --job /absolute/job.json

python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state status
```

`RUNPOD_API_KEY` may replace `--api-key-file`; do not put the key in argv. The key
is used only for `api.runpod.io` in the trusted process. Restrict its provider
permissions to the required pod/catalog operations. Raw provider responses,
request payloads and URL-bearing exceptions are not printed. Public status omits
job URLs and credentials. No HTTP listener, SSH server or public port is opened
by the controller.

The watchdog polls, collects complete results and terminates pods after durable
local receipt. It also requests cleanup on runtime expiry, clock rollback,
reported hardware mismatch, unknown/excessive price or a stopped/failed pod.
Cleanup on those failures can discard unfinished remote work; the trusted worker
must repair/reassign the retained duty independently. Configure service restart
and alert on watchdog errors/staleness. Stopping the watchdog does **not** stop
cloud billing. New launches fail while its lock/heartbeat is absent or stale.

After collection, `<state-dir>/<operation-id>.proof` holds the exact private,
fsynced bytes; `state.json` records their size/hash and `proof_verified: false`.
The trusted worker must read this receipt, validate job identity and prove/verifier
compatibility, independently verify the proof, and submit with its existing lease
and durable spool. Do not credit rewards merely because a file was collected.

`../prover-service/service.py` can use the trusted host's accepted FRI authority
to prepare a current-lease-bound duty signature request. Its manifest and EN
validation remain on the trusted host; service signing material never belongs
in the rental image. See `../prover-service/README.md` for the staged opt-in
workflow and remaining deployment/independent-source launch gates.

Optional manual actions use the same ownership checks:

```sh
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --api-key-file /absolute/runpod-key --execute reconcile --operation-id UUID_HEX

python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --execute collect --operation-id UUID_HEX

# Normal cleanup requires the locally rechecked durable artifact receipt.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --api-key-file /absolute/runpod-key --execute terminate --operation-id UUID_HEX

# Explicit failure cleanup authorizes losing uncollected remote work.
python3 scripts/prover-rental/runpod.py --state-dir /absolute/new-rental-state \
  --api-key-file /absolute/runpod-key --execute terminate --operation-id UUID_HEX \
  --failure-cleanup
```

## Ambiguous operations and recovery

Dispatcher-assigned operators should use `sentry.py export-input` and
`verify-result`, which never receive or acquire an upstream lease. The exact
Sentry/rental/signature/dispatcher sequence is documented in
[`../prover-service/rental-handoff.md`](../prover-service/rental-handoff.md).

An operation UUID, unique pod name, controller/operation markers and complete
request are durable **before** the only create POST. Reusing the same job ID
returns its existing operation; changing its job contents is rejected. A new
attempt must have a new duty-attempt ID and fit all existing reservations.

Timeouts, 5xx, 429, malformed success responses and interrupted creates stay
uncertain. Reconciliation walks the complete paginated pod list and adopts only
one matching name **and** exact controller/operation markers and image digest.
Zero matches never authorize another POST. Multiple matches or changed ownership
require operator investigation; the controller does not guess which pod to
delete. Confirm any unresolved allocation with Runpod support/control-plane
records before deciding recovery. There is deliberately no unsafe “forget and
retry” command. Alert on this condition because unknown or conflicting allocations
can continue billing beyond local deadlines.

Each delete checks the stored pod ID and exact ownership markers against a fresh
provider read and journals delete intent before sending. An ambiguous delete is
reconciled first; it is repeated only if that same owned pod still exists. `204`
alone does not close the operation: a subsequent read must report disappearance
or `TERMINATED`. An ordinary GET `404` without prior delete intent keeps the
reservation because a just-created allocation might not be visible yet.

Atomic state writes preserve the previous complete journal if interrupted.
Interrupted artifact transfers have no receipt; an artifact fully persisted just
before a journal crash is checked again and adopted without redownloading. Never
share the state directory between independent operators or remove reservations
while provider outcomes remain unknown.

## Validation and API references

```sh
python3 -m unittest discover -s scripts/prover-rental -p 'test_*.py' -v
```

Tests use mocked providers/transports, including ambiguous create/delete,
restart/fsync boundaries, wrong ownership, slot/budget exhaustion, corrupt proof
bytes, stale watchdog, hardware/price failures and dry-run redaction. No live
provider validation has been performed by this change.

API contracts inspected 2026-09-30: [create](https://docs.runpod.io/api-reference-v2/pods/create-a-pod),
[list and pagination](https://docs.runpod.io/api-reference-v2/pods/list-pods),
[get](https://docs.runpod.io/api-reference-v2/pods/get-a-pod),
[terminate](https://docs.runpod.io/api-reference-v2/pods/terminate-a-pod),
[GPU catalog pricing](https://docs.runpod.io/api-reference-v2/catalog/get-a-gpu-type).
