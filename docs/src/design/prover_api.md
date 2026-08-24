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

### Startup recovery phases

<!-- SYSCOIN: Split protected prover draining from public readiness while retaining one fail-closed
stage-owned liveness lease. -->
Production startup advances through one monotonic `Recovering -> Drainable -> Ready` phase source:

| Phase | Prover service | Public node readiness | Recovery invariant |
|---|---|---|---|
| `Recovering` | When the external service is enabled, its configured address is reserved without a TCP listen backlog. | Remains HTTP 503. | The durable SNARK journal is checked against canonical committed metadata, replayable wrappers pass the active on-chain verifier preflight, and replay ownership is reconstructed. |
| `Drainable` | If enabled, only the loopback-protected or authenticated prover listener opens. | Remains HTTP 503. | Journal replay and its supervised forwarding/reaping paths are live, so workers may safely drain bounded proving work while FRI recovery continues. |
| `Ready` | If enabled, the protected prover listener remains live. | May leave HTTP 503 after the independent database gate is also ready. | Every startup batch has been canonically classified with one explicit owner or recovery disposition; wrappers do not all need to be completed. |

<!-- SYSCOIN: Durable FRI storage is the restart overflow queue; the configured map span bounds
resident work rather than the amount of crash-recoverable history. -->
During `Drainable`, the startup loader keeps accepted FRI files on durable disk and admits only a
bounded, canonical-order window into the in-memory job map. The server feeds that work in order to
external Airbender workers or the configured in-process fake workers. As workers complete ranges
and free map capacity, the loader admits the next disk-backed work without expanding the RAM bound.
Reaching `Ready` therefore means the complete startup inventory is canonically classified and
exclusively owned by the journal, memory queue, durable pending set, or another explicit recovery
state; it does not mean that every recovered FRI has already become a completed SNARK wrapper.

Accepted real wrappers remain journaled until confirmation, and restart validation reconstructs the
committed V32 log/message/multichain execute root before reuse. The phase source remains a live lease
rather than a one-time flag: if the proving stage exits, the prover listener and node readiness task
fail critically instead of serving from a retained `Drainable` or `Ready` value.

<!-- SYSCOIN: Recovery rejects ambiguous ownership and unsupported proof boundaries instead of
silently discarding durable work or waiting forever. -->
A canonical metadata or ownership inconsistency, an encountered future non-V32 proving-version
boundary unsupported by this V32 recovery lane, or a real singleton that can never acquire a
compatible companion fails startup closed. A V32 tail singleton that can pair with the next
canonical batch remains explicitly pending. Operators must preserve the entire configured
`prover_api.proof_storage.path`, including its `snark_journal` subdirectory, across ordinary
restarts and must not switch between fake and real proving while retaining that recovery state.

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

<!-- SYSCOIN: Bound authenticated transport admission independently from cryptographic lanes. -->
The node buffers at most three authenticated FRI submission bodies and one authenticated SNARK
submission body concurrently, each at most 10 MiB and under a 120-second total body-read deadline.
The lanes are separate, so SNARK uploads cannot starve FRI verification admission or vice versa;
pick, status, and bounded debug-peek requests do not consume these body slots. Configure an nginx
front end with `proxy_request_buffering off` and `client_body_timeout 120s` so node authentication
and its total deadline cover the streamed public upload.

The production default `prover_api.snark_job_timeout` is two hours. This is a lease timeout, not an
aggregation delay: it gives a CPU combine/wrap job enough time to complete before the server makes
the same range available to another eligible SNARK worker. Operators should initially size this for
the cold path and reduce it only after measuring cold- and warm-path wrapping times on the deployed
host.

<!-- SYSCOIN: The assignment span is a live RAM operating bound, not a restart-backlog bound. -->
The production default `prover_api.max_assigned_batch_range` is 256. This bounds the difference
between the oldest and newest work resident or leased in the in-memory map; it does not need to
cover the complete durable FRI restart backlog. The default retains one active 100-FRI SNARK lease,
a complete next 100-FRI aggregate, and at least 56 batches of headroom while disk-backed recovery
waits for workers to free capacity. Deployments with more concurrent SNARK workers must size the
RAM bound for their active leases and operating headroom, not for every proof retained on disk.

<!-- SYSCOIN: Durable capacity is the multi-worker queue's bounded-recovery overflow invariant. -->
Generated production configs set `prover_api.proof_storage.batch_with_proof_capacity` to 8 GiB.
The API accepts proof request bodies up to 10 MiB, while the canonical on-disk JSON hex encoding
can approximately double the decoded proof bytes; the upstream 1 GiB development default cannot
reliably retain the expected production restart backlog. `PROVER_BATCH_WITH_PROOF_CAPACITY_BYTES`
may raise this independent disk bound, but GPU mode rejects values below 8 GiB. Operators must
provision at least that much free space and preserve the proof-storage tree as one recovery unit;
the 256-batch in-memory span does not cap how much valid backlog disk recovery may stream.

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
