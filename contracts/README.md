## Production Solidity Contracts

This Foundry project contains deployable contracts for the zkSYS launch surface.
Security review should treat these contracts as production code, not integration
test fixtures.

### Layout

- `src/zksys/`: canonical L2 zkSYS token, issuer, NEVM membership fact
  registry, reward weight registry, L1 registry bridge adapter, and proxy
  deployment helpers.
- `src/pali/`: Pali ERC-4337 smart account, validators, factory, and verifier.
  Pali does not use an ERC-4337 paymaster; zkSYS fee payment is implemented by
  the patched ZKsync OS bootloader and `src/zksys/ZkSysGasTank.sol`.

### Build

```shell
forge build
```

### SLH-DSA-SHA2-128-24 Verifier Status

<!-- SYSCOIN: Consensus/release qualification for the limited-signature verifier. -->

`src/pali/SLHDSASHA212824Verifier.sol` is a copied Solidity/Yul verifier for the
`SLH-DSA-SHA2-128-24` limited-signature parameter set proposed by the NIST
SP 800-230 Initial Public Draft. It uses the tuple `n=16, h=22, d=1, h'=22,
a=24, k=6, lg_w=2, m=21`, the FIPS 205 external interface with empty context,
and a strict `2^24` signature limit per key. This draft parameter set is not a
FIPS-approved parameter set and its fixtures are not NIST ACVP vectors.

The upstream SPHINCS- repository models this verifier in Verity / Lean 4. Its
theorem proves that hand-transcribed model refines its byte-level verifier spec
under the stated trust surface; it is not a proof of the deployed Solidity/Yul,
Rust or GPU implementations, and it is not a
machine-checked cryptographic EUF-CMA proof. Remaining assumptions include the
SHA-256 precompile/model bridge, an opaque SHA-256 primitive package, and
source-to-model transcription fidelity.

The shared conformance corpus contains two valid fixtures:

- `test/vectors/slh_dsa_sha2_128_24_sp800_230_ipd_counter0.json` is the canonical
  reproducible counter-0 fixture. It records the complete deterministic signing
  inputs, command, generator commits, and source/signature hashes.
- `test/vectors/slh_dsa_sha2_128_24_kat.json` is retained only as an
  unreproducible historical regression fixture. Its original generator, secret
  seeds, and `opt_rand` were not recorded, so release conformance must not rely
  on it alone.

Both fixtures run through the Solidity verifier and the portable Rust OS
system-function implementation. The latter consumes the exact precompile layout
`pkSeed[32] || pkRoot[32] || message[32] || signature[3856]`.
The deterministic mutation sweep is finite regression coverage, not a general
equivalence proof. Keep the per-key signature-count policy outside this
stateless verifier.

An explicit, network-free external conformance gate is documented in
`../tools/slh-dsa-conformance/README.md`. It compiles a hash-pinned
`pq-code-package/slhdsa-c` checkout with the two-line experimental-parameter
adaptation, accepts both fixtures, and rejects modified message, `R`, FORS,
hypertree WOTS/authentication, short, and long cases. It is intentionally not a
mandatory default workspace test because the independent source checkout must
be supplied by the operator.
