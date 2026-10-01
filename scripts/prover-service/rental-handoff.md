# Dispatcher → Sentry → rental → dispatcher

In the service workflow, the trusted sequencer's dispatcher keeps the native
upstream Basic Auth and lease. A Sentry consumes the dispatcher's safe export;
it must not call the upstream pick endpoint to bypass assignment or receive the
dispatcher authority file. The export contains `payload.json`, `manifest.json`,
`subscriptions.json` and producer `evidence.json`, with no lease capability.

Before spending compute, the Sentry authenticates the signed manifest and account
subscriptions and compares the statement/output preimages with its independent
EN. The producer evidence export is source data for comparison, not an assertion
of independent verification. Keep the reviewed immutable stage `release.json`
and scoped storage plan on the Sentry host.

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
  --evidence /trusted/independent-en.json --manifest /trusted/offer/manifest.json \
  --subscriptions /trusted/offer/subscriptions.json --proof /trusted/returned-fri.json
```

This authenticates the signed current assignment and subscriptions, derives the
full EN statement/count, and hashes the exact returned proof. Its EIP-712 digest
is the same duty digest later checked after native acceptance. The request is
explicitly marked `native_acceptance: pending`; it is an operator endorsement of
an offered result, not a successful service receipt. The signed manifest commits
the private lease's hash. Only the dispatcher can compare that commitment to its
actual current lease and reject a stale offer.

After independently verifying the native proof, the operator wallet signs the
request. Return the stripped proof and signature through the authenticated
dispatcher handoff. The trusted dispatcher checks the operator signature, attaches
its retained lease, and submits to the native verifier-backed API. Only the actual
accepted disposition (or recovery of the identical retained accepted proof)
permits its durable accepted duty. A proxy response, rental result, or signature
alone cannot admit work or earn bonus.

For service SNARK compute, follow the complete
[keeper and settlement workflow](keeper-operator-guide.md). The external pool
requires a current selected-wrapper permit. The trusted sequencer then uses
`sentry.py import-result` to bind the completed external result to its original
SNARK lease before native submission. Keep the same job ID, release, payload and
trusted evidence snapshot throughout. Native acceptance feeds the node's
[service publication handoff](node-publication.md), package endorsements and
durable relay. Both child and Gateway lanes share this workflow and one bonus
ledger; a rental receipt alone earns no service credit.
