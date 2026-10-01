# Generated Era release interface — not activated

The copied overlay, manifest and read-only checker are the exact reviewed
domain-corrected crypto bundle. The base is `8fb7c29a4e3174335c6480b23f57822e054f9d5f`,
reviewed source tree `3eefa0f127d1deff365ebffcf489b183cde0e756`, and generated tree
`9b4ff94d1ff647cc00aeb0c3b81dbb922646b946`. Only four exact paths are overlaid;
all 278 identities and retained FFLONK remain unchanged. The manifest binds the
pre-reviewed proof/EVM artifacts but is not a live fixture/deployment certificate.
The helper's four-entry `PREIMAGES` binds the source inventory, stock PLONK/key
bytes and absence of the generated source copy; the unchanged manifest binds
all four exact postimages. Both the intermediate source tree and final generated
tree are checked, including the ignored generated source copy.

## Candidate validation is not release activation

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
to have passed. Reconcile that workflow and the completed release inventory
before activation; do not infer release approval from the registered server key.

`scripts/apply-era-contracts-syscoin-release.py --check-bundle ERA_ROOT` is
read-only: it reconstructs both trees using private Git index/object storage and
classifies the current upstream/source/generated worktree. It does not download,
compile, activate, initialize submodules, change the real index, or authorize a
launch. No tool or fixture hashes can be supplied as command-line overrides.

Ordinary application and `--assert-applied` both fail on the regeneration marker
before inspecting or changing the target. They then require a source-controlled
`CANONICAL_BINDING`, which intentionally remains `None`. Even deleting the marker
cannot enable this candidate. The unchanged source-only helper retains its exact
no-proofs/mock exception; it continues to reject the generated tree. Launcher
dispatch uses those existing modes, not a new operator approval switch. The
zkstack build fingerprint now includes the release helper and exact bundle.

## Mechanical activation inputs, only after genuine fixture acceptance

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
fixture inventory on each assertion; there is deliberately no untrusted stamp
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
checkout mutation or registry activation is part of this implementation.
