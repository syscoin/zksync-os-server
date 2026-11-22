#!/usr/bin/env python3
"""Stage the generated version PR privately and publish only an explicitly qualified SHA."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPRedirectHandler, Request, build_opener

REPO = "syscoin/zksync-os-server"
RELEASE_BRANCH = "release-please--branches--main--components--zksync-os-server"
MANIFEST = ".github/release-please/manifest.json"
VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"
SHA = r"[0-9a-f]{40}"


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def api_for(token):
    if not token:
        raise ValueError("GitHub release token is unavailable")
    opener = build_opener(NoRedirect())

    def api(method, path, data=None, *, missing=False, version="2026-03-10"):
        request = Request(
            f"https://api.github.com/repos/{REPO}/{path}",
            headers={"Accept": "application/vnd.github+json", "Authorization": f"Bearer {token}",
                     "Content-Type": "application/json", "X-GitHub-Api-Version": version},
            data=json.dumps(data).encode() if data is not None else None,
            method=method,
        )
        try:
            with opener.open(request, timeout=30) as response:
                return None if response.status == 204 else json.load(response)
        except HTTPError as error:
            if missing and error.code == 404:
                return None
            raise ValueError(f"GitHub {method} {path.split('?')[0]} returned HTTP {error.code}") from None
        except URLError:
            raise ValueError(f"GitHub {method} {path.split('?')[0]} did not return a result") from None

    return api


def manifest_tag(source):
    manifest = json.loads(git("show", f"{source}:{MANIFEST}"))
    if set(manifest) != {"."} or not re.fullmatch(VERSION, manifest["."]):
        raise ValueError("invalid release version in source manifest")
    return "v" + manifest["."]


def lookup_generated_pr(api, source):
    # GitHub removed merge_commit_sha from pull request payloads in 2026-03-10.
    for pr in api("GET", f"commits/{source}/pulls?per_page=100", version="2022-11-28"):
        if (type(pr["number"]) is int and pr["number"] > 0
                and pr.get("merge_commit_sha") == source and pr["merged_at"]
                and pr["base"]["ref"] == "main" and pr["head"]["ref"] == RELEASE_BRANCH
                and pr["base"]["repo"]["full_name"] == REPO
                and (pr["head"]["repo"] or {}).get("full_name") == REPO):
            return pr
    raise ValueError("the selected source is not a merged generated server release PR")


def release_notes(source, tag):
    changelog = git("show", f"{source}:CHANGELOG.md")
    entries = list(re.finditer(r"(?m)^## ", changelog))
    if not entries or not changelog.startswith(f"[{tag[1:]}]", entries[0].end()):
        raise ValueError("generated changelog does not start with the requested release")
    end = entries[1].start() if len(entries) > 1 else len(changelog)
    note = changelog[entries[0].end():end].partition("\n")[2].strip()
    if not note:
        raise ValueError("generated changelog entry is empty")
    return note


def release_row(row, tag, source, release_id=None):
    if (type(row["id"]) is not int or row["id"] <= 0
            or (release_id is not None and str(row["id"]) != release_id)
            or row["tag_name"] != tag or row["target_commitish"] != source
            or type(row["draft"]) is not bool):
        raise ValueError("release ID, target SHA or tag differs from the selected candidate")
    return row


def existing_draft(api, tag, source):
    for page in range(1, 21):
        rows = api("GET", f"releases?per_page=100&page={page}")
        for row in rows:
            if row["tag_name"] == tag:
                row = release_row(row, tag, source)
                if not row["draft"]:
                    raise ValueError("release became public while staging")
                return row
        if len(rows) < 100:
            return None
    raise ValueError("cannot establish whether the requested version already has a draft")


def ensure_no_ref(api, tag):
    if api("GET", f"git/ref/tags/{tag}", missing=True) is not None:
        raise ValueError("tag exists without a published release; use the exact qualified draft retry")


def stage(api, checked):
    if git("rev-parse", "HEAD") != checked or not re.fullmatch(SHA, checked):
        raise ValueError("checkout differs from the triggering main commit")
    tag = manifest_tag(checked)
    published = api("GET", f"releases/tags/{tag}", missing=True)
    if published is not None:
        if published["tag_name"] != tag or type(published["draft"]) is not bool:
            raise ValueError("published tag lookup returned a different release")
    if published is not None and not published["draft"]:
        # The upstream PR-only action targets live main, so skip a superseded run.
        main = api("GET", "git/ref/heads/main")
        return {"prepare_pr": str(main["object"]["sha"] == checked).lower()}
    source = git("log", "-1", "--first-parent", "--format=%H", checked, "--", MANIFEST)
    if not re.fullmatch(SHA, source) or manifest_tag(source) != tag:
        raise ValueError("could not locate the version-changing commit")
    lookup_generated_pr(api, source)
    draft = existing_draft(api, tag, source)
    if draft is None:
        ensure_no_ref(api, tag)
        draft = release_row(api("POST", "releases", {
            "tag_name": tag, "target_commitish": source, "name": tag,
            "body": release_notes(source, tag), "draft": True, "prerelease": "-" in tag,
        }), tag, source)
        if not draft["draft"]:
            raise ValueError("GitHub did not return a private draft")
    print("The private release draft is ready for qualification.")
    # Later ordinary commits can recover a missing draft without repeatedly triggering its gate.
    return ({"release_id": str(draft["id"]), "tag": tag, "source_sha": source}
            if checked == source else {})


def selected(api, release_id, tag, source):
    if not (re.fullmatch(r"[1-9][0-9]*", release_id)
            and re.fullmatch("v" + VERSION, tag) and re.fullmatch(SHA, source)):
        raise ValueError("release selection requires the exact draft ID, version tag and full source SHA")
    return release_row(api("GET", f"releases/{release_id}"), tag, source, release_id)


def checked_ref(api, tag, source, *, missing=False):
    row = api("GET", f"git/ref/tags/{tag}", missing=missing)
    if row is None:
        return None
    if (row["ref"] != f"refs/tags/{tag}" or row["object"]["type"] != "commit"
            or row["object"]["sha"] != source):
        raise ValueError("existing version tag does not point to the qualified commit")
    return row


def promote(api, release_id, tag, source):
    if git("rev-parse", "HEAD") != source or manifest_tag(source) != tag:
        raise ValueError("checkout or manifest does not match the qualified release")
    git("diff", "--quiet", "HEAD", "--")
    row = selected(api, release_id, tag, source)
    pr = lookup_generated_pr(api, source)
    if row["draft"]:
        if checked_ref(api, tag, source, missing=True) is None:
            # Creation is exclusive. A concurrent existing tag fails the request.
            api("POST", "git/refs", {"ref": f"refs/tags/{tag}", "sha": source})
        checked_ref(api, tag, source)
        row = release_row(api("PATCH", f"releases/{release_id}", {
            "draft": False, "tag_name": tag, "target_commitish": source,
        }), tag, source, release_id)
    if row["draft"]:
        raise ValueError("GitHub did not publish the verified release")
    checked_ref(api, tag, source)

    # Release Please blocks the next version while a merged PR is still pending.
    labels = {label["name"] for label in pr["labels"]}
    path = f"issues/{pr['number']}/labels"
    tagged = "autorelease: tagged"
    pending = "autorelease: pending"
    if tagged not in labels:
        api("POST", path, {"labels": [tagged]})
    if pending in labels:
        api("DELETE", f"{path}/{quote(pending, safe='')}", missing=True)
    current = api("GET", f"pulls/{pr['number']}")
    labels = {label["name"] for label in current["labels"]}
    if current["number"] != pr["number"] or tagged not in labels or pending in labels:
        raise ValueError("published release PR labels did not transition to tagged")
    return {"tag": tag, "source_sha": source}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "publish"))
    args = parser.parse_args()
    if os.environ.get("GITHUB_REPOSITORY") != REPO or os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise ValueError("release workflow must run on syscoin/zksync-os-server main")
    api = api_for(os.environ.pop("GH_TOKEN", ""))
    release_id, tag, source = (os.environ.get(name, "") for name in
                               ("SELECTED_RELEASE_ID", "SELECTED_RELEASE_TAG", "SELECTED_SOURCE_SHA"))
    if args.command == "publish":
        outputs = promote(api, release_id, tag, source)
    elif release_id or tag or source:
        selected(api, release_id, tag, source)
        outputs = {"release_id": release_id, "tag": tag, "source_sha": source}
    else:
        outputs = stage(api, os.environ["GITHUB_SHA"])
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as stream:
        for key, value in outputs.items():
            stream.write(f"{key}={value}\n")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"Release qualification orchestration failed: {error}")
