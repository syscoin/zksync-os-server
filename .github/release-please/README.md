# Server release automation

Release-please uses the repository's `GITHUB_TOKEN` on GitHub-hosted Linux.
Repository settings must permit Actions to create pull requests. GitHub places
checks for token-created or token-updated release PRs in an approval-required
state. A maintainer must approve them before merging.

The Release Please action prepares and updates version PRs. Once the generated
PR merges, the workflow stages a private, untagged GitHub draft at that exact
merge SHA. This leaves the public tag and release announcement for the
qualification gate. The workflow defers the next generated version PR while
the current version is still awaiting publication because Release Please
discovers the previous release through its tag. Ordinary main builds and the
development Docker `:latest` tag continue. [Release Please draft behavior](https://github.com/googleapis/release-please/blob/main/docs/manifest-releaser.md)

After the exact draft commit has passed both private checks, rerun the failed
release jobs or dispatch **Release-please** on `main` with the draft's release
ID, version tag and full source SHA. The workflow verifies all three against
the draft and checks the current trusted CPU and CUDA statuses. It creates
the version tag at the selected SHA, checks the ref and publishes that draft.
After publication it marks the merged release PR as tagged so Release Please
can prepare the next version. The binaries and images use the same checked
tag and source. A newer main
commit cannot change the selected source. If GitHub refuses to create the
exact ref, publication stops. If publication is interrupted after tag
creation, retry the same inputs. [Canonical qualification](../../scripts/fixtures/canonical-restore-supervisor.md#canonical-release-qualification) · [Git reference API](https://docs.github.com/en/rest/git/refs#create-a-reference)

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
