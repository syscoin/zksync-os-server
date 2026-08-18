# SLH-DSA-SHA2-128-24 independent conformance gate

This directory provides an explicit, network-free oracle for the
`SLH-DSA-SHA2-128-24` parameter set in the NIST SP 800-230 Initial Public Draft
(IPD). This draft parameter set is not FIPS-approved, and neither fixture is a
NIST ACVP vector or a NIST-generated KAT.

The gate uses the independent C implementation at
`pq-code-package/slhdsa-c@174c02e42257f95c210963272877c49dbb50070f`. The
upstream checkout is copied to a temporary directory, then
`slhdsa-c-sp800-230-ipd.patch` applies exactly two experimental-bound changes:
`SLH_MAX_LEN` becomes 68 and `SLH_MAX_HP` becomes 22. The harness supplies the
IPD tuple `n=16, h=22, d=1, h'=22, a=24, k=6, lg_w=2, m=21` without changing
the verifier algorithm.

## Run

Prepare the pinned source checkout separately, then run:

```shell
tools/slh-dsa-conformance/run.sh /path/to/slhdsa-c
```

or:

```shell
SLHDSA_C_PATH=/path/to/slhdsa-c tools/slh-dsa-conformance/run.sh
```

The runner never fetches from the network and never mutates the supplied
checkout. It fails closed unless the source is clean and exactly at the pinned
commit, and unless the patch, C harness, Python driver, and both fixture files
match their embedded SHA-256 values. This explicit gate is intentionally not
wired into the default workspace tests.

For both fixtures it requires parity between the public `slh_verify()` API and
the library's internal verifier using the FIPS 205 external empty-context
envelope `0x00 || 0x00 || M`. It also confirms that treating `M` as an already
wrapped internal message fails. Each implementation route must reject mutations
to `M`, `R`, `SIG_FORS`, hypertree WOTS and authentication paths, as well as
short and long signatures.

## Corpus and provenance

The canonical fixture is
`../../contracts/test/vectors/slh_dsa_sha2_128_24_sp800_230_ipd_counter0.json`.
It was generated from `nconsigny/SPHINCS-` using signer-script commit
`49dac7b61c3ba297954f9af360e93a0405082389` and was added as a source fixture at
commit `aedfada38cb0548fe2d5a2070c0c8924f7f261a8`. The same fixture is present at
compatible commit `55b2f3e25d8d7cc0df33ccdb13becca1a168b26f`. The exact command was:

```shell
python3 script/slh_dsa_sha2_128_24_gpu_signer.py \
  0x1111111111111111111111111111111111111111111111111111111111111111 \
  0xc1fd5ba4e304827439265a094a8b82f005662dce23be909f9e179cbce73b5f5d \
  0
```

The JSON records `SK.seed`, `SK.prf`, `PK.seed`, the all-zero 16-byte
`opt_rand`, source fixture and generator script hashes, and the 3,856-byte
signature hash. The legacy
`../../contracts/test/vectors/slh_dsa_sha2_128_24_kat.json` remains accepted as
a regression fixture, but is explicitly marked unreproducible because its
original secret inputs and generation command are unavailable.

The Rust differential runner in `../slh-dsa-difftest/` feeds the same two JSON
fixtures to `SlhDsaSha212824VerifyImpl::execute`. Its input is exactly the 0x101
precompile payload:

```text
pkSeed as a 16-byte value left-aligned in a 32-byte word
|| pkRoot as a 16-byte value left-aligned in a 32-byte word
|| 32-byte message
|| 3,856-byte signature
```

The Solidity twin is
`../../contracts/test/SLHDSASHA212824Differential.t.sol`. Both suites share the
same boundary/mutation corpus; the Rust route selects the portable SHA-256
backend used by the proving RISC-V build.
