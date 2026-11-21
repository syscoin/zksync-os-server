## Canonical local fixture

The canonical v32.0 fixture captures Core, NEVM, Gateway 57001 and both private
edges for Execution V7 and Proving V8. The repository registers its descriptor
and inventory digests. The snapshots and test configuration stay on the owned
qualification host.

Operators with access to the private package can follow
`scripts/fixtures/canonical-restore-supervisor.md` in the repository to run
the CPU restore and CUDA settlement checks. The restore supervisor starts
Core and NEVM, and the test harness starts the Gateway and both edges from the
exact candidate checkout.

For a fresh deployment with your own configuration, follow the
[Gateway launch guide](../guides/gateway_launch.md).
