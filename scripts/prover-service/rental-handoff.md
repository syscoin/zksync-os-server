# Trusted service handoffs and rental compute

Runpod lifecycle code lives in **zksync-airbender-prover**. Set
`ZKSYNC_AIRBENDER_PROVER_DIR` to that checkout to use this server's compatibility
commands, or run the corresponding scripts directly from the prover repository.
The reusable supervisor can feed successive authorized jobs to the same pod;
each attempt retains the immutable handoff and result checks described below.

In the service workflow, the trusted sequencer's dispatcher keeps the native
upstream Basic Auth and lease. A Sentry consumes the dispatcher's safe export;
it must not call the upstream pick endpoint to bypass assignment or receive the
dispatcher authority file. The export contains `payload.json`, `manifest.json`,
`subscriptions.json` and producer `evidence.json`, with no lease capability.

Before spending compute, the Sentry authenticates the signed manifest, account
subscriptions, assignment and native statement/output commitments. Every
subscription covers both services (`services: 3`); these are the same operators
that enter the qualified wrapper roster. Keep the reviewed immutable stage
`release.json` and scoped storage plan on the trusted host. Producer metadata is
input to verification, not an assertion of proof validity. This wrapping flow
requires no EN: the selected wrapper authenticates canonical settlement commitments,
verifies every native FRI on CPU, and verifies the resulting native SNARK before
signing.

After service activation, `coordinator.py` and `wrapper.py` automate the SNARK
handoff using the operator's existing warm pool. The sequencer retains the native
lease, signs the complete dispatcher manifest and audit snapshot, opens the
package, and delivers a turn-bound work envelope only to the selected operator.
The wrapper checks its own audit trust pins, canonical chain state and native
FRIs before enqueueing work. It returns a signed proof result and, while still
selected for the exact work, the package endorsement. The sequencer verifies the
result, submits its original native lease, waits for node publication, and relays
the jointly endorsed package. See the [operator guide](keeper-operator-guide.md)
for configuration and supervised startup. The commands below remain useful for
FRI offers and manual inspection or recovery.

Create the ordinary one-job rental manifest directly from that stripped payload:

```sh
python3 scripts/prover-rental/sentry.py --job-dir /trusted/compute-attempt \
  export-input --payload /trusted/offer/payload.json \
  --release /trusted/fri-release.json --job-id ASSIGNMENT_ID \
  --storage-plan /trusted/storage-plan.json
```

This first invocation is a dry run. Add `--execute` before `export-input` to
persist and upload. The helper validates the fixed native payload and release,
rejects any lease field, freezes the input/stage/VK/job identity and storage
capabilities, then uses the same publication code as the direct trusted lease
handoff. It uploads the payload first and manifest last. Interrupted publication
can retry only those same inputs/capabilities. It creates `input.json`, not an
upstream `authority.json`.

Use `compute-attempt/controller-job.json` with the bounded rental controller and
its separately supervised watchdog as described in `../prover-rental/README.md`.
The included worker runs one FRI or SNARK job and publishes its stripped native
submit payload. Once the controller has a durable local receipt, recheck it:

```sh
python3 scripts/prover-rental/sentry.py --job-dir /trusted/compute-attempt \
  --execute verify-result --controller-state /trusted/rental-controller \
  --operation-id RENTAL_OPERATION_ID --output /trusted/returned-fri.json
```

This verifies the controller's job/manifest and exact artifact hash plus returned
stage, VK and batch range. It freezes the operation/result hash and writes only
the stripped native proof. It neither picks nor submits upstream, never creates a
lease, and makes no native-proof acceptance claim. A matching file hash alone does
not establish proof validity.

For a FRI service duty, prepare the operator's response without a private lease:

```sh
python3 scripts/prover-service/service.py --config /trusted/config.json \
  --output /trusted/offered-duty-request.json offered-duty \
  --evidence /trusted/native-evidence.json --manifest /trusted/offer/manifest.json \
  --subscriptions /trusted/offer/subscriptions.json --proof /trusted/returned-fri.json
```

This authenticates the signed current assignment and subscriptions, derives the
full native statement/count, and hashes the exact returned proof. Its EIP-712 digest
is the same duty digest later checked after native acceptance. The request is
explicitly marked `native_acceptance: pending`; it is an operator endorsement of
an offered result, not a successful service receipt. The signed manifest commits
the private lease's hash. Only the dispatcher can compare that commitment to its
actual current lease and reject a stale offer.

After checking the authenticated assignment, statement and exact returned proof,
the operator wallet signs this pending-result request. Return the stripped proof
and signature through the authenticated
dispatcher handoff. The trusted dispatcher checks the operator signature, attaches
its retained lease, and submits to the native verifier-backed API. Only the actual
accepted disposition (or recovery of the identical retained accepted proof)
permits its durable accepted duty. A proxy response, rental result, or signature
alone cannot admit work or earn bonus. The selected wrapper verifies these native
FRIs again as part of the aggregate input before endorsing a SNARK package.

For service SNARK compute, the external pool requires a current selected-wrapper
permit. The automated coordinator authenticates the wrapper's signed result,
checks the native SNARK, and submits the retained original lease through
`native_handoff.py`. The manual `sentry.py import-result` path is available when
the external pool and original Sentry job share its required job/release identity;
it must not be substituted for a wrapper work envelope with a different identity.
Keep each path's frozen payload, release and native evidence intact. Native
acceptance feeds the node's [service publication handoff](node-publication.md),
package endorsements and durable relay. Child and Gateway lanes share the operator
pool and bonus ledger; rental receipts and wrapper selection create no new duty
credit, fee entitlement, or bonus weight.
