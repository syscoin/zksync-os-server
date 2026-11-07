#!/usr/bin/env python3
"""Complete generated release PR versions without resolving dependencies."""

import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
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



REVIEW_PATH = "scripts/security/native-linux-review.json"
REVIEW_INPUTS = (REVIEW_PATH, "scripts/check-native-audit.py", "scripts/tests/test_native_audit.py")
RELEASE_MANIFEST = ".github/release-please/manifest.json"
RELEASE_FILES = {"Cargo.toml", "Cargo.lock", RELEASE_MANIFEST, "CHANGELOG.md", REVIEW_PATH}
FINGERPRINTS = {"lock_sha256", "source_sha256", "graph_sha256"}


def metadata(root):
    command = ["cargo", "metadata", "--locked", "--all-features", "--filter-platform",
               "x86_64-unknown-linux-gnu", "--format-version", "1"]
    return json.loads(subprocess.check_output(command, cwd=root, text=True))


def audit_module(root):
    spec = importlib.util.spec_from_file_location("native_audit", root / "scripts/check-native-audit.py")
    if spec is None or spec.loader is None:
        raise ValueError("missing native audit guard")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git_output(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def release_manifest_from_base(text, old_version, new_version):
    section = re.search(r"(?ms)^\[workspace\.package\][^\n]*\n(.*?)(?=^\[|\Z)", text)
    if section is None:
        raise ValueError("workspace package table not found")
    content, count = re.subn(
        r'(?m)^(\s*version\s*=\s*)"' + re.escape(old_version) + r'"(\s*(?:#.*)?)$',
        lambda match: match[1] + json.dumps(new_version) + match[2], section[1])
    if count != 1:
        raise ValueError("unexpected workspace package version layout")
    return text[:section.start(1)] + content + text[section.end(1):]


def validate_version_only(root, candidate, versions, version):
    base_manifest = (root / "Cargo.toml").read_text()
    old_version = tomllib.loads(base_manifest)["workspace"]["package"]["version"]
    source = release_manifest_from_base(base_manifest, old_version, version)
    expected_manifest, expected_lock = align_versions(source, (root / "Cargo.lock").read_text(), versions)
    if candidate["Cargo.toml"] != expected_manifest or candidate["Cargo.lock"] != expected_lock:
        raise ValueError("release Cargo graph is not an exact workspace version transformation")
    original = json.loads((root / RELEASE_MANIFEST).read_text())
    expected = dict(original)
    expected["."] = version
    if json.loads(candidate[RELEASE_MANIFEST]) != expected:
        raise ValueError("unexpected release manifest change")


def validate_release_tree(api, endpoint, base, head):
    comparison = api("GET", f"{endpoint}/compare/{base}...{head}")
    if comparison["merge_base_commit"]["sha"] != base or comparison["status"] not in ("ahead", "identical"):
        raise ValueError("release head does not descend from the reviewed base")
    trees = []
    for revision in (base, head):
        result = api("GET", f"{endpoint}/git/trees/{revision}?recursive=1")
        if result.get("truncated"):
            raise ValueError("truncated release source tree")
        trees.append({entry["path"]: (entry["mode"], entry["type"], entry["sha"])
                      for entry in result["tree"] if entry["type"] != "tree"})
    before, after = trees
    for path in set(before) | set(after):
        if path not in RELEASE_FILES and before.get(path) != after.get(path):
            raise ValueError("unreviewed release source change: " + path)
        if path in RELEASE_FILES and (path not in before or path not in after
                                      or before[path][:2] != after[path][:2]):
            raise ValueError("unexpected release file mode or removal: " + path)


def normalized_graph(audit, graph, root, workspace_names):
    graph = copy.deepcopy(graph)
    for package in graph["packages"]:
        if package["source"] is None and package["name"] in workspace_names:
            package["version"] = "workspace-version"
    return audit.graph_fingerprint(graph, root)


def validate_committed_review_inputs(root):
    # These files are excluded from the source fingerprint and must be checked
    # directly before the guard module is imported and used.
    for name in REVIEW_INPUTS:
        expected = subprocess.check_output(["git", "-C", str(root), "show", "HEAD:" + name])
        if (root / name).read_bytes() != expected:
            raise ValueError("reviewed base audit input differs from the committed file: " + name)


def validate_audit_environment(audit, root, policy):
    audit.validate_host(os.environ, platform.system(), platform.machine())
    compiler = subprocess.check_output(["rustc", "--version", "--verbose"], cwd=root, text=True)
    if f"commit-hash: {policy['rustc_commit']}\n" not in compiler:
        raise ValueError("compiler differs from the reviewed toolchain")


def refresh_review(root, candidate, versions, version):
    # Reaffirm the existing review first. Only the exact mechanical release delta
    # is eligible for deriving new source, lock and graph fingerprints.
    validate_version_only(root, candidate, versions, version)
    validate_committed_review_inputs(root)
    audit = audit_module(root)
    policy = json.loads((root / REVIEW_PATH).read_text())
    validate_audit_environment(audit, root, policy)
    branch_policy = json.loads(candidate[REVIEW_PATH])
    if ({k: v for k, v in branch_policy.items() if k not in FINGERPRINTS}
            != {k: v for k, v in policy.items() if k not in FINGERPRINTS}):
        raise ValueError("release changed native audit dispositions")
    original_lock = (root / "Cargo.lock").read_bytes()
    before = metadata(root)
    if (root / "Cargo.lock").read_bytes() != original_lock:
        raise ValueError("reviewed Cargo.lock changed during metadata")
    audit.validate_context(root, policy, before)
    audit.validate_libz_server_builds(root, policy)
    audit.validate_context(root, policy, metadata(root))
    with tempfile.TemporaryDirectory(prefix="release-audit-projection-") as directory:
        projected = Path(directory) / "source"
        subprocess.run(["git", "clone", "--quiet", "--shared", str(root), str(projected)], check=True)
        for name in ("Cargo.toml", "Cargo.lock", RELEASE_MANIFEST):
            (projected / name).write_text(candidate[name])
        after = metadata(projected)
        if (projected / "Cargo.lock").read_text() != candidate["Cargo.lock"]:
            raise ValueError("release Cargo.lock changed during metadata")
        if normalized_graph(audit, before, root, set(versions)) != normalized_graph(audit, after, projected, set(versions)):
            raise ValueError("release changed the resolved feature or dependency graph")
        new = dict(policy)
        new.update(lock_sha256=hashlib.sha256((projected / "Cargo.lock").read_bytes()).hexdigest(),
                   source_sha256=audit.source_fingerprint(projected),
                   graph_sha256=audit.graph_fingerprint(after, projected))
        audit.validate_libz_server_builds(projected, new)
        audit.validate_context(projected, new, metadata(projected))
    validate_committed_review_inputs(root)
    audit.validate_context(root, policy, metadata(root))
    old_hashes = {key: policy[key] for key in FINGERPRINTS}
    current_hashes = {key: branch_policy.get(key) for key in FINGERPRINTS}
    new_hashes = {key: new[key] for key in FINGERPRINTS}
    if current_hashes != old_hashes and current_hashes != new_hashes:
        raise ValueError("release contains an unreviewed native audit fingerprint")
    return json.dumps(new, indent=2) + "\n"

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
    base = git_output(root, "rev-parse", "HEAD")
    main_ref = f"{endpoint}/git/ref/heads/main"
    if api("GET", main_ref)["object"]["sha"] != base:
        raise ValueError("release tooling checkout differs from current main")
    validate_release_tree(api, endpoint, base, head)
    contents = {}
    for path in ("Cargo.toml", "Cargo.lock", RELEASE_MANIFEST, REVIEW_PATH):
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
    updated = dict(contents)
    updated["Cargo.toml"], updated["Cargo.lock"] = align_versions(contents["Cargo.toml"], contents["Cargo.lock"], versions)
    updated[REVIEW_PATH] = refresh_review(root, updated, versions, version)
    changes = [{"path": path, "mode": "100644", "type": "blob", "content": value}
               for path, value in updated.items() if value != contents[path]]
    if not changes:
        return False
    commit = api("GET", f"{endpoint}/git/commits/{head}")
    tree = api("POST", f"{endpoint}/git/trees", {"base_tree": commit["tree"]["sha"], "tree": changes})
    new_commit = api("POST", f"{endpoint}/git/commits", {
        "message": "chore(release): align versions and reviewed audit inputs",
        "tree": tree["sha"], "parents": [head]})
    current = api("GET", f"{endpoint}/pulls/{number}")
    if (current["state"] != "open" or api("GET", reference)["object"]["sha"] != head
            or api("GET", main_ref)["object"]["sha"] != base):
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
