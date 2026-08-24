# Multi-node consensus

The server supports two node roles in a multi-node setup:

1. **ConsensusNode** participates in Raft consensus and can become leader.
2. **ExternalNode** does not participate in consensus; it downloads canonized
   blocks from `ZksProtocol` and replays them locally.

A ConsensusNode proposes blocks while it is leader and follows canonized
blocks while it is a replica. An ExternalNode only replays canonized blocks.

> **Canonical v32.0/V8 fixture regeneration is required.** Historical v32.0
> local-chain state was removed, and launch remains fail-closed while
> `local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED` exists. Runnable
> multi-node commands are intentionally withheld so the old contracts, state,
> and verifying key cannot be presented as the canonical V8 deployment.

The runnable guide must be restored only after the v32.0 fixture is regenerated
atomically from the final patched zksync-os v0.4 source, canonical Syscoin Era
contracts, and final V8 verifier artifacts. Removing or bypassing the marker is
not a supported setup procedure.

The batcher pipeline (proof generation and L1 submission) is not yet highly
available. Once the canonical fixture is available, exactly one consensus node
must run it with `batcher_enabled=true`; replicas must use
`batcher_enabled=false`. Raft leader failover keeps block production available,
but settlement pauses if the sole batcher-enabled node is unavailable.
