# Syscoin v32 consensus, finality, and recovery audit

Date: 2026-09-21. Reviewed local zksync-os-server HEAD `87c25cd0` (`fix(raft): gate proposals on confirmed replay frontier`), clean working tree at audit start. The original audit was read-only; the local watcher remediation is recorded separately below. The live deployment is intentionally v31 pending prover validation and a fresh v32 testnet reset. Findings below are v32 release gates, not claims that existing v31 testnet was intended as production.

## 1. Settlement availability is not covered by Raft failover

Classification: documented architectural limit; production availability gate, not a discovered consensus safety defect.

- `docs/src/setup/multi_node_consensus.md:23-27` explicitly says proof generation and L1 submission are not highly available. Exactly one consensus node must enable the batcher, and settlement pauses when it is unavailable.
- `lib/raft/src/model.rs:45-56` provides quorum-backed block proposal/canonization; it does not make proof storage, operator nonce ownership, or batch settlement highly available.
- `node/bin/src/lib.rs:2682` branches on local `batcher_config.enabled`; there is no automatic batcher leadership handoff in the reviewed component graph.

Release evidence needed: host-loss drill with measured block-production and settlement recovery times; durable batch/proof/replay storage recovery; operator-key and nonce fencing; a tested procedure for promoting the sole batcher role. If single-batcher operation is accepted, explicitly define settlement RTO and alert on lag.

## 2. Original finding: consumed L1/Gateway reorgs were not detected

Classification: concrete missing detection mechanism, conditional on violating an explicit finality/trusted-head assumption. No demonstrated funds-loss exploit or active fork was observed.

- `lib/l1_watcher/src/watcher.rs:261-265` chooses `latest - confirmations` for confirmed watchers.
- `lib/l1_watcher/src/watcher.rs:300-321` processes log ranges, then sets only `next_block = to_block + 1`. It retains no consumed block hash, detects no replaced ancestor, and never rewinds the cursor.
- `node/bin/src/config/mod.rs:1687-1689` defaults to two confirmations.
- `lib/l1_watcher/src/tx_watcher.rs:66` uses this confirmed watcher for priority deposits.
- `lib/l1_watcher/src/interop_watcher.rs:169-177` uses this confirmed watcher for Gateway roots. Its log metadata canonicalization rejects internally conflicting logs, but does not prove they remain on the later canonical chain.
- `lib/provider/src/logs_cache.rs:307-317` repairs the local log cache on reorg; this does not notify consumers about already consumed events or reset their cursors.
- `lib/l1_watcher/src/revert_watcher.rs:8-9` watches a contract `BlocksRevert` event, not consensus-chain reorganization. It is instantiated only for external nodes (`node/bin/src/lib.rs:1346-1362`).

Precise focused regression target: a `#[tokio::test]` within `lib/l1_watcher/src/watcher.rs`, with mock `NodeProvider` and recording `ProcessRawEvents`. Return a deposit/root in block H, call private `poll(H)`, replace canonical H with H-prime at the same height, and call `poll(H)` then `poll(H+1)`. Current logic does not request H again and will neither reject the orphaned consumed event nor import the replacement. The desired assertion is a reorg error/critical stop once a consumed boundary changes. Additional cases: growing head with fork below H; shallow fork entirely above H should remain allowed; RPC failure must not advance checkpoint.

Hardening options: select actual root-L1 finalized/ChainLock-derived boundaries for irreversible deposits, and finalized Gateway execution or an explicitly trusted quorum-canonized Gateway head for imported roots. Retain/check consumed `(height, hash)` checkpoints; anchor fetched ranges to a stable canonical snapshot; fail closed on a replaced consumed prefix. Already executed imported roots cannot safely be undone by simply rescanning. `optimistic_gateway_head=false` restores only confirmation lag, not finality.

### Follow-up: exact geth PR 42 finality integration

Reviewed the remote head of [syscoin/go-ethereum PR 42](https://github.com/syscoin/go-ethereum/pull/42), commit `39671819dabf551a1fe13b40ea47c9af59ee0a94`, via read-only GitHub API downloads. The local geth checkout and local `codex/pq-nevm-pairing` reference were stale relative to this PR; their earlier arithmetic finality behavior is not evidence about this revision.

- [`core/syscoin_finality.go:13-41`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/core/syscoin_finality.go#L13) projects Core's published finality onto an already executed canonical NEVM/Syscoin pair. It rejects an unexecuted height, mismatched Syscoin pairing, and stale/conflicting updates. Certificate verification and durable acceptance belong to Core; this geth component checks the executed pairing.
- [`core/blockchain_reader.go:81-100`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/core/blockchain_reader.go#L81) returns that projection for both `safe` and `finalized`. Further execution alone does not advance either selector.
- [`docs/syscoin-finality-rpc.md:3-24`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/docs/syscoin-finality-rpc.md#L3) specifies the accepted ChainLock boundary, Core's maintenance replay after restart, and unavailable-tag errors until a valid boundary arrives. The projection is in memory; rollback/recovery removing the projected block clears it.
- [`eth/nevm_finality_test.go:134-229`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/eth/nevm_finality_test.go#L134) exercises future/buffered/mismatched pair rejection, stalled Core finality, generic engine marker isolation, rollback clearing, and restart replay. These test sources were inspected; this audit did not execute the remote geth tests or independently verify the corresponding Core implementation/deployment.

This addresses the geth source of finality for consumers that request these selectors. Server finalized execution already uses `StartResolver::new_finalized` (`lib/l1_watcher/src/execute_watcher.rs:147`), and open-segment batch persistence uses `L1Watcher::new_finalized` (`lib/l1_watcher/src/sl_aware_watcher.rs:155-165`). The provider preserves unavailable-boundary semantics and waits for a header (`lib/provider/src/lib.rs:380-395,423-467`). With a deployed matching Core/geth pair, these paths can consume the authoritative ChainLock projection.

The remaining finding is narrower: priority inputs (`lib/l1_watcher/src/tx_watcher.rs:66`), ordinary commit/execution progress (`lib/l1_watcher/src/commit_watcher.rs:91`, `lib/l1_watcher/src/execute_watcher.rs:90`), and Gateway interop roots (`lib/l1_watcher/src/interop_watcher.rs:169-177`) still select the confirmed/latest boundary. Availability of `safe`/`finalized` alone does not change those constructors or supply missing consumed-prefix reorg detection. A release decision must route irreversible inputs through an appropriate finality boundary or explicitly retain and test the current confirmation/trusted-head assumptions. The Gateway's own irreversible-head trust is a separate boundary from the root NEVM RPC finality selector.

The membership keeper issue (C3 in the main report) is also separate. `contracts/src/zksys/ZkSysRegistryBridge.sol:99-149` samples and relays NEVM state only when `pushSentryNodeUpdates` is called. Delivered updates replace membership (`ZkSysMembershipRegistry.sol:138-153`) and change reward weight (`ZkSysRewardWeightRegistry.sol:194-204`); finality RPC tags do not call those methods. A keeper still needs to discover and reconcile membership removals, address/seniority changes, relay them, and monitor delivery lag. Reliable finality provides an observation boundary for that keeper; it does not ensure membership freshness.

### Local follow-up: confirmation policy retained, consumed history authenticated

The local generic watcher now retains an in-memory `(height, hash)` anchor before publishing a range's first event. It checks that anchor on later polls even when the reported head does not advance, authenticates fresh log responses against canonical block hashes, rejects missing/removed/out-of-range metadata, and stops on a replaced or unavailable consumed block. The log request bypasses the numeric-range cache so an earlier fork's cached response cannot be silently consumed. Block authentication uses standard `eth_getBlockByNumber`, including providers without the optional header RPC.

A partially processed range retains its anchor through transport retries and a temporarily regressed boundary. Once all processors succeed, its cursor advances before the post-processing RPC check, so a transient failure of that check cannot replay successfully delivered events. The retained anchor is still checked before any subsequent range is processed. Partial processor failures retain the existing at-least-once retry behavior; this change does not make every processor transactional or idempotent.

**Deposit confirmation policy is unchanged.** At reviewed upstream ZKsync OS commit [`e29a2c0a584b7f3f985e9a414bfbe179456c093d`](https://github.com/matter-labs/zksync-os-server/tree/e29a2c0a584b7f3f985e9a414bfbe179456c093d), the [priority transaction watcher](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/lib/l1_watcher/src/tx_watcher.rs#L54) uses the confirmed resolver. Its [scan cap](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/lib/l1_watcher/src/watcher.rs#L181) is `latest - confirmations`, with a [default of two](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/node/bin/src/config/mod.rs#L1385). This is source-default evidence, not verification of Ethereum production operator configuration. Mandatory finalized priority ingestion was considered but is not included: it is a stricter latency/safety policy requiring an explicit decision. Existing finalized settlement watchers remain finalized.

Canonical root-L1 bridge deposits share that priority boundary. Requiring finality would replace their configured confirmation wait, rather than add a fixed five blocks after it. PR 42 exposes Core's actual published boundary, not a universal five-block delay. Fast liquidity delivery and canonical settlement have distinct timing.

**Detection is not rollback.** There is no automatic coordinator that undoes already executed L2 deposits when their source L1 history changes. Restart replays the existing WAL unless explicit rebuild options are supplied (`node/bin/src/command_source.rs`); a fresh process does not retain these new watcher anchors. The external-node `BlocksRevert` watcher observes a settlement-contract event, not arbitrary source-chain reorganization. Manual revert/rebuild paths have settlement and imported-root constraints. A processor already executing a side effect can finish before a subsequent hash check detects a reorg. Restart validation, coordinated recovery, deployment of the matching Core/geth finality pair and the accepted Gateway trust model therefore remain release gates.

## 3. Generated optimistic Gateway mode carries operator trust

Classification: documented, intentional trust mode requiring an explicit release decision.

- `scripts/gateway-launch/generate-os-server-configs.sh:1125` emits `optimistic_gateway_head: true` for edge-chain configs.
- `node/bin/src/lib.rs:1282-1289` sets active settlement watcher confirmations to zero and warns that imported roots are irreversible and Gateway RPC must be quorum-canonized.
- `docs/src/guides/gateway_launch.md:258-265` documents this trust boundary.

Release evidence needed: Gateway voter topology and failure domains; configured trusted RPC; failover/partition tests; explicit fencing for rebuild/admin procedures that can rewrite already imported Gateway history. Raft provides crash-fault consensus, not permission to roll back irreversible imported roots.

## 4. Raft recovery has no bounded long-term history mechanism

Classification: intentional storage/recovery design limit; capacity and restart-time release gate.

- `lib/raft/src/init.rs:36` selects `SnapshotPolicy::Never`.
- `lib/raft/src/state_machine.rs:166-205` rejects snapshot receiving/install/build.
- `lib/raft/src/storage.rs:583-588` reports purging disabled.
- `lib/raft/src/storage.rs:400-433` documents and implements startup scanning with O(journal entries + touched heights) time and O(touched heights) memory, retaining a HashMap of identities.
- `lib/raft/src/init.rs:54` and `lib/raft/src/state_machine.rs:29-31` use an unbounded channel for replay forwarding before startup consumers run.

Release evidence needed: mature-chain log-size and restart-memory benchmarks, disk-growth alerts, and full-history replica/recovery drill. Long history or a large crash backlog must not exceed memory or acceptable restart time.

## Positive safety evidence

- `lib/raft/src/init.rs:62-64` binds Raft routing authorization to configured cluster peers; `lib/network/src/raft/protocol.rs:58-60,86-106` checks authenticated PeerIds against the allowlist.
- `lib/raft/src/leadership_monitor.rs:102-138` confirms quorum, checks the same vote, waits for retained log application, and captures a forwarded-replay watermark. `node/bin/src/command_source.rs:177-182` requires that watermark before proposals.
- `lib/raft/src/state_machine.rs:74-85,108-134` authenticates durable replay journal state rather than trusting block height or queue length.
- `lib/raft/src/storage.rs:255-270,400-484` rejects legacy height-only metadata and WAL state that matches no authenticated journal prefix.
- `lib/provider/src/lib.rs:382-400` refuses finalized watchers when finalized tags are unsupported, rather than substituting latest.
- `node/bin/src/batcher/bitcoin_da_finality_gate.rs:86-165` validates publication identity and applies the configured DA finality policy. Recovery republishing authenticates blob bytes before wallet publication. This DA gate is separate from L1/Gateway event ingestion finality.

## Validation

Executed existing cached binary `target/debug/deps/zksync_os_raft-061c261105909cd5 --test-threads=2`: 23 passed, 0 failed, 8.77 seconds. Binary modification time was 2026-09-05 11:30:36. Coverage includes changed/unflushed votes, retained replay barriers, crash before/after WAL writes, repeated same-height rebuilds, partial rebuild identity, malformed journal schema, missing immutable WAL identity, and rejected metadata mutation. This is cached-binary evidence, not a fresh source build or evidence of deployed revision.

Fresh focused source test completed successfully with the required wrapper:

```sh
scripts/cargo-with-patched-zksync-os.sh audit-consensus-v32 -- test --locked -p zksync_os_raft -p zksync_os_l1_watcher
```

The wrapper materialized upstream zksync-os revision `69bc430549e88f9264066d14f2001707572c5d33`, applied the checked-in Syscoin patch, and built a disposable rewritten server workspace under `target/syscoin-zksync-os-server-build/.gateway-launch/zksync-os-server/audit-consensus-v32`. The patched local guest git revision reported by Cargo was `8d473e35`.

Results: build/test profile finished in 3m 30s; `zksync_os_l1_watcher` passed 27/27 unit tests; `zksync_os_raft` passed 23/23 unit tests; both doc-test suites contained zero tests and passed. Command exited 0. Total fresh unit tests: **50 passed, zero failed, zero ignored**. Existing watcher tests cover static resolver identity, metadata canonicalization, historical commit/revert decisions, and helper invariants; they do not exercise a processed-chain reorg through the running generic watcher.

The preceding validation is the original audit baseline, before the local watcher hardening described above.

The remediation was freshly compiled and tested with:

```sh
scripts/cargo-with-patched-zksync-os.sh audit-finality-v32 -- test --locked -p zksync_os_l1_watcher --lib
```

Result: **38/38 tests passed**, including eleven new generic-watcher tests covering consumed-history replacement with unchanged/growing/regressed scan boundaries, event metadata and canonical block validation, partial processing, transport retry without duplicate successful delivery, and providers without the optional header RPC. The existing priority-watcher constructor remains unchanged. These mocked-provider tests do not replace a live coordinated reorg/restart rehearsal or full workspace integration coverage.

Focused Clippy also passed with `scripts/cargo-with-patched-zksync-os.sh audit-finality-v32 -- clippy --locked -p zksync_os_l1_watcher --all-targets -- -D warnings`. `cargo fmt --all -- --check` and `git diff --check` passed. The full workspace lint and integration suites were not rerun for this follow-up.
