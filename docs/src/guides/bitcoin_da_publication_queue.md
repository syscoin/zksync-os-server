# Bounded Bitcoin DA recovery publication

The default `batcher.bitcoin_da_publication_window: 1` retains the existing
publication and sequential recovery behavior. The opt-in coordinator supports a
window of two to eight outstanding publications, not a burst of 32 wallet calls.
Start with two on a reviewed testnet rollout.

## Shared-wallet rollout requirements

Upgrade **every** normal Edge publisher, normal Gateway publisher, and Gateway
recovery publisher using the Core wallet before enabling this mode. They must run
on the same host and use the same private physical directory on a local filesystem
with working OS file locks. A shared wallet across hosts, network filesystems,
unmodified publishers, manual wallet spends, or other uncoordinated writers is not
supported by this coordinator.

Quiesce all writers before first activation. Core must be out of initial block
download, with its saved mempool fully loaded and empty, before the coordinator
initializes. An empty mempool while Core is still loading is not sufficient.
Wait for existing publications to mine; do not delete Core mempool or wallet state.

All writers must use matching values for these settings:

```yaml
batcher:
  bitcoin_da_publication_window: 2
  bitcoin_da_publication_state_path: /absolute/private/shared-wallet-directory
  bitcoin_da_publication_mempool_count_limit: 20
  bitcoin_da_publication_mempool_vbytes_limit: 75000
  bitcoin_da_publication_vbytes_reserve: 25000
```

The directory must be private (0700 on Unix); files are private (0600). Symlink
aliases are rejected. The journal binds the chain genesis, sanitized RPC endpoint,
wallet name, window, and advisory budgets. A mismatch stops publication. Existing
RPC URL, credentials, wallet, and finality settings remain required; do not put
credentials in this journal or in a support report.

The count and vbyte values above are deliberately conservative **whole-mempool
testnet budgets**, not universal production defaults or discovered Core policy.
Unrelated transactions consume this headroom too. Operators must review them
against the actual node policy and workload. Automatic wallet input selection can
make a transaction larger than the configured vbyte estimate. These checks are
advisory backpressure; Core remains the final admission authority. This feature
does not raise ancestor/descendant limits, bypass policy, change confirmations, or
guarantee that a particular candidate will be admitted.

Increasing the window does not increase these budgets, the fee policy, finality
requirements, or the paid-republication attempt limit. A window of eight is an
upper bound on outstanding transactions shared by all cooperating writers, not a
guarantee that eight transactions will fit the available mempool headroom. The
fund-and-broadcast calls remain serialized.

## Changing an existing shared policy

Changing configuration alone does not migrate an existing coordinator. The
journal's exact policy match remains mandatory; writers configured for different
windows refuse to use the same journal. Older binaries that support only windows
up to four cannot run with a window of eight.

An existing-policy change requires a separately reviewed, coordinated operator
procedure: positively stop every writer behind effective restart interlocks,
hold the existing shared wallet lock, and authenticate the exact old and new
policies and canonical chain identity. A widening may change only the window,
preserving every pending row (including duplicate hashes or transaction IDs), the
genesis, mined anchor, and all recovery clocks and paid-attempt reservations. Do
not create a fresh coordinator or clear state. Unknown transaction outcomes must
be investigated, not migrated around. Keep durable before/after evidence and
once-only completion records; an interrupted or uncertain transition must remain
fenced for operator review. This feature does not provide an automatic migration
or rollback API.

## What is durable

The coordinator serializes the entire fund-and-broadcast RPC. It writes and fsyncs
an outstanding entry before calling the wallet, then durably records the returned
transaction ID. Normal publication has priority over recovery when waiting for a
free slot. Normal publication adopts a known journal receipt before checking new
capacity or headroom, including after a crash before per-batch progress was saved.
That receipt-only guard cannot broadcast another transaction. Normal duplicate
hashes are not resubmitted. If another writer has already retired the mined entry,
normal publication can recognize matching hash/transaction metadata directly from
Core without waiting for new capacity. A missing or failed Core lookup supplies no
receipt; archive availability alone cannot establish publication. DA finality is
still checked separately under the unchanged policy.

Once the existing recovery deadline and paid-attempt claim allow it, forced
recovery may submit another transaction for a still-unconfirmed or evicted hash.
It requires a free slot and headroom before consuming an attempt, retains every
original transaction ID, and counts each transaction against the same window.
Overwrite is not RBF or cancellation: a full window still blocks new wallet calls,
even for recovery, until exact canonical mining releases capacity. It does not
block adoption of a receipt already recorded in the journal.

A known transaction frees a funding slot only when Core's wallet receipt identifies
that exact transaction ID in a canonical mined block. This works even after Core
prunes the blob metadata. A prior transaction with the same blob hash cannot free
it. Receipt reads are bounded and raw transaction bodies are not logged or retained.
The journal retains the highest mined block anchor;
a later canonical-chain mismatch stops all writers for investigation. Mining can
free a funding slot before the DA finality policy is satisfied. The commit gate
still waits for **all** own and forwarded references in the current command to
satisfy the unchanged finality policy.

## Reusing a publication after metadata expiry

Core's expiring blob metadata is not a record of every historical publication.
On Tanenbaum (NEVM chain 5700), with the one-confirmation policy, both the normal
publisher and finality gate check current NEVM hash registration before querying
Core blob metadata. A fresh, exact 32-byte hash response from the `0x63`
precompile satisfies this settlement path's registered-hash requirement and
prevents another publication. This path does not require the original Core block
or a block-locator RPC.

A previously observed registration only suppresses fees; it cannot complete the
gate after the fresh lookup fails. Failed or malformed lookups wait without
querying Core or spending, even if Core metadata might still be available.
Only a successful empty native response follows the existing Core finality and
bounded recovery path; an unavailable response is not proof of absence.

This fallback does not apply to mainnet, ChainLock policy, or confirmation
thresholds above one. It does not change proof verification, recover missing
payloads from an unauthenticated source, or clear an unknown wallet outcome.
The existing native hash precompile is sufficient; no new Geth RPC, database
migration, or index rebuild is required.

The existing per-hash recovery clocks, cooldowns, and paid-attempt reservations are
unchanged. Headroom is checked before claiming an attempt. Once a wallet RPC has
been attempted, a failure still consumes that attempt and its cooldown. There is
no same-attempt retry, clock reset, or attempt refund.

## Unknown outcomes and recovery

A timeout, cancellation, malformed response, or failed result persistence can
leave Core processing or retaining the transaction even after the caller stops
waiting. Its outstanding entry therefore remains a **global wallet-writer fence**
across restarts. It does not expire, and a missing mempool entry or an older mined
publication of the same blob hash does not clear it. All RPC errors are handled
conservatively in this first version, including policy errors raced after the
advisory preflight.

Stop and investigate the exact wallet transaction and canonical-chain evidence
when this happens. This version does not automatically reconcile an unknown
transaction ID. Do not delete or edit the journal, clear attempt markers, reset
clocks, disable coordination, downgrade a writer, or switch it to the legacy path
to work around the fence. Such actions would discard the evidence that prevents
duplicate fees or concurrent wallet execution. Any recovery or rollback requires
all writers quiesced and a reviewed, positive reconciliation of outstanding work.

The queue is only a throughput control for the current bounded commit command.
It does not sweep arbitrary historical blobs, alter archive authentication, or
change when a batch is safe to commit.
