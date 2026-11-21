# Server release automation

Release-please uses the repository's `GITHUB_TOKEN` on GitHub-hosted Linux.
Repository settings must permit Actions to create pull requests. GitHub places
checks for token-created or token-updated release PRs in an approval-required
state; a maintainer must approve them before merging. Release binaries and images
are invoked in the same run using the exact release tag and peeled source SHA,
because token-created tags do not trigger other push workflows.

Binary release archives are staged together as an Actions artifact after the
builds, smoke tests and attestation. If publication fails, rerun the failed
`publish-bins` job in the original run. That rerun uses the original workflow
revision, tag and source, including for historical releases created before
private qualification was required.

For recovery in a separate run, start **Resume server binary release** on
`main` with the release tag, its peeled source SHA and the original run ID.
The current workflow requires private CPU and CUDA qualification for that
exact source before it verifies and publishes the original attested archives.
Historical releases that lack that qualification use their original workflow
run within GitHub's rerun window. The staged artifact must still be available
under the repository's retention policy. Publication stops if it has expired
or an existing asset differs. [GitHub workflow reruns](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs)

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

New versioned releases require an activated private V32 fixture and exact
CPU restore and CUDA settlement qualification for the commit peeled from the
release tag. A pending fixture blocks publication. Hosted binary builds
retain their CLI and linkage checks, and public archives contain the binaries.
Ordinary PR, main and stress CI continue while private qualification is
pending. [Canonical release qualification](../../scripts/fixtures/canonical-restore-supervisor.md#canonical-release-qualification)
