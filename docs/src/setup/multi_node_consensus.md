# Multi-node consensus

The server supports two node roles in a multi-node setup:

1. **ConsensusNode** participates in Raft consensus and can become leader.
2. **ExternalNode** does not participate in consensus; it downloads canonized
   blocks from `ZksProtocol` and replays them locally.

A ConsensusNode proposes blocks while it is leader and follows canonized
blocks while it is a replica. An ExternalNode only replays canonized blocks.

The canonical v32.0/V8 fixture is a private release-qualification capture of
Core, NEVM, one Gateway and two edges. For a fresh deployment with your own
configuration, follow the [Gateway launch guide](../guides/gateway_launch.md).

The batcher pipeline (proof generation and L1 submission) is not yet highly
available. Exactly one consensus node must run it with `batcher_enabled=true`; replicas must use
`batcher_enabled=false`. Raft leader failover keeps block production available,
but settlement pauses if the sole batcher-enabled node is unavailable.
