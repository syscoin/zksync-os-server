#!/usr/bin/env python3
"""Complete generated release PR versions without resolving dependencies."""

import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
BRANCH = "release-please--branches--main--components--zksync-os-server"


def align_versions(manifest_text, lock_text, versions):
    manifest = tomllib.loads(manifest_text)
    replacements = {}
    for alias, dependency in manifest["workspace"].get("dependencies", {}).items():
        if not isinstance(dependency, dict) or "path" not in dependency:
            continue
        name = dependency.get("package", alias)
        if name not in versions:
            continue
        if not isinstance(dependency.get("version"), str) or not re.fullmatch(r"=[0-9A-Za-z.+-]+", dependency["version"]):
            raise ValueError("internal dependency must have an exact version: " + alias)
        replacements[alias] = "=" + versions[name]

    section = re.search(r"(?ms)^\[workspace\.dependencies\][^\n]*\n(.*?)(?=^\[|\Z)", manifest_text)
    if replacements and section is None:
        raise ValueError("workspace dependency table not found")
    if section:
        content = section[1]
        for alias, version in replacements.items():
            pattern = (r'(?m)^(\s*' + re.escape(alias)
                       + r'\s*=\s*\{[^\n]*?\bversion\s*=\s*")[^"]+("[^\n]*\})')
            content, count = re.subn(pattern, lambda match: match[1] + version + match[2], content)
            if count != 1:
                raise ValueError("expected one inline workspace dependency: " + alias)
        manifest_text = manifest_text[:section.start(1)] + content + manifest_text[section.end(1):]
    expected = manifest
    for alias, version in replacements.items():
        expected["workspace"]["dependencies"][alias]["version"] = version
    if tomllib.loads(manifest_text) != expected:
        raise ValueError("unexpected manifest change")

    packages = tomllib.loads(lock_text)["package"]
    blocks = re.split(r"(?m)(?=^\[\[package\]\]$)", lock_text)
    seen = set()
    for index, block in enumerate(blocks):
        if not block.startswith("[[package]]"):
            continue
        package = tomllib.loads(block)["package"][0]
        name = package["name"]
        if "source" in package or name not in versions:
            continue
        if name in seen:
            raise ValueError("duplicate workspace lock package: " + name)
        seen.add(name)
        if package["version"] != versions[name]:
            # Version-qualified references need disambiguation against other sources.
            reference = name + " " + package["version"]
            if any(reference in item.get("dependencies", [])
                   for item in packages):
                raise ValueError("version-qualified workspace lock reference requires review: " + name)
            blocks[index], count = re.subn(
                r'(?m)^version = "' + re.escape(package["version"]) + r'"$',
                'version = "' + versions[name] + '"', block, count=1)
            if count != 1:
                raise ValueError("workspace lock version not found: " + name)
    if seen != set(versions):
        raise ValueError("workspace package missing from Cargo.lock")
    return manifest_text, "".join(blocks)


def github_api(method, endpoint, body=None):
    command = ["gh", "api", "--method", method, endpoint]
    if body is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=None if body is None else json.dumps(body),
                            text=True, capture_output=True, check=True)
    return json.loads(result.stdout)


def sync_pull_request(repository, number, root=ROOT, api=github_api):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or type(number) is not int or number <= 0:
        raise ValueError("invalid release pull request identity")
    endpoint = "repos/" + repository
    pr = api("GET", f"{endpoint}/pulls/{number}")
    if (pr["state"] != "open" or pr["base"]["ref"] != "main"
            or pr["head"]["repo"]["full_name"] != repository or pr["head"]["ref"] != BRANCH):
        raise ValueError("unexpected release pull request repository or branch")
    # PR metadata can lag Release Please's branch update; read the ref itself.
    reference = f"{endpoint}/git/ref/heads/{BRANCH}"
    head = api("GET", reference)["object"]["sha"]
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        raise ValueError("invalid release head")
    contents = {}
    for path in ("Cargo.toml", "Cargo.lock"):
        file = api("GET", f"{endpoint}/contents/{path}?ref={head}")
        if file.get("encoding") != "base64":
            raise ValueError("release file is not base64 encoded")
        contents[path] = base64.b64decode(file["content"]).decode()
    candidate = tomllib.loads(contents["Cargo.toml"])["workspace"]
    trusted = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    if candidate["members"] != trusted["members"]:
        raise ValueError("release changed workspace membership")
    version = candidate["package"]["version"]
    if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError("invalid workspace release version")
    versions = {}
    # Cargo also includes local path dependencies omitted from workspace.members.
    paths = set(trusted["members"])
    paths.update(dependency["path"] for dependency in trusted.get("dependencies", {}).values()
                 if isinstance(dependency, dict) and "path" in dependency)
    for member in sorted(paths):
        manifest_path = (root / member / "Cargo.toml").resolve()
        if not manifest_path.is_relative_to(root.resolve()):
            raise ValueError("local dependency is outside the repository")
        package = tomllib.loads(manifest_path.read_text())["package"]
        versions[package["name"]] = version if package["version"] == {"workspace": True} else package["version"]
    updated = dict(zip(contents, align_versions(contents["Cargo.toml"], contents["Cargo.lock"], versions)))
    changes = [{"path": path, "mode": "100644", "type": "blob", "content": value}
               for path, value in updated.items() if value != contents[path]]
    if not changes:
        return False
    commit = api("GET", f"{endpoint}/git/commits/{head}")
    tree = api("POST", f"{endpoint}/git/trees", {"base_tree": commit["tree"]["sha"], "tree": changes})
    new_commit = api("POST", f"{endpoint}/git/commits", {
        "message": "chore(release): align internal workspace versions",
        "tree": tree["sha"], "parents": [head]})
    current = api("GET", f"{endpoint}/pulls/{number}")
    if current["state"] != "open" or api("GET", reference)["object"]["sha"] != head:
        raise ValueError("release head changed during version alignment")
    # A concurrent branch advance is also rejected by GitHub's fast-forward check.
    api("PATCH", f"{endpoint}/git/refs/heads/{BRANCH}", {"sha": new_commit["sha"], "force": False})
    return True


def main():
    for pr in json.loads(os.environ["RELEASE_PRS"]):
        changed = sync_pull_request(os.environ["GITHUB_REPOSITORY"], pr["number"])
        print(f"Release PR #{pr['number']}: workspace versions {'updated' if changed else 'already aligned'}")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print("Release workspace alignment failed: " + str(error), file=sys.stderr)
        sys.exit(1)
