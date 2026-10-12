# Locked Airbender end_params helper

The reviewed Syscoin Airbender fork commit
`e8f380777b7e90388c7323d6fd80a3b58c589f17` does not track a root
`Cargo.lock`. Running its full CLI workspace with `cargo --locked` therefore
cannot create a reproducible helper build. This minimal CPU-only harness supplies
the reviewed lock without changing the upstream `tools/cli/src/bin/end_params.rs`.
The source is vendored byte-for-byte under Airbender's MIT license; do not reformat
or alter it independently of its source hash and pinned fork revision.

The exact build inputs are:

| Input | SHA256 |
| --- | --- |
| Cargo.toml | `dc2ca3c5ad7126733eb26dcddaf6cb4e130a5770e6c363f23d3962a8d6634b89` |
| Cargo.lock | `0b004f8fe8b536edfb841f99f1c5d78d058df598d2c7f093c457b7932f55e2e6` |
| src/end_params.rs | `95a59eca0f59f7de4d2e256a6edde5eb4248a563e2ff506d9a8fa3dd136ba2c7` |

The 203-package lock was generated once from wrapper commit
`585595f145cb53a09a130706ca36f80ddcac3961`'s 363-package lock, then audited:
retained package versions, source commits and registry checksums did not change.
Only the new harness root and the reviewed fork's exact-revision source spelling
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
compares fork and packaged helper bytes, checks all three input hashes,
materializes a fresh build directory outside both source checkouts, and always
uses `cargo build --locked --release`. `CARGO_NET_OFFLINE=true` may be used when
the exact dependency cache is already available. It preserves the lock and emits
`SHA256SUMS` and `rustc-version.txt` beside the build. It does not compute app
identity, generate a VK, broadcast, or alter either source checkout.

The vendored helper source is unchanged, but the fork-pinned manifest and lock
are new build inputs. The previous upstream helper binary hash does not attest
this build. A fresh Linux x86_64 locked build and the full app/VK qualification
remain pending.

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
