#!/usr/bin/env python3
"""Verify owned-host qualification without opening the private fixture."""

import argparse
from functools import cache
import json
import os
import re
import subprocess
import sys
from urllib.error import URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener


REPO = "syscoin/zksync-os-server"
API = f"https://api.github.com/repos/{REPO}"
CONTEXTS = {
    "cpu": "syscoin/v32/canonical-restore-cpu",
    "cuda": "syscoin/v32/canonical-settlement-cuda",
}
SHA = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def descriptor_hash():
    result = subprocess.check_output(
        [sys.executable, "-I", "-B", "scripts/apply-era-contracts-syscoin-release.py",
         "--check-canonical-fixture-binding"],
        text=True,
    )
    binding = json.loads(result)
    digest = binding["descriptor_sha256"]
    if (binding.get("status") != "canonical_fixture_binding_checked"
            or binding.get("fixture_mode") != "private" or not SHA256.fullmatch(digest)
            or not int(digest, 16)):
        raise ValueError("invalid source-controlled canonical fixture binding")
    return digest


def status_page(token, sha, page):
    request = Request(
        f"{API}/commits/{sha}/statuses?per_page=100&page={page}",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2026-03-10",
        },
    )
    with build_opener(NoRedirect()).open(request, timeout=15) as response:
        return json.load(response)


def verify(sha, tree, descriptor, creator_id, context, fetch_page):
    expected = f"v1 tree={tree} descriptor={descriptor}"
    # GitHub returns newest first. A later pending or failure from the approved
    # creator revokes its earlier success for this exact release commit.
    for page in range(1, 11):
        rows = fetch_page(sha, page)
        if not isinstance(rows, list) or len(rows) > 100:
            raise ValueError("invalid GitHub commit statuses response")
        for row in rows:
            creator = row.get("creator") or {}
            status_creator = creator.get("id")
            status_context = row.get("context")
            if (type(status_creator) is not int or status_creator != creator_id
                    or not isinstance(status_context, str)
                    or status_context.casefold() != context):
                continue
            if (status_context != context or row.get("state") != "success"
                    or row.get("url") != f"{API}/statuses/{sha}"
                    or row.get("description") != expected):
                raise ValueError(f"latest trusted {context} status does not qualify {sha}")
            return
        if len(rows) < 100:
            raise ValueError(f"missing trusted {context} status for release commit {sha}")
    raise ValueError(f"GitHub status search exhausted for {sha}")


def verify_release(source_sha, creator_id, fetch_page):
    if not SHA.fullmatch(source_sha) or git("rev-parse", "HEAD") != source_sha:
        raise ValueError("checkout is not the requested exact release commit")
    git("diff", "--quiet", "HEAD", "--")
    tree = git("rev-parse", "HEAD^{tree}")
    descriptor = descriptor_hash()
    # Qualify both lanes against the same response pages for this immutable SHA.
    read_statuses = cache(fetch_page)
    for context in CONTEXTS.values():
        verify(source_sha, tree, descriptor, creator_id, context, read_statuses)
    print(f"Verified owned CPU and CUDA qualification on {source_sha}, tree {tree}, descriptor {descriptor}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-sha", required=True)
    args = parser.parse_args()
    configured_id = os.environ.get("SYSCOIN_CANONICAL_QUALIFIER_ID", "")
    if not re.fullmatch(r"[1-9][0-9]{0,19}", configured_id):
        raise ValueError("set SYSCOIN_CANONICAL_QUALIFIER_ID to the approved numeric GitHub account ID")
    if os.environ.get("GITHUB_REPOSITORY", "").lower() != REPO:
        raise ValueError("unexpected GitHub qualification repository")
    token = os.environ.pop("GITHUB_TOKEN", "")
    if not token:
        raise ValueError("GitHub status read token is unavailable")
    verify_release(args.source_sha, int(configured_id), lambda sha, page: status_page(token, sha, page))


if __name__ == "__main__":
    try:
        main()
    except URLError as error:
        sys.exit(f"error: GitHub qualification status request failed: {error.reason}")
    except (AttributeError, KeyError, OSError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"error: {error}")
