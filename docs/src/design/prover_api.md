# Prover API

```
        .route("/prover-jobs/v1/status", get(status))
        .route("/prover-jobs/v1/FRI/pick", post(pick_fri_job))
        .route("/prover-jobs/v1/FRI/submit", post(submit_fri_proof))
        .route("/prover-jobs/v1/SNARK/pick", post(pick_snark_job))
        .route("/prover-jobs/v1/SNARK/submit", post(submit_snark_proof))
```

## Real SNARK readiness

<!-- SYSCOIN: downstream multi-worker aggregation policy. -->

Before a main-node batcher starts either proof producer, it reads `getVerifier()` from the active
settlement-layer diamond at the sampled startup block. The selected wrapper must explicitly report
`IS_TESTNET_VERIFIER=true` for the in-process fake pools. An external real-prover API instead
requires `IS_TESTNET_VERIFIER=false`, a completed/nonzero canonical V8 key, and an on-chain
`verificationKeyHash()` equal to the server's compiled key. Mixed real/fake producers, missing or
malformed marker calls, and key mismatches fail startup. A batcher-disabled node, external node, or
main-node batcher intentionally configured with no proof producer does not perform this gate.

`/SNARK/pick` assigns a range only when it contains at least two consecutive compatible real FRI
proofs and either reaches `prover_api.target_fris_per_snark` or its oldest proof has waited
`prover_api.max_snark_batch_wait`. The range never exceeds `prover_api.max_fris_per_snark`
(hard-capped at 100), and never crosses a gap or proving-version boundary. Protocol-upgrade
metadata does not create an additional proof boundary; the committed upgrade hash is already
bound by the batch public input.

<!-- SYSCOIN: Separate resident stages avoid GPU cache teardown without weakening global leasing. -->
The canonical fleet runs three standalone GPU FRI workers continuously and a separate standalone
CPU SNARK worker for combining and wrapping. FRI workers only request FRI jobs, so their proving
state remains resident between jobs. The CPU worker calls `/SNARK/pick` only when idle. A
`204 No Content` response leaves it idle, while a returned range is already atomically leased to
that worker. The server therefore remains authoritative for readiness, range selection, and
reassignment across all workers and sequencers, without competing local timers or duplicate
speculative wrapping.

The production default `prover_api.snark_job_timeout` is two hours. This is a lease timeout, not an
aggregation delay: it gives a CPU combine/wrap job enough time to complete before the server makes
the same range available to another eligible SNARK worker. Operators should initially size this for
the cold path and reduce it only after measuring cold- and warm-path wrapping times on the deployed
host.

The production default `prover_api.max_assigned_batch_range` is 256. This retains one active
100-FRI SNARK lease, a complete next 100-FRI aggregate, and at least 56 batches of headroom, so
all three resident FRI workers can keep filling the queue while the CPU worker combines and wraps
the active range. Deployments that intentionally run more concurrent SNARK workers must size this
bound for every active lease plus the next aggregate.

<!-- SYSCOIN: durable capacity is part of the multi-worker queue's restart-safety invariant. -->
Generated production configs set `prover_api.proof_storage.batch_with_proof_capacity` to 8 GiB.
The API accepts proof request bodies up to 10 MiB, while the canonical on-disk JSON hex encoding
can approximately double the decoded proof bytes; the upstream 1 GiB development default cannot
reliably retain the full 256-batch window. `PROVER_BATCH_WITH_PROOF_CAPACITY_BYTES` may raise this
capacity, but GPU mode rejects values below 8 GiB. Operators must also provision that much free
disk space and increase it before increasing `max_assigned_batch_range`.

SYSCOIN's V32 / Execution V7 / Proving V8 interop priority lane bypasses only the target-or-age
delay when that same contiguous range contains an InteropCenter bundle and already has at least
two real FRI proofs.
The signal is reconstructed from canonical batch metadata: a service log on shard zero from the
L2-to-L1 messenger whose caller key is the V32 InteropCenter and whose hash matches a retained
message preimage beginning with the pinned interop bundle identifier. Those `logs` and `messages`
are part of the existing stored batch envelope, so the signal survives FRI storage and queue
rehydration without a new API or on-disk field. A direct messenger call cannot opt into the
priority lane merely by copying the bundle prefix.

### Interop companion batching

<!-- SYSCOIN: downstream low-traffic companion policy for stock Airbender min-two aggregation. -->

An authenticated bundle seals its source batch immediately. The next block is also sealed as a
one-block successor batch, producing the second distinct FRI needed by the stock minimum-two
SNARK rule. Real upgrade, system, L1, or L2 traffic wins during the configured
`sequencer.interop_companion_idle_delay` (250 ms by default). If the edge still settles on
Gateway when that grace period expires, the sequencer produces one real zero-transaction block;
the batcher isolates it as the successor FRI, and the interop-priority SNARK range can be released
as soon as both FRI proofs are accepted.

The companion state is reconstructed from canonical replay and committed-but-unexecuted batches,
so it does not require a second durable marker. A proving-version boundary expires the old tail:
protocol and security upgrades retain absolute priority and are never delayed to manufacture a
compatible companion.

Direct-L1 chains do not use the empty fallback. Era priority mode rejects a batch containing zero
L1 and zero L2 transactions, and activation can race local block production. A direct-L1 interop
tail therefore has no idle-time SLA: it waits for commit-valid real traffic (an L1 priority
transaction while priority mode is active) or for priority-mode deactivation. Operators must not
interpret the 250 ms Gateway grace period as a bound on FRI proving, SNARK wrapping, DA finality,
or settlement inclusion.

Fake SNARK proving retains its immediate behavior. Jobs held for aggregation remain visible
through `/status`; a real pick returns no job until both the two-proof minimum and the target,
age, or interop condition are met.
