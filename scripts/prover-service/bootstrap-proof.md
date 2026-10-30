# Initial uncredited bootstrap proof

The first membership delivery can require a native proof before any Sentry is
eligible to subscribe. This path uses the sequencer with an empty subscription
snapshot and an empty duty report. It does not credit a Sentry or create a
qualified proving roster. The ordinary coordinator and wrapper require an
activated service and cannot perform this pre-activation operation.

This is a controlled manual workflow, not an end-to-end launcher. Configure the
two proof lanes, priority guard, node service-publication mode, dedicated wallet
nonces, external prover pool and exact production verifier first. Gateway prover
role changes are separate L1 ChainAdmin priority messages; do not send them as
direct Gateway transactions. Preserve every native lease, pool and relay journal.

1. Pick a native SNARK lease and matching evidence as described in the
   [keeper guide](keeper-operator-guide.md#manual-inspection-and-recovery-of-the-native-snark-lease).
   The node must already have accepted the real FRI proofs for at least two
   consecutive batches. Retain the original lease and its `payload.json` and
   fetch evidence for that exact range. Do not create a second picker for a
   possibly live lease.

2. Create the empty manifest signing request with `bootstrap_control.py`, sign
   its `request` with the sequencer, and write the resulting manifest as a
   mode-0600 JSON file with exactly `payload` and `sequencer_signature` fields.
   The sequencer signature is the plain `0x` EOA signature for that request.
   The keeper guide shows the `bootstrap_control.py` invocation. Select a
   nonzero cursor, retain it with the artifacts, and pin an independently
   reviewed finalized enrollment block containing no eligible subscribers.

3. Build the exact frozen request and simulated gate-opening call:

   ```sh
   python3 scripts/prover-service/bootstrap_package.py \
     --keeper-config /trusted/bootstrap-keeper.json \
     --evidence /trusted/native-evidence.json --payload /trusted/lease/payload.json \
     --output /trusted/bootstrap-request-and-call.json request \
     --manifest /trusted/manifest.json --beneficiary 0xREVIEWED_SEQUENCER_BENEFICIARY
   ```

   Extract the `request` object to a private JSON file. Review and submit the
   returned `open_call` using the sequencer's durable signing-only transaction
   supervisor, then confirm its canonical receipt. This helper only produces a
   simulated call; it does not manage that transaction or nonce. If the package,
   evidence, or parent changes, obtain and review a fresh call; preserve any
   already-signed transaction until its nonce is resolved.

4. Publish the required priority checkpoint and prefix witness, and use
   `keeper.py permit` for the exact request, evidence and native payload. Enqueue
   the SNARK in the pinned prover pool with that permit, preserving the original
   lease deadline. Follow the pool's `export`, `launch`, and `complete` sequence
   in the [prover pool guide](../prover-rental/pool-README.md). Import the result
   and submit it through the original native lease as in the keeper guide. Neither
   a successful rental nor a native API acknowledgement alone verifies the proof.

5. Verify the exact ordered native FRIs with the locally pinned verifier and the
   SNARK against the canonical production verifier, then construct the signing
   package. The verifier JSON has exactly `executable`, `sha256`, and
   `timeout_seconds`, as in the wrapper configuration in the keeper guide:

   ```sh
   python3 scripts/prover-service/bootstrap_package.py \
     --keeper-config /trusted/bootstrap-keeper.json \
     --evidence /trusted/native-evidence.json --payload /trusted/lease/payload.json \
     --output /trusted/verified-bootstrap.json verify \
     --request /trusted/request.json --proof /trusted/returned-proof.json \
     --release /trusted/lease/release.json --fri-verifier /trusted/fri-verifier.json \
     --check-dir /trusted/fri-checks --lease-dir /trusted/lease
   ```

   This requires the retained native lease to show acceptance of the exact proof,
   payload and reviewed release. It checks the still-open frozen package before and after verification and
   returns the checks and a `package` object. Extract `package` into a private
   file and sign its `sequencer_request`. Use `service.py complete` with that
   package and signature, omitting the wrapper signature. Extract `proof_data`
   from the same package, and use the [durable relay](relay-README.md) to stage,
   sign, broadcast, confirm and export the node handoff. The relay independently
   rechecks gate and priority state; it is not replaced by these local checks.

After the membership transaction is finalized on zkSYS, use
[initial enrollment](bootstrap-enrollment.md). Accepted qualification duties,
the initial roster, its delivery, both proof-lane activations and ordinary service
still require their own operations; these steps are not automated by this helper.
