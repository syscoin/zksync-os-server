#!/usr/bin/env python3
"""Verify owned-host qualification without opening the private fixture."""

import argparse
import json
import os
from pathlib import Path
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


def candidate_commits(event_name, ref, event, checked, tree):
    parents = git("show", "-s", "--format=%P", checked).split()
    if event_name == "pull_request":
        head = event["pull_request"]["head"]["sha"]
        if len(parents) != 2 or head != parents[1]:
            raise ValueError("checkout is not the PR event's merge commit")
    elif event_name == "push" and ref == "refs/heads/main":
        # A single main merge can retain the exact tested PR tree despite a new commit SHA.
        head = parents[1] if len(parents) == 2 and parents[0] == event["before"] else None
    elif event_name == "push":
        head = None
    else:
        raise ValueError("unsupported canonical qualification event")

    if head is not None and SHA.fullmatch(head) and git("rev-parse", f"{head}^{{tree}}") == tree:
        return checked, head
    return (checked,)


def descriptor_hash():
    result = subprocess.check_output(
        [sys.executable, "-B", "scripts/apply-era-contracts-syscoin-release.py",
         "--check-canonical-fixture-binding"],
        text=True,
    )
    binding = json.loads(result)
    digest = binding["descriptor_sha256"]
    if binding.get("fixture_mode") != "private" or not SHA256.fullmatch(digest):
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


def verify(candidates, tree, descriptor, creator_id, context, fetch_page):
    expected = f"v1 tree={tree} descriptor={descriptor}"
    for sha in candidates:
        # GitHub returns newest first. An earlier success cannot override a later
        # update by the trusted account, including an explicit pending or failure.
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
                return sha
            if len(rows) < 100:
                break
        else:
            raise ValueError(f"GitHub status search exhausted for {sha}")
    raise ValueError(f"missing trusted {context} status for the checked tree")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lane", choices=CONTEXTS)
    args = parser.parse_args()
    configured_id = os.environ.get("SYSCOIN_CANONICAL_QUALIFIER_ID", "")
    if not re.fullmatch(r"[1-9][0-9]{0,19}", configured_id):
        raise ValueError("set SYSCOIN_CANONICAL_QUALIFIER_ID to the approved numeric GitHub account ID")
    if os.environ.get("GITHUB_REPOSITORY", "").lower() != REPO:
        raise ValueError("unexpected GitHub qualification repository")
    token = os.environ.pop("GITHUB_TOKEN", "")
    if not token:
        raise ValueError("GitHub status read token is unavailable")
    checked = git("rev-parse", "HEAD")
    if not SHA.fullmatch(checked) or checked != os.environ["GITHUB_SHA"]:
        raise ValueError("checkout is not the exact GitHub event commit")
    git("diff", "--quiet", "HEAD", "--")
    tree = git("rev-parse", "HEAD^{tree}")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    candidates = candidate_commits(os.environ["GITHUB_EVENT_NAME"], os.environ["GITHUB_REF"],
                                  event, checked, tree)
    descriptor = descriptor_hash()
    context = CONTEXTS[args.lane]
    tested = verify(candidates, tree, descriptor, int(configured_id), context,
                    lambda sha, page: status_page(token, sha, page))
    print(f"Verified {context} on tested commit {tested}, tree {tree}, descriptor {descriptor}.")


if __name__ == "__main__":
    try:
        main()
    except URLError as error:
        sys.exit(f"error: GitHub qualification status request failed: {error.reason}")
    except (AttributeError, KeyError, OSError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"error: {error}")
