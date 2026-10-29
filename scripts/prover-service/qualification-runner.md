# Initial prover qualification duties

`qualification_runner.py` drives native FRI duties for one already-enrolled
operator during the period-zero bootstrap window. Each `--execute` invocation
does one bounded step using the existing dispatcher and external-input compute
pool. The separate pool supervisor or pool operator performs the real prover
compute. The runner checks the returned pool receipt, signs the exact proof
with the enrolled operator, and submits it under the dispatcher's retained
native lease. It does not fabricate a success or count a rental as proof
acceptance.

This automates native FRI duty collection only. A native-accepted duty is not
yet on-chain qualification: the duty must be included in a real accepted
bootstrap SNARK package and its authenticated message delivered to zkSYS.
That aggregation, proof publication, message delivery, initial roster and
service activation remain separate. The runner reports both
`onchain_qualification_verified` and `service_ready_verified` as false.

Initialize an existing bootstrap dispatcher as described in the
[dispatcher guide](README.md#live-trusted-host-fri-dispatcher), using period 0,
the complete finalized eligible subscription snapshot and the intended native
lane endpoints. This runner deliberately requires that snapshot to contain
exactly one eligible account. The operator must have consented to enrollment;
the sequencer and operator wallet endpoints must support signing-only
`eth_signTypedData_v4` for their respective EOA identities. Use only one
dispatcher per sequencer/period, with its journal and original leases retained.

Configure and initialize the existing [prover-owned pool](../prover-rental/pool-README.md)
with matching lane identities, FRI external acquisition and the reviewed FRI
release. Its native lease duration must match the actual node setting; the
runner calculates the conservative deadline from the dispatcher's persisted
pick start, the pool's configured lease duration and signed readiness expiry.
Run its existing compute supervisor separately. Provide enough time for the
actual FRI job and network handoff within that deadline. If a pick or submission
is uncertain, preserve the journal and reconcile the same native capability;
the runner never creates a replacement for it automatically.

Create a private signature directory and a mode-0600 JSON configuration:

```json
{
  "schema_version": 1,
  "dispatcher_dir": "/trusted/bootstrap-dispatcher",
  "pool_dir": "/trusted/prover-pool",
  "signature_dir": "/trusted/qualification-signatures",
  "registry_rpc_file": "/trusted/registry-rpc.json",
  "sequencer_wallet_file": "/trusted/sequencer-wallet.json",
  "operator_wallet_file": "/trusted/operator-wallet.json",
  "child_auth_file": "/trusted/child-native-auth",
  "gateway_auth_file": null,
  "account": "0xREVIEWED_SENTRY_ACCOUNT",
  "readiness_seconds": 3600
}
```

The three connection files use the existing private `{ "url": ..., "authorization": ... }`
format. Set `gateway_auth_file` to a separate private native Basic Auth file if
the dispatcher has a Gateway lane. The values are examples; choose the actual
account and timing for the deployment. Never share the dispatcher journal,
native auth or lease capability with the rented prover.

```sh
python3 scripts/prover-service/qualification_runner.py --config /trusted/qualification.json
python3 scripts/prover-service/qualification_runner.py --config /trusted/qualification.json --execute
```

The first command is a dry run. Repeat the second while the existing prover
pool is running. `await_pool_result` means the pool has the immutable external
input, and `native_submission` with `accepted` means the native node accepted
that exact signed proof. At quota the runner stops with
`await_bootstrap_package_or_investigate_unfilled_quota`; check the dispatcher
report's credited accepted count before attempting qualification packaging.
Rejected or uncertain work, a changed pool binding, an expired compute job,
another eligible operator, or a closed bootstrap window stops for investigation.
No production prover, live node or Gateway was exercised by its unit tests.
