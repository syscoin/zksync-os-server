# Generated Era sources and separate canonical fixture certification

The overlay, manifest and read-only checker bind the reviewed domain-corrected
crypto bundle to the source tree containing the native priority observation getters. The base is `8fb7c29a4e3174335c6480b23f57822e054f9d5f`,
reviewed source tree `264d98e758c3a032942dfb08ee7d87a3f46288b4`, and generated tree
`ff5565cd22b61259d6f886e9a0130f788bbdb11c`. Only four exact paths are overlaid;
all 278 identities and retained FFLONK remain unchanged. The manifest binds the
pre-reviewed proof/EVM artifacts but is not a live fixture/deployment certificate.
The helper's four-entry `PREIMAGES` binds the source inventory, stock PLONK/key
bytes and absence of the generated source copy; the manifest binds
all four exact postimages. Both the intermediate source tree and final generated
tree are checked, including the ignored generated source copy.

## Completed crypto sources are not a packaged fixture

The server now registers the reproduced Syscoin guest tree
`6935489bdbc7b1ed31e608677d1b2418b10691b5` and its Security100 key
`0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe`.
The identities have been exercised by genuine private service proofs and native
verification with real DA and settlement. This does not qualify the remaining
edge/bridge, sustained performance, five-role snapshot/restore or public rollout
gates. No fixture descriptor or successful restore is manufactured by this PR.

Key generation used one host under the explicitly approved validation constraint.
It is not two-independent-host reproduction. The existing production workflow's
identity sentinels and two-runner reconciliation remain gated and are not claimed
to have passed. Those historical workflow results must not be invented, and that
workflow is not a prerequisite imposed by this source-materialization helper.

`scripts/apply-era-contracts-syscoin-release.py --check-bundle ERA_ROOT` is
read-only: it reconstructs both trees using private Git index/object storage and
classifies the current upstream/source/generated worktree. It does not download,
compile, activate, initialize submodules, change the real index, or authorize a
launch. No tool or fixture hashes can be supplied as command-line overrides.

Ordinary application and `--assert-applied` authenticate the final server app/VK
source bytes, exact bundle, complete source/generated trees, and clean initialized
submodules. They do not require a local-chain snapshot before the deployment that
would produce it. Both return `canonical_fixture_authorized: false`; successful
source materialization does not certify deployment, restore or rollout.

The fixed V32 Era/zkstack source pins are source-controlled independently of the
absent fixture's `versions.yaml`. The real launcher still requires its existing
GPU modes without a mock verifier, authenticates live Gateway identities, and the
node checks the deployed production verifier against the compiled V8 VK. The
unchanged source-only helper retains its explicit no-proofs/mock lane and rejects
the generated tree. No operator approval switch or hash override is added. The
zkstack build fingerprint includes the release helper and exact bundle.

`scripts/apply-era-contracts-syscoin-release.py --check-canonical-fixture` is a
distinct read-only check. It fails first on the regeneration marker, then requires
the source-controlled `CANONICAL_BINDING`, which intentionally remains `None`.
Even deleting the marker cannot certify the absent fixture. `run_local.sh` and the
Rust fixture consumers remain blocked with an empty trusted-descriptor registry.

## Canonical fixture binding, only after genuine fixture acceptance

Fill `CANONICAL_BINDING` with exactly this structure (no nulls/placeholders):

```text
descriptor_sha256: SHA256(local-chains/v32.0/l1-backend.json)
versions_sha256: SHA256(local-chains/v32.0/versions.yaml)
identity_sha256:
  source_identities: SHA256(file named by descriptor inventory.source_identities)
  proof_identities: SHA256(file named by descriptor inventory.proof_identities)
  deployment_record: SHA256(file named by descriptor inventory.deployment_record)
  clean_boundary: SHA256(file named by descriptor inventory.clean_boundary)
  snapshot_manifest: SHA256(file named by descriptor inventory.snapshot_manifest)
```

These must come from the accepted completed five-role fixture, never a synthetic
test record. The same descriptor hash must appear once in the consumer's literal
`TRUSTED_DESCRIPTORS` registry for `v32.0`. The helper verifies the real
31337→57001→{6565,6566} topology, five-confirmation policy, all five snapshot and
seven tool roles, all referenced file sizes/hashes, distinct safe paths, and
the pinned final server VK/FRI identity source bytes. This hashes the complete
fixture inventory on each explicit fixture check; there is no untrusted stamp
or cached-success shortcut. Changing app identity source requires renewed review
of `APP_SOURCES`, not an environment override.

Promote that binding, registry entry, complete fixture and marker removal
coherently only after actual stopped-boundary, restore, DA, server-frontier and
proof/settlement qualification. This source interface does not create those
records, interpret synthetic tests as qualification, or assert that private
57001 evidence alone qualifies production root-chain deployment.

The target Era checkout must remain at the exact upstream HEAD, with no staged,
partial or unrelated changes. Application accepts only the upstream tree or
exact reviewed source tree and produces the exact generated tree; assertion
accepts only the generated tree. Already initialized, clean exact submodules
are required. No automatic repair, reset or retry is performed after a partial
failure. The real Git index is not changed. Existing ignored build outputs are
not artifact attestation; normal artifact/build gates remain necessary.

Focused tests use synthetic temporary repositories/fixtures only; the real
bundle reconstruction is read-only. No build, deployment, signing, original
checkout mutation or fixture registry activation is part of this implementation.

## Priority observation source integration

The service source adds only the concrete Getters facet's priority transaction
timestamp and tree-height observations, their selectors, generated inventory row,
and regression test. The shared IGetters interface, genesis predeploys, real VK,
generated PLONK/key bytes, and guest application identity are unchanged. The
source-only inventory and the generated overlay are re-attested as distinct exact
trees; the overlay still changes only its two permitted verifier inventory rows.

`crypto_validation_provenance` retains the original PR323 source/generated trees
for the existing real-proof, EVM, Cancun and native-build archive records. Their
unchanged hashes are historical crypto evidence, not a claim that a real proof
or deployment was rerun for the service source tree. The published offline
critical Gateway input/helper/result under `scripts/keygen/gateway-identity` is
also preserved byte-for-byte at its historical source tree. It is not consumed
as current deployment authorization. Before a fresh service deployment, rebuild
and rederive the full CTM configuration, including the new Getters facet address
and selectors, and recheck the critical timelock/relay identities against those
artifacts. The production identity and canonical fixture regeneration gates
remain closed.
