# Authenticated dispatch audit

The wrapper checks the complete period journal before endorsing a service package.
The journal exposes signed enrollment, signed operator readiness, assignments,
sequencer offers, retries, and signed accepted duty receipts. It contains no native
lease token, Basic authentication, endpoint URL, FRI input, or proof bytes.

`audit.verify_package(settings, evidence, manifest, subscriptions, duties, bundle,
trust, rpc, fri_payload=payload)` independently enumerates the eligible registry accounts at the pinned
canonical enrollment block, authenticates every exact accepted subscription and
verifies every fresh event signature, then
replays account and lane selection. Account selection follows the existing sorted
circular cursor, skips absent or expired readiness, and blocks an account with a
live job. Both child and gateway work share the same account quota. Only a signed
offer consumes a quota opportunity; empty picks and unsigned expired reservations
consume none. A rejected signed attempt consumes its opportunity, and a retry uses
a fresh native lease commitment and increments the attempt. An ambiguous submission
cannot be expired or reassigned.

Historical account consent comes from the registry's accepted subscription and
exact account/operator period mappings. It supports ERC-1271 accounts without
rechecking wallet approval after enrollment. The original account-signature JSON
bytes and commitments are retained. Readiness, duty, sequencer and wrapper
signatures remain EOA-authenticated; fresh contract-operator signatures require a
separately implemented adapter. The authority is bound to the exact snapshot, registry identity, period
and independently pinned canonical block, and cannot be supplied as an untrusted
JSON assertion.

For the proposed lane and contiguous batch range, the manifest must contain every
assignment and retry from the journal, and the duty report must contain exactly
every accepted receipt. Pending assignments in that range make the wrapper wait.
Pending or accepted work outside that range does not enter its report. Ordinary
nonempty service requires an accepted journal duty for every nonempty batch while
any enrolled account still has unconsumed opportunities. Including one favored
accepted duty cannot conceal other unassigned nonempty batches. Empty native ranges may advance with
zero duties, as may nonempty work after every enrolled account has consumed its
full signed-opportunity quota. This preserves quiet-chain and post-quota progress
without turning empty work into service credit. The summary's `zero_duty_reason`
identifies `empty_native_range`, `quota_exhausted`, or `control`; it is null when
accepted duties exist. `unrewarded_batches` and `unrewarded_reason` identify nonempty
batches permitted to advance without credit, including mixed post-quota ranges.
The caller may pass
`allow_control=True` only after authenticating an actual protocol control package;
this exempts the minimum duty count, not exact history or receipt coverage.

Native artifact verification remains a separate required package check: the duty's
statement, transaction count, and FRI proof hash must match the actual native proof
inputs. The required `fri_payload` argument contains the exact native SNARK input
(`from_batch_number`, `to_batch_number`, `vk_hash`, and ordered `fri_proofs`). Its
hashes must match accepted receipts. If a signed submission was recorded as rejected
but its exact FRI proof appears in that input, the audit refuses both an uncredited
package and a newly credited replacement attempt; it never invents acceptance or
credit. A checkpoint authenticates the sequencer's assertion of native acceptance;
it does not replace cryptographic proof verification or canonical settlement
checks. Every subscription must enroll its operator in both services (`services=3`);
FRI and SNARK work use the same pool. The audit does not change rewards or quotas.

## Independent trust file

The wrapper supplies this configuration from its own retained observations. It must
not derive this trust file from the current untrusted bundle:

```json
{
  "schema_version": 1,
  "journal_id": "0x<32-byte journal identity>",
  "enrollment_block_hash": "0x<independently finalized enrollment block>",
  "minimum_checkpoint": {
    "event_count": 0,
    "event_head": "0x<journal identity for an empty prefix>",
    "assignment_cursor": "0x<journal identity for an empty prefix>"
  },
  "required_readiness": [],
  "required_duties": []
}
```

A later minimum checkpoint pins its event count, event head, and assignment cursor,
so a new bundle must extend that exact prefix. Each `required_readiness` entry is
`{"request": <ServiceWorkRequestV1 signing request>, "signature": "0x...",
"observed_before_event_count": N}`. The signed readiness must occur at or before
event N; appending it after favored work does not satisfy that observation. Each
`required_duties` entry is the full signed `DutySuccessV1` receipt, including
`operatorSignature`. Every independently known accepted receipt must occur in the
ledger even if it concerns another batch range.

A sequencer signature alone cannot prove that the sequencer disclosed every
operator's readiness or every native job, or that claimed empty, abandoned, and
timed events were truthful. The verifier detects scheduling bias against the
authenticated disclosed history and rejects omissions known through the wrapper's
independent pins. It cannot prove the absence of withheld off-ledger activity.
It also cannot prove that every discarded FRI was invalid: a sequencer might falsely
reject valid proof A and then use distinct valid proof B for the same statement.
Detecting that case requires independent verification of rejected proof bytes or
independently observed acceptance. Exact reuse of A is rejected by the native-input
hash cross-check, but the journal does not authenticate every rejection by itself.
Operators and wrappers must retain and compare observations to expose equivocation.

## Export and signing

New dispatcher journals commit to a public identity containing the complete signed
subscription snapshot, lane settings, endpoint commitments, and enrollment anchor.
Each durable transition appends one hash-chained event in the same atomic journal
write. `DispatchCheckpointV1` uses the `ZkSysServiceDispatcher` EIP-712 domain and
binds the journal, event count/head, assignment cursor, lane, range, period, and
issuance time. Existing journals without this history cannot export an audit
bundle or silently reconstruct a prefix; finish or retire them explicitly and
initialize a new period journal from independently verified enrollment.

```sh
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  audit-request --lane child --batch-from 1 --batch-to 2 --output /private/audit-request.json

# Sign the request.digest using the configured sequencer signer, then store
# {"signature":"0x..."} in /private/audit-signature.json.
python3 scripts/prover-service/dispatcher.py --execute --state /private/service-round-5 \
  export-audit --request /private/audit-request.json --signature /private/audit-signature.json \
  --output /private/audit.json
```

Export fails if the journal changed after the signing request. Programmatic callers
use `Dispatcher.audit_payload(lane, batch_from, batch_to)`,
`audit.checkpoint_request(bundle)`, and `Dispatcher.export_audit(...)` under the
dispatcher lock. `Dispatcher.range_manifest_payload(...)` builds the deterministic
complete manifest for the proposed range.
