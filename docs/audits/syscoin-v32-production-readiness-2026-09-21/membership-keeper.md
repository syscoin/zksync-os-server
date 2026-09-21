# C3: zkSYS membership keeper design and remaining release gates

Date: 2026-09-21. Status: design for review; no keeper implementation or deployment in this task. No transactions or live writes were performed. This supplements the membership finding in [proving-and-zksys.md](proving-and-zksys.md) and the exact PR42 finality review in [consensus.md](consensus.md#follow-up-exact-geth-pr-42-finality-integration).

The contracts support a permissionless keeper without granting it an administrator role. Production still needs a funded, monitored service that discovers changes, retains removed addresses, submits updates, and confirms their final application. This is an operational requirement of the current last-relayed-membership design; finality selectors alone do not update membership or expire stale reward weights.

## Source identity and corrected finality assessment

The local geth checkout (`2ce420e7892f7400296ad209dbe8efe3536e3f79`) is older than the reviewed [PR42 head `39671819dabf551a1fe13b40ea47c9af59ee0a94`](https://github.com/syscoin/go-ethereum/pull/42). Earlier statements based only on the local generic engine finality pointers do not apply to that PR head.

- At the exact PR head, [`core/syscoin_finality.go:13-41`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/core/syscoin_finality.go#L13) projects Core's accepted ChainLock onto an already executed canonical NEVM/Syscoin pair, checking the paired Syscoin hash and rejecting future, conflicting, or regressing updates.
- [`core/blockchain_reader.go:81-96`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/core/blockchain_reader.go#L81) uses this Core-backed projection for both NEVM `safe` and `finalized`, independently of generic engine markers.
- [`docs/syscoin-finality-rpc.md:3-24`](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/docs/syscoin-finality-rpc.md#L3) specifies Core's maintenance replay after restart, unavailable-tag errors until a valid projection arrives, and a stationary boundary when ChainLocks stop. The keeper must wait and alert on unavailable/stalled finality, never substitute `latest` or a confirmation count.
- Mandatory finalized root-L1 priority ingestion was considered but **is not included in the local changes**. The reviewed upstream main revision `e29a2c0a584b7f3f985e9a414bfbe179456c093d` uses `StartResolver::new`, a `latest - confirmations` boundary, and a default of two confirmations. The keeper's own finality requirements below are separate from that server policy decision. Verify the chosen ingestion boundary and matching Core/geth deployment before release; the keeper cannot enforce the server's boundary by itself, and anybody can call the permissionless bridge.
- zkSYS L2 has different selector meanings: `safe` resolves to the last committed block, while `finalized` resolves to the last finalized executed block: [`lib/rpc/src/rpc_storage.rs:45-54`](../../../lib/rpc/src/rpc_storage.rs). Require the latter for completed delivery.

These are source-level conclusions. The exact remote geth finality tests were inspected in the consensus audit, not run in this subtask; neither matching deployed binaries nor Core's accepted certificate were independently verified here.

## Membership reads are not historical precompile snapshots

The exact PR42 backend still implements [`GetNEVMAddress(ctx, address)` without a block argument](https://github.com/syscoin/go-ethereum/blob/39671819dabf551a1fe13b40ea47c9af59ee0a94/eth/api_backend.go#L95). It reads current canonical auxiliary metadata. The precompile accepts only a 20-byte address. Consequently, calling `nevmCollateralHeight` with an old numeric or `finalized` `eth_call` block tag does not establish membership at that historical block. In particular, `nevmSentryNodeWeight` can combine the selected execution block's number with a current collateral lookup; do not record that as an authenticated historical weight.

[`ZkSysRegistryBridge.sol:127-149`](../../../contracts/src/zksys/ZkSysRegistryBridge.sol) reads the precompile during transaction execution and constructs the L2 update itself, using that execution's `block.number` for seniority. The keeper supplies addresses, not membership claims. Treat its preflight lookup as a current-state hint, and the finalized originating transaction's actual priority-operation payload as the facts sent. An as-of-finalized-height membership API would require separate protocol support; this keeper does not invent one.

## Complete address discovery

Use Core's deterministic masternode RPC, not portal balances or only the current L2 list:

1. Read a reviewed Core/NEVM paired boundary. Use the NEVM finalized header and its actual Syscoin pairing, and verify the Syscoin block hash/height with Core. Do not infer the pairing solely from an unreviewed offset. If a paired-hash RPC is used, restrict it to the private endpoint and validate its response encoding. The `0x61` paired-hash precompile only accepts an 8-byte big-endian height and restricts access to older blocks in a 50,000-block window; it cannot query its own execution height (`go-ethereum/core/vm/contracts.go:1235-1250`).
2. Call `protx_list ["registered", true, syscoinHeight]`. Bracket the call with `getblockhash(syscoinHeight)` and reject a changed hash. `src/rpc/rpcevo.cpp:78-92` uses the deterministic list at that explicit height. Parse only the historical deterministic fields needed here: `proTxHash`, `state.nevmAddress`, and `state.collateralHeight` (`src/evo/deterministicmns.cpp:129-153`, `src/evo/dmnstate.cpp:33-57`). The RPC's auxiliary collateral-address/confirmation fields use current UTXO state and must not be mistaken for historical eligibility.
3. Union those addresses with a durable ever-seen address set, addresses in unresolved submissions, and the entire L2 active set. Read `activeSentryNodeCount` and every `activeSentryNodeAt(i)` at one pinned L2 block; the array uses swap-and-pop on removal, so mixed-block pagination can skip a member (`ZkSysMembershipRegistry.sol:101-110,163-187`). Use deployment-to-checkpoint membership logs to recover additional history where available; retain deployment block/hash and log cursors in state.
4. Preserve each old address when its `proTxHash` disappears, clears its address, or changes address. Submit old and new addresses in the same batch where practical. An old address that is now absent must still reach `pushSentryNodeUpdates`, so the bridge can send zero height/weight. Do not derive the work queue only from the new list. Core operator reset and PoSe ban clear the address (`src/evo/dmnstate.h:97-110`); deterministic diffs explicitly carry old and new addresses (`src/evo/deterministicmns.cpp:445-478`).
5. Persist the ever-seen union before considering discovery complete. Do not delete history merely because an address disappeared or a zero-weight update was sent. A restart or partial outage must not erase pending removals. An optional latest-Core scan may add addresses as discovery hints; it must not turn unfinalized facts into claimed final snapshots.

Core references above were inspected in adjacent local Syscoin revision `dd1457d89a5906288d0c0813a157413e79f75f6d`. Verify the same RPC schema and mapping against the intended Core release. No existing membership keeper or call site for `pushSentryNodeUpdates` was found in the searched server scripts, Syscoin, geth, or dapp-portal sources. This does not rule out an independently deployed service elsewhere.

## Reconciliation and priority

Deduplicate addresses, reject malformed/zero addresses, and compare the current hints with pinned L2 membership. Prioritize removals and reductions, then address replacements, additions, and seniority increases. Periodically reconcile every known/current address even if the deterministic list has not changed: seniority changes with block height (`ZkSysRegistryBridge.sol:173-195`). Scan the full active set on startup and periodically thereafter to recover updates submitted by other callers.

The contract limit is 512 addresses, with quadratic duplicate detection (`ZkSysRegistryBridge.sol:39,105-122`). Use smaller measured batches rather than assuming the maximum fits. Keep address-replacement pairs together when possible. A failed account callback can revert a whole L2 batch; a bounded split/retry procedure should isolate it and alert while unrelated work continues. Resubmission calls the bridge again with addresses and therefore resamples facts; it must not directly impersonate the L1 alias on L2.

Do not make discovery or urgent-removal processing wait for every older operation to reach L2 finality. Retain a bounded outbox of operations in flight, preserve sender nonce order, and track per-account supersession. A later valid update may supersede the observable state of an earlier one; do not label that earlier successful transaction as a failure just because the current member tuple has changed again.

## Durable outbox and restart behavior

A minimal implementation fits `scripts/zksys-membership-keeper.py`, following the repository's Python/`cast` operational tooling. This is a proposed location only; no file is implemented in this task. Default to a read-only reconciliation report; require explicit send/watch configuration for operation. Use a dedicated service account, one active instance per signer, and a lock covering nonce allocation and checkpoint writes.

Persist chain identities and deployment hashes, discovery checkpoints, the ever-seen/proTxHash address history, pending batches, sender/nonces, signed transaction identity, submitted L1 hashes, receipt identities, canonical L2 hashes, and separate observed/final delivery state. Atomic durable writes can follow [`scripts/gateway-launch/_checkpoint_state_io.py`](../../../scripts/gateway-launch/_checkpoint_state_io.py). Keep secrets out of checkpoints and logs. If persisting signed transactions for unambiguous rebroadcast, protect the files because they contain spend authorizations.

Use these transitions:

| State | Required evidence before advancing |
| --- | --- |
| Prepared | Addresses and limits durably recorded; correct network, contract wiring, signer, and funding checked. |
| Signed / submission uncertain | Transaction hash and nonce retained before broadcast; an RPC timeout does not authorize allocating another nonce or marking the batch absent. |
| L1 included | Receipt status successful, expected bridge and event found, receipt block/hash still canonical. Record `RegistryUpdatesRequested.canonicalTxHash`; retain the originating priority-operation payload. |
| L1 final | Receipt block is within the Core-backed NEVM finalized prefix and its hash is unchanged. Finality unavailable/stalled remains pending and alerts. |
| L2 applied | `eth_getTransactionReceipt(canonicalTxHash)` succeeds on the correct L2; inclusion hash is canonical and registry effects agree with the actual priority payload. |
| L2 final | L2 receipt block is within `finalized`, canonical inclusion still matches, and state/event reconciliation succeeds. Only then mark delivery final. |

On restart, validate the configured network/deployment identity and replay unresolved outbox entries before sending new work. Reconcile receipt/hash/nonce state after an ambiguous broadcast; retry the same known transaction or use an explicit same-nonce replacement policy. A replaced consumed checkpoint is a critical stop requiring reconciliation, not an automatic cursor advance. Resample and retry failed L2 operations with adequate gas; a successful L1 enqueue is not successful L2 delivery.

`RegistryUpdatesRequested` contains only the canonical hash and update count (`ZkSysRegistryBridge.sol:62,152`). Obtain exact sent tuples from the matching authenticated `NewPriorityRequest` transaction data, rather than from a later historical precompile call. The server exposes the event shape in [`lib/contract_interface/src/lib.rs:131`](../../../lib/contract_interface/src/lib.rs). Membership emits change events only when fields change, so unchanged/idempotent deliveries may have no membership change event. A successful receipt plus exact payload and appropriate pinned state reconciliation must handle that case.

## Funding and gas

Use a dedicated, budgeted NEVM SYS-funded EOA or supported external signer; it needs no membership/admin role. Validate L1/L2 chain IDs, bridge code/proxy implementation, `bridgehub`, `zksysChainId`, `l2Registry`, the L2 registry's `l1RegistryBridge`/alias, receiver wiring, and native base-token configuration before sending. Production governance keys should not be the keeper's routine spending key.

Pay both L1 gas and the Bridgehub L2 base cost. Quote `l2TransactionBaseCost(chainId, gasPrice, l2GasLimit, gasPerPubdata)` with a bounded transaction gas-price policy, attach the required native value, and re-estimate before submission/replacement. The bridge forwards the entire `msg.value` as `mintValue`, uses zero L2 call value, and defaults refund recipient to its caller (`ZkSysRegistryBridge.sol:138-148`). Set an explicit reviewed refund address, track the L2 refunds, and do not assume refunds refill the L1 spending balance.

The portal's 800 gas-per-pubdata and 2.5 million L2 gas values are deposit defaults (`dapp-portal/utils/syscoinBridge.ts:20-21`), not evidence of keeper gas sufficiency. Measure worst-case new-member storage, removals, replacements, downstream issuer checkpointing, and mixed batches using real priority operations on the disposable candidate. L1 `eth_estimateGas` only estimates the enqueue transaction; it does not prove the L2 callback succeeds. Bound per-transaction fees, daily spend, outstanding nonces, and batch size; alert on runway before funds are exhausted.

## Seniority height mapping: unresolved acceptance evidence

Do not change the bridge formula or deployment defaults based solely on this inspection.

- The deployment helper defaults `ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK` to `1,317,500`, and both seniority bonus BPS to zero (`scripts/gateway-launch/gateway-deploy-l1.sh:738-743`). Core local mainnet activation is `1,317,500`; testnet is `840,000` (`syscoin/src/kernel/chainparams.cpp:208,376`). A Tanenbaum launch must explicitly verify the intended configured value rather than inherit an unexamined mainnet default.
- Core reports `nevmHeight = syscoinHeight - activationHeight + 1` (`src/services/rpc/nevmrpc.cpp:85`). Its startup code repeats this relation and converts a nonzero geth height back as `syscoinHeight = nevmHeight - 1 + activationHeight` (`src/init.cpp:2005,2015-2016`). This is stronger evidence for the offset convention than the RPC display alone.
- The bridge calculates `syscoinHeight = nevmStartBlock + currentHeight` (`ZkSysRegistryBridge.sol:186-188`). This would match the Core relation if the bridge parameter represents `activationHeight - 1`. Passing the activation height itself suggests a one-block boundary difference, but this subtask did not run an end-to-end paired genesis/activation mapping or inspect deployed initialization values. PR42's finality test uses synthetic paired hashes, so it does not settle this real-chain offset.
- Required decisive evidence: pair NEVM genesis/first execution and a later block to actual Core hashes/heights on the intended release/network; record the deployed bridge parameter; check seniority at threshold minus one, threshold, and threshold plus one against the intended economic convention. Include mainnet and Tanenbaum defaults/overrides. With zero bonus BPS, this uncertainty does not change the bridge's base weight; enabled bonuses make it material.

Classification: unresolved configuration and boundary-validation question, not a demonstrated deployed reward defect. Any eventual change affecting bound contract identity must follow the existing app/key/fixture release process.

## Required inputs, acceptance, and residual economics

Before implementation/deployment, review and pin:

1. Intended Core/geth/server releases, authenticated RPC endpoints, complete contract/deployment identities, and access to paired Syscoin/NEVM hashes. Demonstrate matching Core-backed finality and its restart replay on the candidate.
2. The economic freshness policy and maximum removal/address-change delivery lag, including what happens when ChainLocks, Gateway settlement, or L2 proving stop. Decide whether last-relayed eligibility is acceptable; the keeper cannot guarantee eligibility changes exactly at their original L1 time.
3. Signer and refund identities, funding operator/runway, transaction and daily budgets, measured L2 batch gas, and retry/replacement limits.
4. Persisted state location, backup/restore and single-writer ownership, deployment starting blocks, monitoring ownership, and escalation thresholds. Resolve the seniority mapping evidence before enabling bonuses.

Acceptance must cover initial sync; removal; PoSe ban; address replacement; seniority crossing; other permissionless callers; duplicate/no-op updates; restart after signing and ambiguous broadcast; replaced L1 receipt; unavailable/stalled and replayed finality; L2 revert/out-of-gas; successful enqueue with delayed delivery; a later update superseding an earlier one; low funds; and complete finalized delivery. Test the maximum supported workload and missed-update recovery in a disposable environment, not by mutating the running v31 network.

The economic consequence remains: membership has no expiry or observation timestamp (`ZkSysMembershipRegistry.sol:20-29,138-153`); reward weight persists until an update arrives (`ZkSysRewardWeightRegistry.sol:194-204`); a late removal settles the old weight before replacing it (`ZkSysIssuer.sol:224-235`). Already accrued rewards are not clawed back. Keeper delay can reward a removed member and dilute others without bypassing the global issuance cap. Monitoring should therefore expose oldest discovered but unfinalized removal, source scan age, pending L1/L2 age, finality lag, reconciliation mismatches, and spending runway, not just process uptime.

Validation in this follow-up: source and exact PR42 cached-file inspection only. No new executable tests, live keeper probe, or gas benchmark was performed. The earlier 118 Solidity tests remain useful contract coverage but use mocked bridge delivery and do not satisfy this keeper acceptance matrix.
