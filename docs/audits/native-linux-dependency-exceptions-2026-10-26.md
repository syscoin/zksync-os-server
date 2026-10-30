# Native Linux dependency advisory applicability review — 2026-10-26

This review covers the server source and dependency graph at
`f8d141db2f8666d1ce1a53b6f48eb7a272e2b74f`. It supports the applicability
exceptions below for the reviewed configuration. It does not establish an
application exploit, prove constant-time machine code, or approve a release.

## Scope and enforcement

The scope is **`x86_64-unknown-linux-gnu`**, the pinned
**`nightly-2026-02-10`** Rust toolchain, and the exact recorded dependency graph
and source/build inputs. Zstd is the default bundled library. Alternate system
Zstd linkage and environment overrides that change compiler selection, cfgs,
features, dependency sources, or library linkage are outside this review.

`scripts/check-native-audit.py` and
`scripts/security/native-linux-review.json` must enforce the boundary before
applying any exception. The policy binds the source/build inputs, lockfile hash,
target-filtered all-features graph, package checksums, and individual advisory
record hashes. A workspace `--all-features` invocation is covered only when it
resolves to that exact recorded graph; arbitrary dependency feature combinations
are not covered.

The ordinary `deny.toml` policy retains its existing exceptions and does not gain
these advisory IDs. The guarded runner filters only explicitly reviewed findings
from the raw JSON reports after its checks succeed. Unknown findings, changed
records, changed packages or features, and failed or incomplete tool executions
must remain failures. A successful filtered audit means the recorded findings
have an applicability disposition for this configuration, not that their upstream
packages have been patched.

This review does **not** extend to other architectures, wasm,
separate prover repositories, or a changed server configuration. Server-pinned
prover-library dependencies in the recorded graph do not extend the review to an
independently built prover.

## Source identities and reviewed consumers

The Zstd packages are `zstd 0.13.3`, `zstd-safe 7.2.4`, and
`zstd-sys 2.0.15+zstd.1.5.7`. Their reviewed direct Rust consumers are
`async-compression 0.4.27`, `reth-zstd-compressors 0.6.0`, `reth-nippy-jar 2.5.0`,
and the server's integration-test build script. `librocksdb-sys 0.17.3+10.4.2`
also depends on `zstd-sys` and invokes the native C API, not the affected
`zstd-safe` wrappers.

Registry source identities, recorded by the crates' `.cargo_vcs_info.json`, are:

| Crate | Upstream source identity |
| --- | --- |
| `zstd 0.13.3` | [`gyscos/zstd-rs@52e9d41a645fb49edcf1ed3df765f380dbcf13b2`](https://github.com/gyscos/zstd-rs/tree/52e9d41a645fb49edcf1ed3df765f380dbcf13b2) |
| `zstd-safe 7.2.4`, `zstd-sys 2.0.15+zstd.1.5.7` | [`gyscos/zstd-rs@229054099aa73f7e861762f687d7e07cac1d9b3b`](https://github.com/gyscos/zstd-rs/tree/229054099aa73f7e861762f687d7e07cac1d9b3b) |
| `async-compression 0.4.27` | [`Nullus157/async-compression@7256c69b1a72883c35a524342af0db4c1a62b1ea`](https://github.com/Nullus157/async-compression/tree/7256c69b1a72883c35a524342af0db4c1a62b1ea) |
| `reth-zstd-compressors 0.6.0` | [`paradigmxyz/reth-core@e14f5105b49785bddd3793a86ab2fa49acff5650`](https://github.com/paradigmxyz/reth-core/tree/e14f5105b49785bddd3793a86ab2fa49acff5650) |
| `reth-nippy-jar 2.5.0` | [`paradigmxyz/reth@189c0df32617afc488e0f091dbface1bd72cceb4`](https://github.com/paradigmxyz/reth/tree/189c0df32617afc488e0f091dbface1bd72cceb4) |
| `tower-http 0.6.8` | [`tower-rs/tower-http@33166c8104be9b40ec15c480503d50d8c2312062`](https://github.com/tower-rs/tower-http/tree/33166c8104be9b40ec15c480503d50d8c2312062) |

The recorded feature graph enables `experimental`, `legacy`, `arrays`, `std`,
and `zdict_builder` as applicable. **`experimental` is enabled and is not an
exclusion.** Neither `seekable` nor `zstdmt` is enabled.

Reth's reusable transaction and receipt contexts are
[bulk compressors/decompressors](https://github.com/paradigmxyz/reth-core/blob/e14f5105b49785bddd3793a86ab2fa49acff5650/crates/zstd-compressors/src/lib.rs#L16-L56).
The actual [receipt](https://github.com/paradigmxyz/reth-core/blob/e14f5105b49785bddd3793a86ab2fa49acff5650/crates/codecs/src/alloy/receipt.rs#L63-L88)
and [transaction](https://github.com/paradigmxyz/reth-core/blob/e14f5105b49785bddd3793a86ab2fa49acff5650/crates/codecs/src/alloy/transaction/ethereum.rs#L152-L184)
callers use their bulk operations. Nippy-jar uses bulk dictionary contexts or
[fresh stream encoders/read decoders](https://github.com/paradigmxyz/reth/blob/189c0df32617afc488e0f091dbface1bd72cceb4/crates/storage/nippy-jar/src/compression/zstd.rs#L155-L185).

Async-compression constructs its own [raw decoder](https://github.com/Nullus157/async-compression/blob/7256c69b1a72883c35a524342af0db4c1a62b1ea/src/codec/zstd/decoder.rs#L12-L33)
and [raw encoder](https://github.com/Nullus157/async-compression/blob/7256c69b1a72883c35a524342af0db4c1a62b1ea/src/codec/zstd/encoder.rs#L11-L32).
Tower HTTP's [encoder setup](https://github.com/tower-rs/tower-http/blob/33166c8104be9b40ec15c480503d50d8c2312062/tower-http/src/compression/body.rs#L350-L381)
uses the ordinary quality setting and, when needed, `WindowLog(23)`.

## Zstandard advisory dispositions

The advisory conditions were read from RustSec database snapshot
`a78282a1414d6c28cc228c4eae3570451affb0dc`; the policy additionally binds the
individual record contents so a revised advisory requires review.

| Advisory | Applicability disposition for the scoped build |
| --- | --- |
| [RUSTSEC-2026-0391](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd/RUSTSEC-2026-0391.md) — abandoned stream reused through `with_context` | No reviewed caller uses `with_context`. The stream consumers above create owned contexts, while Reth's reusable contexts remain bulk-only. The vulnerable [borrowed-context constructors](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/stream/raw.rs#L157-L162) are therefore outside the reviewed call paths. |
| [RUSTSEC-2026-0395](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0395.md) — `StableInBuffer(true)` | No reviewed caller enables `StableInBuffer`. [Tower's supplied parameters](https://github.com/tower-rs/tower-http/blob/33166c8104be9b40ec15c480503d50d8c2312062/tower-http/src/compression/body.rs#L350-L381) only constrain the window; Reth's bulk callers do not change this parameter. This is a caller exclusion, not an `experimental`-feature exclusion. |
| [RUSTSEC-2026-0396](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0396.md) — invalid seek table access | The recorded graph does not enable `seekable`, so the affected seekable APIs are not compiled into this build. |
| [RUSTSEC-2026-0397](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0397.md) — `OutBuffer::around_pos` beyond initialized length | No reviewed caller uses `around_pos`. [Raw buffer operations](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/stream/raw.rs#L34-L48) and [async decode/flush](https://github.com/Nullus157/async-compression/blob/7256c69b1a72883c35a524342af0db4c1a62b1ea/src/codec/zstd/decoder.rs#L43-L75) use `around`, beginning at position zero, on bounded output slices. |
| [RUSTSEC-2026-0398](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0398.md) — seekable output length beyond capacity | `seekable` is absent from the recorded graph. Neither seekable decompressor is available in the scoped build. |
| [RUSTSEC-2026-0399](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0399.md) — dictionary lifetime through deprecated initialization | No reviewed caller uses `init_using_cdict` or `init_using_ddict`. Dictionary users use copied dictionaries or [higher-level prepared-dictionary constructors that bind the dictionary reference lifetime](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/bulk/decompressor.rs#L32-L77). This does not assert that arbitrary low-level dictionary API use is safe. |
| [RUSTSEC-2026-0400](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0400.md) — one-shot operation on an unfinished streaming context | The [reused Reth contexts](https://github.com/paradigmxyz/reth-core/blob/e14f5105b49785bddd3793a86ab2fa49acff5650/crates/zstd-compressors/src/lib.rs#L142-L191) only perform bulk operations. The independently owned streaming contexts are not subsequently passed to one-shot operations. The necessary mixed-context sequence is absent. |
| [RUSTSEC-2026-0401](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0401.md) — overflowing training sample sizes | The only reviewed Reth training caller is [under `cfg(test)`](https://github.com/paradigmxyz/reth/blob/189c0df32617afc488e0f091dbface1bd72cceb4/crates/storage/nippy-jar/src/compression/zstd.rs#L192-L229). It derives each size from the corresponding resident vector's length, rather than accepting independent attacker-supplied size metadata. The server release profile also enables overflow checks. No production training caller was found. |
| [RUSTSEC-2026-0402](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0402.md) — multithreaded `CCtx::try_clone` | `zstdmt` is disabled, workers are not enabled by the reviewed callers, and those callers do not invoke `CCtx::try_clone`. |
| [RUSTSEC-2026-0403](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0403.md) — legacy decode error publishes uninitialized vector bytes | No reviewed caller constructs `zstd::stream::write::Decoder`. Reth's bulk API and the ordinary read decoder are explicitly outside the advisory's affected paths. [Async-compression's raw decoder](https://github.com/Nullus157/async-compression/blob/7256c69b1a72883c35a524342af0db4c1a62b1ea/src/codec/zstd/decoder.rs#L43-L75) supplies initialized slices and propagates an error before advancing/publicizing output bytes. Legacy support being enabled does not by itself supply the vulnerable output path. |
| [RUSTSEC-2026-0404](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0404.md) — `DCtx::try_clone` double free | No reviewed caller invokes `DCtx::try_clone`; the bulk and raw decoder wrappers do not implicitly clone their C context. |
| [RUSTSEC-2026-0405](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0405.md) — out-of-range `InBuffer::pos` | The advisory specifically lists `seekable::SeekableCStream::compress_stream`; `seekable` is disabled. Independently, [raw operations](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/stream/raw.rs#L34-L48), [the reader](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/stream/zio/reader.rs#L136-L148), and [the writer](https://github.com/gyscos/zstd-rs/blob/52e9d41a645fb49edcf1ed3df765f380dbcf13b2/src/stream/zio/writer.rs#L242-L252) construct a fresh input buffer at position zero from a real slice. Async codecs use bounds-checked subslices and advance only after successful calls; Reth bulk callers have no streaming position input. No caller accepts arbitrary external `pos` metadata. |
| [RUSTSEC-2026-0406](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-safe/RUSTSEC-2026-0406.md) — changing an unfinished multithreaded context | The required worker threads are absent: `zstdmt` is disabled and no reviewed caller enables `NbWorkers`. This does not cover builds that substitute another native Zstd library or enable workers. |
| [RUSTSEC-2026-0410](https://github.com/RustSec/advisory-db/blob/a78282a1414d6c28cc228c4eae3570451affb0dc/crates/zstd-sys/RUSTSEC-2026-0410.md) — wasm allocator shim | The affected allocator shim is wasm32-specific. The scoped target is explicitly native `x86_64-unknown-linux-gnu`; no wasm exception is granted. |

For the buffer-bound conclusions, `InBuffer::around` initializes position zero
and its position setter checks the slice length in
[`zstd-safe`](https://github.com/gyscos/zstd-rs/blob/229054099aa73f7e861762f687d7e07cac1d9b3b/zstd-safe/src/lib.rs#L1912-L1952).
Async-compression's [`PartialBuffer`](https://github.com/Nullus157/async-compression/blob/7256c69b1a72883c35a524342af0db4c1a62b1ea/src/util.rs#L5-L43)
uses normal bounds-checked slicing. Its decoder's `?` precedes the input/output
advance. Public compressed bytes therefore do not become unchecked native buffer
positions in these call paths.

The integration build script also uses a fresh read decoder and an initialized
fixed-size output buffer, with errors checked before the bytes are written to a
temporary file:
[`integration-tests/build.rs`](https://github.com/syscoin/zksync-os-server/blob/f8d141db2f8666d1ce1a53b6f48eb7a272e2b74f/integration-tests/build.rs#L12-L46).
This observation does not authorize creating or consuming a canonical fixture
while its regeneration marker is present.

## Crypto-bigint applicability boundary

`crypto-bigint 0.5.5` is used by the signing dependency graph. That rules out an
unused-dependency rationale, but does not establish that a routine described by
RUSTSEC-2026-0387 processes a private signing value in this version.

The published crate records VCS identity
[`98f29002552211cbd37dd09d354d1514d84158c4`](https://github.com/RustCrypto/crypto-bigint/tree/98f29002552211cbd37dd09d354d1514d84158c4).
Its [`src/limb/sub.rs`](https://github.com/RustCrypto/crypto-bigint/blob/98f29002552211cbd37dd09d354d1514d84158c4/src/limb/sub.rs#L7-L16)
is byte-identical to the [subtraction file cited in the supplied review](https://github.com/RustCrypto/crypto-bigint/blob/395bb171178990a93ef571664271dabc50749043/src/limb/sub.rs#L7-L16)
at `395bb171178990a93ef571664271dabc50749043`. The complete file SHA-256 is
`2fe52b63f4876beb4d6fb44f2d04ac62231d66b7071097d592c690ceb4944764`.
This establishes which older implementation was reviewed; it does not establish
constant-time output from a particular compiler.

The independent source review verified the following paths against the locked
crate archives. Secret scalars do reach the older bigint add/subtract/compare
operations; the rationale is the specific implementation and callers, not the
absence of secrets from this dependency.

| Path | Applicability finding |
| --- | --- |
| Subtraction | The pinned `Limb::sbb` uses widened integer subtraction. It predates the `overflowing_sub` regression introduced in [PR #1029](https://github.com/RustCrypto/crypto-bigint/pull/1029) and repaired in [PR #1358](https://github.com/RustCrypto/crypto-bigint/pull/1358). This excludes that specific reported regression, without claiming a proof about generated instructions. |
| k256 signing | [`Scalar` multiplication and inversion](https://github.com/RustCrypto/elliptic-curves/blob/5ac8f5d77f11399ff48d87b0554935f6eddda342/k256/src/arithmetic/scalar.rs#L111-L176) use custom wide multiplication/reduction and a fixed addition chain. The ECDSA signing path uses this concrete scalar implementation rather than generic bigint Montgomery multiplication or inversion. |
| p256 arithmetic | [`Scalar` multiplication and inversion](https://github.com/RustCrypto/elliptic-curves/blob/d21a81b2be325db940502fdc5299094b79b450f0/p256/src/arithmetic/scalar.rs#L114-L176) use schoolbook wide multiplication, custom Barrett reduction, and a fixed public inversion exponent. |
| Generic routines and types | No reviewed runtime caller uses bigint `Residue`, `DynResidue`, or generic Montgomery routines. `UintRef` and `BoxedMontyForm` described in the advisory are absent from 0.5.5; its `alloc` feature is disabled. |
| Zero counting | `ScalarPrimitive::random` calls `Uint::random_mod`, whose bit-length calculation processes the public curve modulus. No reviewed curve caller performs bigint zero-count/division operations on a private signing value. |
| Actual remainder caller | Airbender's `scalars/scalar64.rs` uses `Uint::rem` in `eq_mod`/`is_zero` along the [public message/signature recovery path](https://github.com/matter-labs/airbender-platform/blob/a350ee3e08bef4cbe2b7bc968966bee45bc98a17/crates/airbender-crypto/src/secp256k1/recover.rs#L57-L106). These are supplied recovery values rather than a private signing key. |
| Other optional consumers | AWS SigV4a's bigint private-key derivation is not enabled: `aws-sigv4/sigv4a` is absent from the resolved graph. The optional `der` bigint integration is also absent. Enabling either requires re-review. |

The native build wrapper's checked-in zkOS patch also remains within this
boundary. Its manifest changes add the SLH-DSA verification feature and SHA-2;
the [checked lock delta](https://github.com/syscoin/zksync-os-server/blob/f8d141db2f8666d1ce1a53b6f48eb7a272e2b74f/scripts/_patched-zksync-os-workspace.sh#L325-L372)
adds only the `sha2 0.10.9` edge to `basic_system`. It neither enables additional
bigint/curve features nor changes the existing secp256k1 recovery and field hints.
The server's pinned Syscoin Airbender fork is included in the reviewed graph;
there is no additional server-side Airbender patch replay at this source revision.

This supports a conditional applicability exception for the exact pinned Linux
configuration. It does not establish constant-time production machine code or
extend to other callers, compilers, CPU targets, or the separate prover build.

## RUSTSEC-2026-0418: libz-sys build environment

The exact RustSec record at database revision
[`ee1a49a2769a24a596afc1c20cb33748d2769586`](https://github.com/RustSec/advisory-db/blob/ee1a49a2769a24a596afc1c20cb33748d2769586/crates/libz-sys/RUSTSEC-2026-0418.md)
describes a Unix build-script environment dump when `libz-sys` through 1.1.30
runs its zlib probe with `cc` 1.6.0 or later. The report is applicable by crate
version to locked `libz-sys 1.1.22`; the scoped production build has two
independently checked conditions that prevent this specific dump:

- The server's locked Linux feature trees, both default and `gcp`, enable
  `libz-sys/static` through `rocksdb 0.24.0` and `librocksdb-sys`'s `static`
  feature. In the checksum-verified `libz-sys 1.1.22` crate,
  [`build.rs`](https://github.com/rust-lang/libz-sys/blob/7a4e6d74ee26c954ee9c512b0ee7bad81b7b5e06/build.rs)
  defines `want_static` as the static feature OR `LIBZ_SYS_STATIC=1` and takes
  the static-build branch before `zlib_installed` can print the probe command.
  The workspace all-features Linux graph also has `static` enabled.
- The exact `libz-sys` build-dependency edge resolves to checksum-verified
  `cc 1.2.62`, before the advisory's 1.6.0 change. Its
  [`Tool::to_command`](https://github.com/rust-lang/cc-rs/blob/37a5f8f4e1a2eaf4cf483fc0fb97d35761b720c5/src/tool.rs)
  copies only explicitly configured compiler variables, rather than clearing
  and copying the entire ambient environment into the debug-printed command.

The guard now binds both crate versions, registry sources and checksums; checks
the `cc` build edge and static feature in the exact all-features graph; and
independently checks the default and `gcp` server feature trees. It rejects
`LIBZ_SYS_*`, C/C++ compiler, flags, archiver and target-linker environment
overrides in addition to its existing compiler and native-library restrictions.
The advisory record is bound by its SHA-256 and must be re-reviewed if changed.
This is a conditional disposition for the recorded Linux server configuration,
not a claim that other versions, features, compiler arguments or builds cannot
leak secrets. Passing the audit does not retroactively establish the contents
of earlier build caches or artifacts.

The actual cargo-deny 0.20.2 raw report from run `40912534150` at source
`03d4d1b0ba6b6d8a81f8fea80bac48f576a9515a` contained this finding with a null
package checksum; the locked checksum is verified separately. With the reviewed
disposition, that raw report retains no unreviewed vulnerability or unsound
finding. Independently, cargo-audit 0.22.0 on the exact lock, Linux target and
the same RustSec revision reports the finding with the locked checksum, and
its raw report also retains no unreviewed finding. These source/report checks
do not substitute for running both guarded scanners on the final Linux CI head.

## Re-review requirements

Before accepting an exception after any drift, review the changed source and
advisory semantics and rerun the guard and both audit tools on the new exact head.
In particular, re-review is required for:

- A dependency version, source, checksum, resolved feature, compiler/toolchain,
  target, build input, or advisory-record change.
- New raw Zstd use, `with_context`, `around_pos`, context cloning, deprecated
  dictionary initialization, mixed streaming/bulk context use, a write decoder,
  `StableInBuffer`, worker threads, or production dictionary training.
- New private-value bigint operations, different curve/signing consumers, or
  caller changes invalidating the public-input distinction.
- Alternate native library linkage or environment overrides outside the recorded
  build configuration.
- A `libz-sys` or `cc` version/source change, loss of the static feature or the
  reviewed build-dependency edge, or a change in either server feature tree.

The reviewer should approve these per-advisory conditions, not a blanket package
allowlist. A passing guarded audit remains one release check; it does not replace
the remaining release qualification, integration coverage, or security review.

## Validation

Both guarded scanners were exercised on the supplied Linux x86_64 build server
against RustSec snapshot `a78282a1414d6c28cc228c4eae3570451affb0dc`.
`cargo-audit 0.22.0` accepted 14 reviewed matches: its native target filter already
excludes the wasm-only record. `cargo-deny 0.20.2` reported all 15 and accepted
the explicitly scoped wasm exclusion as well. Neither run retained an unreviewed
vulnerability or unsoundness finding. Cargo-deny's separate bans, licenses and
sources checks also passed. Raw scanner output and the applied per-advisory
dispositions are retained under `target/security-audit/` and uploaded by CI.

The guard regressions exercise changed sources, graph edges/features, package
identities, compiler/target overrides, changed advisory records, malformed
reports and unknown findings. These are tests of the exception boundary; they
are not timing or exploitation tests.

## Membership-expiry source re-review

The membership-expiry change removes an immutable field and time-based eligibility check from the prover-service Solidity contract, and updates its Solidity tests. It does not change Rust callers, Cargo manifests, the lockfile, native dependency features, or the reviewed advisory conditions. The source fingerprint is updated to bind the changed files; the guarded CI scanners still run on the final head.

## Prover-service setup source re-review

The subsequent setup changes affect Solidity contracts, launch and prover-service scripts, their tests, documentation, and CI. They do not change Rust source, Cargo manifests, the lockfile, native dependency features, or the advisory dispositions reviewed here. The refreshed source fingerprint binds those inputs; this limited dependency-scope review does not certify the service contracts or deployment. Both guarded scanners remain required on the final Linux CI head.

The follow-up root relay changes affect the prover-service Solidity contracts, Python workers and tests, and documentation. They do not change Rust source, Cargo manifests, the lockfile, native dependency features, or the reviewed advisory dispositions. The source fingerprint is refreshed for these changes; the guarded scanners remain required on the final head.

## L1 sender recovery source re-review

The sender change checks canonical receipts and can retain and rebroadcast an
already-signed EIP-1559 transaction. Its new signer recovery operates on the
public transaction, signature and hash; it does not handle a private signing
value or add a signing implementation. The change does not modify Cargo
manifests, the lockfile, native dependency features, or the reviewed advisory
dispositions. The refreshed source fingerprint binds the changed inputs; the
guarded scanners remain required on the final Linux CI head.

The follow-up validates the identities of visible transactions in a stale
startup suffix and adds sender recovery tests. It adds no cryptographic or
dependency operations to the reviewed production path.

## CI-only release workflow review

The GitHub-hosted release migration changes release and SBOM workflows, their
policy helpers, and tests. The review against server commit `8aa4cf5` found no
changes to Rust callers, Cargo manifests or lockfile, the pinned compiler, native
patches, Dockerfile, or production deployment code. The Linux release build
retains the reviewed target, GCP feature and `tokio_unstable` flag. The audit
scope's source fingerprint is updated for these CI-only files; the lock, graph,
package identities, advisory records and rationales are unchanged. The guarded
Linux audit and deny jobs must still pass for the resulting commit.

## Workspace 0.24.0 release version review

The release updates the inherited workspace version, 45 exact local path
requirements and 48 workspace package versions in `Cargo.lock` from 0.23.0 to
0.24.0. The 1,277 external lockfile package records, including sources,
checksums and dependency lists, are unchanged. The all-feature Linux-target
metadata graph matches the prior reviewed graph after normalizing only the
workspace versions. The lock, source and graph fingerprints are refreshed for
this version-only change; advisory dispositions and fail-closed checks are
unchanged. The guarded Linux audit and deny jobs remain required on the final
release head.

## Prover-service discovery and bootstrap re-review

The service discovery fix queries only the configured gates' acceptance and
activation events, retaining canonical-block, receipt and finalized-source
checks. The launch fix defers the staking vault's updater role until period-zero
service accounting is configured. The remaining changes are regression tests,
their CI invocation and documentation. No Rust source, Cargo manifest, lockfile,
native dependency feature or advisory disposition changes. The source fingerprint
is refreshed for these inputs; both guarded Linux scanners remain required on
the final head.

## Receipt-worker indexed storage re-review

The receipt workers now reuse the existing indexed Python journal for retained
reports, requests and history. A read-only snapshot helper keeps cross-worker
observations coherent; regression tests cover migration and restart. This
changes Python storage and worker callers, tests and documentation only. No Rust
source, Cargo manifest, lockfile, native feature or advisory disposition changes.
The source fingerprint binds the replacement storage paths; both guarded Linux
scanners remain required on the final head.
