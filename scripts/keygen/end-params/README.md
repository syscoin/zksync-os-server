# Locked upstream end_params helper

Airbender `03454c7a41053a4b88bb421e97fb9efe893a92f5` does not track a root
`Cargo.lock`. Running its full CLI workspace with `cargo --locked` therefore
cannot create a reproducible helper build. This minimal CPU-only harness supplies
the reviewed lock without changing the upstream `tools/cli/src/bin/end_params.rs`.
The source is vendored byte-for-byte under upstream's MIT license; do not reformat
or alter it independently of its source hash and pinned upstream revision.

The exact build inputs are:

| Input | SHA256 |
| --- | --- |
| Cargo.toml | `c58938395721dec889f6fe49f991f1dd1ae7cd8b457dd1434f4512d572e30335` |
| Cargo.lock | `06d8dd4fd5efc9ad7db969dbee9949f6bd7af4fc2920d784f33047fac6acc293` |
| src/end_params.rs | `95a59eca0f59f7de4d2e256a6edde5eb4248a563e2ff506d9a8fa3dd136ba2c7` |

The 203-package lock was generated once from wrapper commit
`585595f145cb53a09a130706ca36f80ddcac3961`'s 363-package lock, then audited:
retained package versions, source commits and registry checksums did not change.
Only the new harness root and Airbender's tag-to-exact-revision source spelling
were added; unused dependencies were removed. All three direct dependencies
disable defaults, avoiding the unused JIT/GPU graph. The `execution_utils`
`prover` and `verifier_binaries` features are enabled. Release settings match the
pinned upstream profile. CI never generates or updates this lock.

Build on Linux x86_64 with the pinned Rust `nightly-2026-02-10` toolchain:

```sh
bash scripts/keygen/build-end-params.sh /path/to/pristine-airbender /path/to/new-helper-build
/path/to/new-helper-build/target/release/end_params /path/to/app.bin /path/to/app.text
```

The wrapper verifies the supplied checkout's exact commit and clean state,
compares upstream and packaged helper bytes, checks all three input hashes,
materializes a fresh build directory outside both source checkouts, and always
uses `cargo build --locked --release`. `CARGO_NET_OFFLINE=true` may be used when
the exact dependency cache is already available. It preserves the lock and emits
`SHA256SUMS` and `rustc-version.txt` beside the build. It does not compute app
identity, generate a VK, broadcast, or alter either source checkout.

The initial successful Linux locked/offline build used those exact three inputs
and produced helper binary SHA256
`18fac00a31660952de819529ba530da3f27388d8aa40bf23f5c0c58beaa0d50b`.
This is a recorded single-host build result, not a required cross-host binary
hash: build paths, native toolchain and environment can affect the executable.
The checked-in builder (SHA256
`5154c7c7a01fd18ba472de04c8f8a715602694282d065195fcdba4c314ddc775`)
was subsequently validated with a fresh Linux x86_64 `--locked`, offline release
build, completing in 49.36 seconds and reproducing that exact binary hash. This
test built the helper only; it did not compute app identity or generate a VK.

## Security level boundary

Upstream's helper intentionally prints a diagnostic **Security80** recursion
chain. Its base `app_end_params` are security-independent. The production keygen
workflow consumes only that base result and separately invokes the pinned
wrapper's `compute-aux-params` to verify the full **Security100** words and
commitment, including their match to the server FRI verifier. Do not edit the
upstream helper into a manual Security100 chain or substitute its diagnostic
chain for the wrapper output. All readiness, production identity and release
gates remain unchanged by this build fix.

Run focused, no-build tests with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest -v scripts.tests.test_v32_keygen_end_params
```
