# Server release automation

Release-please uses the repository's `GITHUB_TOKEN` on GitHub-hosted Linux.
Repository settings must permit Actions to create pull requests. GitHub places
checks for token-created or token-updated release PRs in an approval-required
state; a maintainer must approve them before merging. Release binaries and images
are invoked in the same run using the exact release tag and peeled source SHA,
because token-created tags do not trigger other push workflows.

Release-please changes the workspace version in `Cargo.toml` and its manifest.
Before merging the generated release PR, update the matching workspace package
versions in `Cargo.lock` without refreshing unrelated dependencies. Review the
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
`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED` exists. Any existing
release smoke or artifact path that depends on that fixture must fail closed
until the canonical fixture is regenerated and verified. See repository build
instructions; do not populate or consume the blocked fixture to satisfy CI.
Release-please still prepares the release PR, but will not create its tag or
GitHub release while the marker exists or the required fixture files are absent.
After the verified fixture is restored and merged into `main`, the next run can
create the release and invoke the binary and image workflows.
