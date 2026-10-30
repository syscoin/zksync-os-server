# Post-deployment prover setup

`post_deployment.py` provides one entry point for the implemented bootstrap
proof, initial enrollment and initial roster publication steps. Each command is
one explicit step. It does not run a continuous coordinator, operate the
membership keeper, generate native proofs, rotate prover roles, deliver roster
messages or activate the service. Those remaining actions and the required
production prover must be completed separately before service can run.

Start after the token and service contracts have been deployed and configured.
Use an independently reviewed keeper configuration for the child bootstrap
lane, an initial enrollment configuration, and an initial roster publication
configuration. Their existing formats are documented in the [keeper guide](keeper-operator-guide.md),
[enrollment guide](bootstrap-enrollment.md), and [roster publication guide](README.md#initial-roster-publication).
The bootstrap keeper and enrollment settings must be identical, with a zero
coordinator; all three configs must identify the same zkSYS service registry,
chain, genesis and registry runtime code. If a configuration is for another
deployment, the entry point stops before processing a step.

Create existing, separate mode-0700 directories for the enrollment journal,
roster journal and FRI checks. Save a mode-0600 JSON file with absolute paths:

```json
{
  "schema_version": 1,
  "bootstrap_keeper_config": "/trusted/bootstrap-keeper.json",
  "enrollment_config": "/trusted/enrollment-config.json",
  "roster_config": "/trusted/roster-config.json",
  "enrollment_state_dir": "/trusted/enrollment-state",
  "roster_state_dir": "/trusted/roster-state",
  "fri_check_dir": "/trusted/fri-checks"
}
```

Replace every path and deployment placeholder with the reviewed deployment's
actual values. Keep the dedicated wallet nonces and the journals intact across
restarts. The enrollment and roster configurations may use different accounts;
do not race either journal with another sender using the same account.

Every output is a JSON envelope. The step's original output is in `result`;
`service_ready_verified` is always false and `not_orchestrated` names the
remaining proof generation, qualifying duties, role cutover, message delivery
and service activation. Extract the `result` object when the individual helper
guides refer to their output files.

The prover-role handoff is managed manually using the existing administrator,
as described in the [gate deployment guide](gate-deployment.md). Pause the initial
proof sender and reconcile its submitted transactions before deploying the gate
against the current chain state. No separate cutover contract is required.

For an initial uncredited proof when membership delivery needs it, follow the
[bootstrap proof guide](bootstrap-proof.md) for the real native leases,
production FRI/SNARK proofs, gate opening, priority witness and relay. The
entry point provides the artifact steps:

```sh
python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  manifest --previous-cursor 0xREVIEWED_32_BYTE_CURSOR --output /trusted/manifest-request.json

python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  proof-request --evidence /trusted/evidence.json --payload /trusted/lease/payload.json \
  --manifest /trusted/signed-manifest.json --beneficiary 0xREVIEWED_SEQUENCER_BENEFICIARY \
  --output /trusted/request-and-open-call.json

python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  proof-verify --evidence /trusted/evidence.json --payload /trusted/lease/payload.json \
  --request /trusted/request.json --proof /trusted/returned-proof.json \
  --release /trusted/lease/release.json --fri-verifier /trusted/fri-verifier.json \
  --lease-dir /trusted/lease --output /trusted/verified-bootstrap.json
```

The second command returns a simulated gate-opening call; it does not submit it.
The third requires the original accepted native lease and verifies both native
proof stages. It creates a package for sequencer signing and the existing durable
relay. These proof actions never broadcast a transaction. Extract the original
manifest request, gate request or verified package from the corresponding
`result` object before following the bootstrap proof guide.

After the membership update is finalized on zkSYS and the account and operator
have signed the subscription, preview and then explicitly submit enrollment:

```sh
python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  enroll --subscription /trusted/signed-subscription.json --operator-signature /trusted/operator.sig

python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  enroll --subscription /trusted/signed-subscription.json --operator-signature /trusted/operator.sig --execute
```

Repeat the second command until it reports `enrolled`. It resumes the same
transaction and only reports enrollment after finalized on-chain verification.
It does not claim the account remains eligible after a later removal.

After real accepted bootstrap duties qualify the initial prover and the roster
cutoff is reached, construct the authenticated initial roster as described in
the roster guide. Preview and explicitly submit one page at a time:

```sh
python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  publish-roster --roster /trusted/initial-roster.json

python3 scripts/prover-service/post_deployment.py --config /trusted/post-deployment.json \
  publish-roster --roster /trusted/initial-roster.json --execute
```

Repeat the second command until it reports `published`; each page is reconciled
through the existing durable journal. The entry point reports `published` only
after the same root is visible at a finalized zkSYS block. A
published roster does not mean both proof lanes are active. Finish the required
delivery, draw and activation steps, then start the normal prover coordinator
and wrapper after service activation.
