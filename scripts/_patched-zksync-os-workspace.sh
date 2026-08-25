# SYSCOIN: Shared preparation for builds that apply the checked-in downstream patch to the
# immutable official final zksync-os revision selected by Cargo.lock. Callers
# must source gateway-launch/_common.sh first and set ZKSYNC_OS_SERVER_PATH,
# GATEWAY_DIR, WORKSPACE_NAME, and ZKSYNC_OS_GIT_URL.

# SYSCOIN: Exact source tree produced by the reviewed final-v0.4.0 downstream patch.
SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE="20dc217bbd535877f600df88bd7e2966d3d9b43a"

extract_zksync_os_dependency_field() {
  local dependency_alias="$1"
  local field="$2"
  python3 - "${ZKSYNC_OS_SERVER_PATH}/Cargo.toml" "${dependency_alias}" "${field}" <<'PY'
import re
import sys
from pathlib import Path


def inline_table(text: str, alias: str) -> str:
    match = re.search(rf"(?m)^{re.escape(alias)}\s*=\s*\{{", text)
    if match is None:
        raise SystemExit(f"failed to locate canonical dependency {alias} in Cargo.toml")
    cursor = match.end() - 1
    depth = 0
    in_string = False
    escaped = False
    while cursor < len(text):
        char = text[cursor]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[match.start() : cursor + 1]
        cursor += 1
    raise SystemExit(f"unterminated inline table for {alias} in Cargo.toml")


text = Path(sys.argv[1]).read_text(encoding="utf-8")
alias = sys.argv[2]
field = sys.argv[3]
entry = inline_table(text, alias)
match = re.search(rf"\b{re.escape(field)}\s*=\s*\"([^\"]+)\"", entry)
if match is None:
    raise SystemExit(f"failed to locate {field} for canonical dependency {alias}")
print(match.group(1))
PY
}

extract_zksync_os_tag() {
  extract_zksync_os_dependency_field "${1:-zk_os_forward_system}" tag
}

extract_zksync_os_git_url() {
  extract_zksync_os_dependency_field "${1:-zk_os_forward_system}" git
}

require_official_zksync_os_source() {
  local dependency_alias="$1"
  local git_url="$2"
  case "${git_url%/}" in
  https://github.com/matter-labs/zksync-os | https://github.com/matter-labs/zksync-os.git)
    ;;
  *)
    gl_die "${dependency_alias} must resolve from official matter-labs/zksync-os, got ${git_url}"
    ;;
  esac
}

extract_locked_rev() {
  python3 - "${ZKSYNC_OS_SERVER_PATH}/Cargo.lock" "$1" "$2" <<'PY'
import re
import sys
from pathlib import Path


def normalize(url: str) -> str:
    return url.rstrip("/").removesuffix(".git")


text = Path(sys.argv[1]).read_text(encoding="utf-8")
expected_url = normalize(sys.argv[2])
expected_tag = sys.argv[3]
revisions = {
    revision
    for url, tag, revision in re.findall(
        r'git\+([^"?]+)\?tag=([^"#]+)#([0-9a-f]{40})', text
    )
    if normalize(url) == expected_url and tag == expected_tag
}
if len(revisions) != 1:
    raise SystemExit(
        f"expected one locked revision for {sys.argv[2]} tag {expected_tag}, "
        f"found {sorted(revisions)}"
    )
print(revisions.pop())
PY
}

checkout_locked_base() {
  local checkout_path="$1"
  local locked_rev="$2"
  local os_tag="$3"

  if ! git -C "${checkout_path}" cat-file -e "${locked_rev}^{commit}" 2>/dev/null; then
    git -C "${checkout_path}" fetch --no-tags "${ZKSYNC_OS_GIT_URL}" "refs/tags/${os_tag}" >/dev/null || \
      gl_die "failed to fetch locked zksync-os tag ${os_tag} from ${ZKSYNC_OS_GIT_URL}"
  fi
  git -C "${checkout_path}" cat-file -e "${locked_rev}^{commit}" 2>/dev/null || \
    gl_die "locked zksync-os revision ${locked_rev} is unavailable in ${checkout_path}"
  [ -z "$(git -C "${checkout_path}" status --porcelain)" ] || \
    gl_die "zksync-os checkout has local changes: ${checkout_path}"

  git -C "${checkout_path}" checkout --detach "${locked_rev}" >/dev/null
  [ "$(git -C "${checkout_path}" rev-parse HEAD)" = "${locked_rev}" ] || \
    gl_die "zksync-os checkout did not resolve to locked revision ${locked_rev}"
}

prepare_zksync_os_checkout() {
  if [ "$#" -ne 3 ]; then
    gl_die "prepare_zksync_os_checkout requires dependency alias, applicator, and optional dev path"
  fi

  local dependency_alias="$1"
  local applicator="$2"
  local dev_path="$3"
  local os_tag os_git_url locked_rev os_root os_path repo_root base_date patched_rev patched_tree

  case "${WORKSPACE_NAME}" in
  "" | *[!A-Za-z0-9._-]*) gl_die "invalid workspace name: ${WORKSPACE_NAME}" ;;
  esac
  [ -f "${applicator}" ] || gl_die "zksync-os patch applicator is missing: ${applicator}"

  os_tag="$(extract_zksync_os_tag "${dependency_alias}")"
  os_git_url="$(extract_zksync_os_git_url "${dependency_alias}")"
  require_official_zksync_os_source "${dependency_alias}" "${os_git_url}"
  locked_rev="$(extract_locked_rev "${os_git_url}" "${os_tag}")"
  git check-ref-format "refs/tags/${os_tag}" >/dev/null 2>&1 || \
    gl_die "invalid locked zksync-os tag: ${os_tag}"

  if [ -n "${dev_path}" ]; then
    if [ "${ALLOW_SHARED_ZKSYNC_OS_DEV_PATH:-false}" != "true" ]; then
      gl_die "ZKSYNC_OS_DEV_PATH is a shared mutable checkout; unset it for isolated builds or set ALLOW_SHARED_ZKSYNC_OS_DEV_PATH=true for local development only"
    fi
    os_path="${dev_path}"
    repo_root="$(git -C "${os_path}" rev-parse --show-toplevel 2>/dev/null)" || \
      gl_die "ZKSYNC_OS_DEV_PATH is not a git repository root: ${os_path}"
    [ "$(cd "${os_path}" && pwd -P)" = "$(cd "${repo_root}" && pwd -P)" ] || \
      gl_die "ZKSYNC_OS_DEV_PATH is not the repository root: ${os_path}"
  else
    os_root="${GATEWAY_DIR}/.gateway-launch/zksync-os/${WORKSPACE_NAME}/canonical"
    os_path="${os_root}/${locked_rev}"
    if [ -e "${os_path}" ] && [ ! -d "${os_path}/.git" ]; then
      gl_die "zksync-os checkout path exists but is not a git repository: ${os_path}"
    fi
    if [ ! -d "${os_path}/.git" ]; then
      mkdir -p "${os_root}"
      git clone "${ZKSYNC_OS_GIT_URL}" "${os_path}"
    fi
  fi

  checkout_locked_base "${os_path}" "${locked_rev}" "${os_tag}"
  bash "${applicator}" "${os_path}"
  git -C "${os_path}" add --all
  patched_tree="$(git -C "${os_path}" write-tree)"
  # SYSCOIN: This is the final build-boundary attestation. Never commit or compile
  # an applicator postimage containing unrelated, partial, or concurrently added files.
  [ "${patched_tree}" = "${SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE}" ] || \
    gl_die "wrong patched zksync-os tree: expected=${SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE} actual=${patched_tree}"
  if ! git -C "${os_path}" diff --cached --quiet; then
    base_date="$(git -C "${os_path}" show -s --format=%cI "${locked_rev}")"
    GIT_AUTHOR_DATE="${base_date}" GIT_COMMITTER_DATE="${base_date}" \
      git -C "${os_path}" -c user.name="gateway-launch" -c user.email="gateway-launch@local" \
      commit -m "gateway-launch canonical local Syscoin patch" >/dev/null
  fi
  [ -z "$(git -C "${os_path}" status --porcelain)" ] || \
    gl_die "patched zksync-os checkout is not clean: ${os_path}"

  patched_rev="$(git -C "${os_path}" rev-parse HEAD)"
  patched_tree="$(git -C "${os_path}" rev-parse 'HEAD^{tree}')"
  [ "${patched_tree}" = "${SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE}" ] || \
    gl_die "committed zksync-os tree drifted before build: expected=${SYSCOIN_EXPECTED_ZKSYNC_OS_PATCHED_TREE} actual=${patched_tree}"
  if [ "${patched_rev}" != "${locked_rev}" ]; then
    [ "$(git -C "${os_path}" rev-parse HEAD^)" = "${locked_rev}" ] || \
      gl_die "local patch commit is not based directly on locked revision ${locked_rev}"
  fi
  # The original tag is rebound only inside this disposable local repository so
  # Cargo can resolve the rewritten file:// dependency. Nothing is pushed.
  git -C "${os_path}" tag -f "${os_tag}" "${patched_rev}" >/dev/null
  [ "$(git -C "${os_path}" rev-parse "refs/tags/${os_tag}^{commit}")" = "${patched_rev}" ] || \
    gl_die "failed to bind local tag ${os_tag} to patched revision ${patched_rev}"
  printf '%s\n' "${os_path}"
}

prepare_run_workspace() {
  if [ "$#" -ne 6 ]; then
    gl_die "prepare_run_workspace requires run path and canonical source metadata"
  fi

  local run_path="$1"
  local os_path="$2"
  local os_tag="$3"
  local os_git_url="$4"
  local locked_rev="$5"
  local patched_rev="$6"

  python3 - \
    "${ZKSYNC_OS_SERVER_PATH}" "${run_path}" "${os_path}" "${os_tag}" \
    "${os_git_url}" "${locked_rev}" "${patched_rev}" <<'PY'
import re
import shutil
import sys
from pathlib import Path


def normalize(url: str) -> str:
    return url.rstrip("/").removesuffix(".git")


def assignment_spans(text: str) -> list[tuple[str, int, int]]:
    spans: list[tuple[str, int, int]] = []
    for match in re.finditer(r"(?m)^([A-Za-z0-9_]+)\s*=\s*\{", text):
        alias = match.group(1)
        cursor = match.end() - 1
        depth = 0
        in_string = False
        escaped = False
        while cursor < len(text):
            char = text[cursor]
            if in_string:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    in_string = False
            elif char == '"':
                in_string = True
            elif char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    spans.append((alias, match.start(), cursor + 1))
                    break
            cursor += 1
        else:
            raise SystemExit(f"unterminated inline table for {alias} in Cargo.toml")
    return spans


def rewrite_canonical_dependencies(
    text: str, expected_url: str, expected_tag: str, local_url: str
) -> tuple[str, list[str]]:
    rewritten_aliases: list[str] = []
    # Work backwards so replacing one inline table does not invalidate later
    # spans. Every direct dependency on the exact canonical tag is rewritten;
    # older tags remain byte-for-byte untouched.
    for alias, start, end in reversed(assignment_spans(text)):
        entry = text[start:end]
        git_match = re.search(r'\bgit\s*=\s*"([^"]+)"', entry)
        tag_match = re.search(r'\btag\s*=\s*"([^"]+)"', entry)
        if git_match is None or tag_match is None:
            continue
        if (
            normalize(git_match.group(1)) != normalize(expected_url)
            or tag_match.group(1) != expected_tag
        ):
            continue
        entry, count = re.subn(
            r'(\bgit\s*=\s*)"[^"]+"',
            lambda match: f'{match.group(1)}"{local_url}"',
            entry,
            count=1,
        )
        if count != 1:
            raise SystemExit(f"failed to rewrite canonical dependency {alias}")
        text = text[:start] + entry + text[end:]
        rewritten_aliases.append(alias)
    return text, rewritten_aliases


def rewrite_lock(
    text: str,
    expected_url: str,
    expected_tag: str,
    expected_locked_rev: str,
    local_url: str,
    patched_rev: str,
) -> tuple[str, int]:
    pattern = re.compile(
        r"git\+(?P<url>[^\"?#)\s]+)\?tag=(?P<tag>[^\"#)\s]+)"
        r"(?P<revision>#[0-9a-f]{40})?"
    )
    count = 0

    def replace(match: re.Match[str]) -> str:
        nonlocal count
        if (
            normalize(match.group("url")) != normalize(expected_url)
            or match.group("tag") != expected_tag
        ):
            return match.group(0)
        revision = match.group("revision")
        if revision is not None and revision[1:] != expected_locked_rev:
            raise SystemExit(
                f"Cargo.lock revision {revision[1:]} does not match "
                f"canonical locked revision {expected_locked_rev}"
            )
        count += 1
        patched_suffix = f"#{patched_rev}" if revision is not None else ""
        return f"git+{local_url}?tag={expected_tag}{patched_suffix}"

    return pattern.sub(replace, text), count


# SYSCOIN: Reconcile only dependency edges that the reviewed guest patch actually changes.
def apply_checked_patch_lock_delta(
    text: str, local_url: str, expected_tag: str, patched_rev: str
) -> str:
    """Apply the exact dependency-edge delta from the reviewed Syscoin patch.

    The checked-in server lock describes the official, unpatched v0.4 source.
    The patch adds `sha2` to basic_system for the portable SLH-DSA verifier.
    Keep this narrower than running Cargo unlocked; all unchanged dependency
    edges must remain byte-for-byte aligned with the checked-in lock.
    """
    package_re = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]\n|\Z)")
    expected_source = (
        f'source = "git+{local_url}?tag={expected_tag}#{patched_rev}"'
    )

    def package_block(name: str) -> tuple[int, int, str]:
        matches = [
            (match.start(), match.end(), match.group(0))
            for match in package_re.finditer(text)
            if f'name = "{name}"' in match.group(0)
            and expected_source in match.group(0)
        ]
        if len(matches) != 1:
            raise SystemExit(
                f"expected exactly one canonical {name} package in rewritten "
                f"Cargo.lock, found {len(matches)}"
            )
        return matches[0]

    start, end, block = package_block("basic_system")
    dependency = ' "sha2 0.10.9",\n'
    if not re.search(
        r'(?ms)^\[\[package\]\]\nname = "sha2"\nversion = "0\.10\.9"\n', text
    ):
        raise SystemExit("Cargo.lock does not contain the required sha2 0.10.9 package")
    if dependency not in block:
        anchor = ' "storage_models",\n'
        if block.count(anchor) != 1:
            raise SystemExit(
                "canonical basic_system lock entry is missing the expected "
                "storage_models anchor"
            )
        block = block.replace(anchor, dependency + anchor, 1)
        text = text[:start] + block + text[end:]

    return text


source = Path(sys.argv[1]).resolve()
target = Path(sys.argv[2]).resolve()
os_path = Path(sys.argv[3]).resolve()
os_tag, os_git_url, locked_rev, patched_rev = sys.argv[4:8]
if normalize(os_git_url) != "https://github.com/matter-labs/zksync-os":
    raise SystemExit(f"canonical source is not official matter-labs/zksync-os: {os_git_url}")
for label, revision in (("locked", locked_rev), ("patched", patched_rev)):
    if re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        raise SystemExit(f"{label} revision is not a full Git object ID: {revision}")

if target.exists():
    shutil.rmtree(target)


def ignore(_dir: str, names: list[str]) -> set[str]:
    blocked = {".git", "target", ".cursor", ".gateway-launch"}
    return {name for name in names if name in blocked}


# SYSCOIN: The run workspace is recreated while its Cargo target directory is
# intentionally retained. Do not preserve source mtimes here: Cargo's freshness
# checks could otherwise reuse metadata compiled from a previous source snapshot
# (including after a security-relevant API change) merely because the copied
# file's original mtime predates the cached artifact.
shutil.copytree(source, target, ignore=ignore, copy_function=shutil.copy)
local_url = os_path.as_uri()
cargo_toml = target / "Cargo.toml"
original_toml = cargo_toml.read_text(encoding="utf-8")
for alias, start, end in assignment_spans(original_toml):
    entry = original_toml[start:end]
    git_match = re.search(r'\bgit\s*=\s*"([^"]+)"', entry)
    tag_match = re.search(r'\btag\s*=\s*"([^"]+)"', entry)
    if (
        git_match is not None
        and normalize(git_match.group(1)) == normalize(os_git_url)
        and (tag_match is None or tag_match.group(1) != os_tag)
    ):
        raise SystemExit(f"noncanonical zksync-os dependency remains: {alias}")

text, rewritten_aliases = rewrite_canonical_dependencies(
    original_toml, os_git_url, os_tag, local_url
)
expected_aliases = {
    "zk_os_forward_system",
    "zk_ee",
    "zk_os_basic_system",
    "zk_os_api",
    "zk_os_evm_interpreter",
}
if set(rewritten_aliases) != expected_aliases:
    raise SystemExit(
        "canonical zksync-os dependency set mismatch: "
        f"expected {sorted(expected_aliases)}, got {sorted(rewritten_aliases)}"
    )
cargo_toml.write_text(text, encoding="utf-8")

cargo_lock = target / "Cargo.lock"
if not cargo_lock.is_file():
    raise SystemExit("Cargo.lock is required for an immutable patched workspace")
original_lock = cargo_lock.read_text(encoding="utf-8")
for url, tag in re.findall(
    r"git\+([^\"?#)\s]+)\?tag=([^\"#)\s]+)", original_lock
):
    if normalize(url) == normalize(os_git_url) and tag != os_tag:
        raise SystemExit(f"noncanonical zksync-os tag remains in Cargo.lock: {tag}")

lock_text, count = rewrite_lock(
    original_lock,
    os_git_url,
    os_tag,
    locked_rev,
    local_url,
    patched_rev,
)
if count == 0:
    raise SystemExit("canonical zksync-os source was not rewritten in Cargo.lock")
lock_text = apply_checked_patch_lock_delta(
    lock_text, local_url, os_tag, patched_rev
)
cargo_lock.write_text(lock_text, encoding="utf-8")
PY
}

clear_multivm_build_script_cache() {
  local target_dir="$1"
  # prepare_run_workspace recreates lib/multivm/apps, but Cargo may reuse an
  # old build-script output that points include_bytes! at deleted files.
  rm -rf "${target_dir}"/debug/build/zksync_os_multivm-* \
    "${target_dir}"/release/build/zksync_os_multivm-*
}
