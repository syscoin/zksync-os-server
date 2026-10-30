# Server release automation

Release-please uses the repository's `GITHUB_TOKEN` on GitHub-hosted Linux.
Repository settings must permit Actions to create pull requests. GitHub places
checks for token-created or token-updated release PRs in an approval-required
state; a maintainer must approve them before merging. Release binaries and images
are invoked in the same run using the exact release tag and peeled source SHA,
because token-created tags do not trigger other push workflows.

Binary release archives are staged together as an Actions artifact after the
builds, smoke tests and attestation. If publication fails, rerun the failed
`publish-bins` job in the original run. For recovery in a separate run, start
the **Resume server binary release** workflow on `main` with the release tag,
its peeled source SHA and the original run ID. It verifies and publishes the
original archives, retaining their original attestations. The staged artifact
must still be available under the repository's Actions artifact retention policy;
if it has expired or an existing release asset differs, publication stops for
investigation rather than replacing the asset.

Release-please changes the workspace version in `Cargo.toml` and its manifest.
The following workflow step automatically aligns exact local dependency pins
and workspace package versions in `Cargo.lock` on that generated PR. It edits
only these two files, preserves external dependency versions and checksums, and
does not run Cargo resolution or execute code from the PR. It validates the
same-repository release branch and stops if its head changes while preparing the
update. Repeated runs are harmless when versions already agree. An unsupported
manifest layout or ambiguous version-qualified lock reference stops for review.

Before merging the generated release PR, review the
lock, source and dependency graph changes against the existing Linux advisory
exceptions in `scripts/security/native-linux-review.json`, then update its
fingerprints only after that review. The native Linux security-audit PR checks
must pass for the exact release commit. The pre-release check refuses to create
an additional release when the lock or source no longer matches the reviewed
scope; it does not authorize refreshing that scope or replace the full audit.
After that explicit review, obtain the new candidate fingerprints on the
reviewed Linux toolchain with `python3 scripts/check-native-audit.py fingerprints`,
compare them with the reviewed changes, and update the corresponding fields in
`scripts/security/native-linux-review.json` as part of the release PR. Run the
`audit` and `deny` modes of `scripts/check-native-audit.py` against the
reviewed advisory database.

The canonical v32 fixture remains blocked while
`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED` exists. Release builds
may proceed with the available CLI smoke checks, but this does not verify
canonical local-chain execution. The release workflow must not consume or
archive the blocked fixture, and the full canonical smoke check can resume
after the fixture is regenerated and verified.
