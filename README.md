# ZKsync OS Server

[![Logo](.github/assets/zksync-os-logo.png)](https://zksync.io/)

## What is ZKsync OS Server?

ZKsync OS Server is the sequencer implementation for the [ZKsync OS](https://github.com/matter-labs/zksync-os),
the new operating system of the ZK Stack.<br>
The ZKsync OS Server design optimizes for throughput, low latency, and a seamless development experience.

## [Install](https://matter-labs.github.io/zksync-os-server/latest/setup) | [User docs](https://docs.zksync.io/zksync-network/zksync-os) | [Developer docs](https://matter-labs.github.io/zksync-os-server/latest/) | [![CI](https://github.com/matter-labs/zksync-os-server/actions/workflows/ci.yml/badge.svg)](https://github.com/matter-labs/zksync-os-server/actions/workflows/ci.yml)

## Design principles

* Minimal, async persistence
  * to meet throughput and latency requirements, we avoid synchronous persistence at the critical path. Additionally,
    we aim at storing only the data that is strictly needed - minimizing the potential for state inconsistency
* Easy to replay arbitrary blocks
  * Sequencer: components are idempotent
  * Batcher: `batcher` component skips all blocks until the first uncommitted batch.
    Thus, downstream components only receive batches that they need to act upon 
* State - strong separation between
  * Actual state - data needed to execute VM: key-value storage and preimages map
  * Receipts repositories - data only needed in API
  * Data related to Proofs and L1 - not needed by sequencer / JSON RPC - only introduced downstream from `batcher`

## Quickstart

The canonical v32.0/V8 local fixture is pending regeneration with the final V8
verification key and patched-v0.4 Era contracts. Repository launch/config helpers
fail closed while
`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED` exists; do not launch the
old fixture bytes from an external checkout.

For more configuration and detailed instructions, check the [developer documentation](https://matter-labs.github.io/zksync-os-server/latest).

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for contribution guidelines.

## Security

See [SECURITY.md](./SECURITY.md) for security policy details.

## Policies

- [Security policy](SECURITY.md)
- [Contribution policy](CONTRIBUTING.md)

## License

ZKsync OS repositories are distributed under the terms of either

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/blog/license/mit/>)

at your option.

## Official Links

- [Website](https://zksync.io/)
- [GitHub](https://github.com/matter-labs)
- [ZK Credo](https://github.com/zksync/credo)
- [Twitter](https://twitter.com/zksync)
- [Twitter for Developers](https://twitter.com/zkSyncDevs)
- [Discord](https://join.zksync.dev/)
- [Mirror](https://zksync.mirror.xyz/)
- [Youtube](https://www.youtube.com/@zksync-io)
