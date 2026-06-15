## Test Contracts

This directory contains a [Foundry](https://book.getfoundry.sh/) project with contracts that are used by integration tests.

### Build

```shell
$ forge build
```

Artifacts end up in `./out/<contract-name>.sol/<contract-name>.json` and are used by `zksync_os_integration_tests` via `alloy::sol!` macro.

### SLH-DSA-SHA2-128-24 verifier status

`src/passkey/SLHDSASHA212824Verifier.sol` is a copied Solidity/Yul verifier for
the NIST SP 800-230 `SLH-DSA-SHA2-128-24` parameter set. The intended security
level is NIST category 1 / roughly 128-bit post-quantum security, subject to the
scheme assumptions and per-key signing budget.

The upstream SPHINCS- repository models this verifier in Verity / Lean 4, but
that proof is an implementation-correctness result, not a machine-checked
cryptographic EUF-CMA proof. The upstream theorem proves the hand-transcribed
model refines its byte-level verifier spec under the stated trust surface.
Remaining assumptions include the SHA-256 precompile/model bridge, an opaque
SHA-256 primitive package, and source-to-model transcription fidelity.

Local tests currently cover the Pali validator module integration, fail-closed
behavior, malformed signature length, non-canonical public keys, and rejection of
an all-zero 3,856-byte signature. Before production use, pin at least one
independent valid known-answer vector from a signer/reference implementation and
keep the per-key signature-count policy outside this stateless verifier.
