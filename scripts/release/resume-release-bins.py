#!/usr/bin/env python3
"""Resume an interrupted binary release from its original attested artifacts."""

import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def identity():
    repo = os.environ["GH_REPO"]
    tag = os.environ["RELEASE_TAG"]
    sha = os.environ["RELEASE_SOURCE_SHA"]
    run_id = os.environ["ORIGINAL_RUN_ID"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("invalid repository")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", tag):
        raise ValueError("invalid release tag")
    if not re.fullmatch(r"[0-9a-f]{40}", sha) or not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise ValueError("invalid release SHA or run ID")
    return repo, tag, sha, run_id


def validate_run(repo, run_id):
    run = json.loads(gh("api", f"repos/{repo}/actions/runs/{run_id}"))
    allowed = {
        (".github/workflows/release-please.yml", "push"),
        (".github/workflows/release-please.yml", "workflow_dispatch"),
        (".github/workflows/release-please.yml", "workflow_dispatch"),
        (".github/workflows/release-bins.yml", "workflow_dispatch"),
    }
    if (str(run.get("id")) != run_id or run.get("status") != "completed"
            or run.get("head_branch") != "main"
            or run.get("repository", {}).get("full_name") != repo
            or run.get("head_repository", {}).get("full_name") != repo
            or (run.get("path"), run.get("event")) not in allowed):
        raise ValueError("original run is not a trusted main-branch release run")
    pages = json.loads(gh("api", "--paginate", "--slurp",
                          f"repos/{repo}/actions/runs/{run_id}/jobs?per_page=100&filter=all"))
    jobs = [job for page in pages for job in page["jobs"]]
    if not any((j.get("name") == "release-bins" or j.get("name", "").endswith(" / release-bins"))
               and j.get("status") == "completed" and j.get("conclusion") == "success" for j in jobs):
        raise ValueError("original release packaging job did not succeed")


def expected_files(tag, fixture):
    names = {f"zksync-os-server-{tag}-{target}.tar.gz" for target in (
        "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "universal-apple-darwin")}
    if fixture == "true":
        names.add("local-chains.tar.gz")
    elif fixture != "false":
        raise ValueError("invalid fixture readiness")
    return names


def validate_attestation(result, repo, tag, sha, fixture):
    wanted_uri = f"git+https://github.com/{repo}@refs/tags/{tag}"
    for entry in result:
        try:
            predicate = entry["verificationResult"]["statement"]["predicate"]
            definition = predicate["buildDefinition"]
            dependencies = definition["resolvedDependencies"]
            if (definition["buildType"] == "https://actions.github.io/buildtypes/workflow/v1"
                    and definition["internalParameters"]["syscoinCanonicalFixture"] == fixture
                    and any(dep.get("uri") == wanted_uri and dep.get("digest", {}).get("gitCommit") == sha
                            for dep in dependencies)):
                return
        except (KeyError, TypeError, AttributeError):
            continue
    raise ValueError("archive attestation does not bind the requested release source and fixture")


def verify_bundle(root, repo, tag, sha, run_id):
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        raise ValueError("missing staged release")
    if {p.name for p in root.iterdir()} != {"dist", "metadata.json", "attestation-url.txt"}:
        raise ValueError("invalid staged release layout")
    for name in ("metadata.json", "attestation-url.txt"):
        p = root / name
        if p.is_symlink() or not p.is_file():
            raise ValueError("invalid staged release metadata")
    metadata = json.loads((root / "metadata.json").read_text())
    if (metadata.get("tag") != tag or metadata.get("source_sha") != sha
            or str(metadata.get("run_id")) != run_id):
        raise ValueError("staged release identity does not match")
    fixture = metadata.get("fixture_ready")
    dist = root / "dist"
    if dist.is_symlink() or not dist.is_dir():
        raise ValueError("missing release archives")
    files = list(dist.iterdir())
    if {p.name for p in files} != expected_files(tag, fixture) or any(p.is_symlink() or not p.is_file() for p in files):
        raise ValueError("incomplete or unexpected release archives")
    url = (root / "attestation-url.txt").read_text().strip()
    if not re.fullmatch(rf"https://github\.com/{re.escape(repo)}/attestations/[0-9]+", url):
        raise ValueError("invalid original attestation URL")
    for path in sorted(files):
        result = json.loads(gh("attestation", "verify", str(path), "--repo", repo,
                               "--signer-workflow", f"{repo}/.github/workflows/release-bins.yml",
                               "--source-ref", "refs/heads/main", "--deny-self-hosted-runners",
                               "--format", "json"))
        validate_attestation(result, repo, tag, sha, fixture)
    return sorted(files), fixture, url


def update_notes(repo, tag, files, fixture, attestation_url):
    release = json.loads(gh("release", "view", tag, "--json", "body"))
    body = release.get("body") or ""
    if "## Binaries" in body:
        if any(p.name not in body for p in files if p.name != "local-chains.tar.gz") or attestation_url not in body:
            raise ValueError("existing release notes conflict with staged artifacts")
        return
    lines = ["## Binaries", "", "| System | Architecture | Binary | Attestation |", "|:---:|:---:|:---:|:---|"]
    for label, arch, target in (("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
                                ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
                                ("macOS", "universal", "universal-apple-darwin")):
        name = f"zksync-os-server-{tag}-{target}.tar.gz"
        lines.append(f"| {label} | {arch} | [{name}](https://github.com/{repo}/releases/download/{tag}/{name}) | [Attestation]({attestation_url}) |")
    lines.append(f"| Docker | | [zksync-os-server-{tag}](https://github.com/{repo}/pkgs/container/zksync-os-server?tag={tag}) | - |")
    notice = ("Canonical fixture smoke: passed; local chain archive included." if fixture == "true" else
              "Canonical V32 fixture regeneration is pending. Binary CLI and linkage checks passed; block-production smoke was not run and no canonical local-chain archive is included.")
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8") as notes:
        notes.write(body + "\n\n" + "\n".join(lines) + "\n\n" + notice + "\n")
        notes.flush()
        gh("release", "edit", tag, "--notes-file", notes.name)


def main():
    try:
        repo, tag, sha, run_id = identity()
        if sys.argv[1:] == ["validate-run"]:
            validate_run(repo, run_id)
        elif len(sys.argv) == 3 and sys.argv[1] == "publish":
            validate_run(repo, run_id)
            paths, fixture, url = verify_bundle(sys.argv[2], repo, tag, sha, run_id)
            spec = importlib.util.spec_from_file_location("publisher", Path(__file__).with_name("publish-release-assets.py"))
            publisher = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(publisher)
            publisher.publish(tag, paths)
            update_notes(repo, tag, paths, fixture, url)
        else:
            raise ValueError("usage: resume-release-bins.py validate-run | publish STAGED_DIR")
    except (KeyError, OSError, TypeError, ValueError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f"Release recovery failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
