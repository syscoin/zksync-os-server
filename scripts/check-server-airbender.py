#!/usr/bin/env python3
"""Attest the server's immutable Airbender fork source before and after Cargo.

The optional local repository supplies only independent Git object attestation.
Cargo still resolves the fork URL and revision pinned in Cargo.toml and Cargo.lock.
"""

import argparse
from collections import Counter
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
try:
    import tomllib
except ImportError:
    import tomli as tomllib


ROOT = Path(__file__).resolve().parents[1]
FORK = "https://github.com/syscoin/zksync-airbender"
COMMIT = "e8f380777b7e90388c7323d6fd80a3b58c589f17"
PARENT = "03454c7a41053a4b88bb421e97fb9efe893a92f5"
TREE = "799e018339df40d0e456ba3fa0fc768ce0691853"
SOURCE = f"git+{FORK}?rev={COMMIT}#{COMMIT}"
LEGACY = ("git+https://github.com/matter-labs/zksync-airbender"
          "?rev=73d69b5#73d69b5346b3c2350fa104a56ec4df78840cea99")
OLD_SOURCE = ("git+https://github.com/matter-labs/zksync-airbender"
              f"?tag=v0.6.0-rc.2#{PARENT}")
SELECTED_CLOSURE = "2fefe8f11e55ae3a85a4656bd973dc700507db82434cc1728aa8aa3a9b605486"
LEGACY_CLOSURE = "30ca22d2f5c800b589c2ba2fe6912739a58e4a07e6d2cf568ce5b2a87301d56c"
SELECTED_REFS = {"common_constants": 19, "field": 18, "blake2s_u32": 5,
                 "riscv_transpiler": 6, "worker": 6, "cs": 3}
LEGACY_REFS = {"common_constants": 3, "field": 2, "riscv_transpiler": 1,
               "blake2s_u32": 1, "cs": 1, "worker": 1}
RECORD = ".server-airbender-attestation.json"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def git(*args):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    return subprocess.check_output(["git", *map(str, args)], env=env, text=True).strip()


def digest(path):
    require(path.is_file() and not path.is_symlink(), f"missing source input: {path}")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inputs(directory):
    return {name: digest(directory / name) for name in ("Cargo.toml", "Cargo.lock")}


def semantic_hash(rows):
    data = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(data).hexdigest()


def check_manifest(path):
    dependencies = tomllib.loads(path.read_text())["workspace"]["dependencies"]
    expected = {
        "execution_utils": {"git": FORK, "rev": COMMIT, "features": ["verifier_binaries"]},
        "riscv_transpiler": {"git": FORK, "rev": COMMIT},
        "verifier_common": {"git": FORK, "rev": COMMIT},
    }
    for name, row in expected.items():
        require(dependencies.get(name) == row, f"wrong direct Airbender pin: {name}")
    sources = [row for row in dependencies.values() if isinstance(row, dict)
               and "zksync-airbender" in row.get("git", "")]
    require(len(sources) == 3, "additional Airbender manifest source")


def check_lock(path):
    packages = tomllib.loads(path.read_text())["package"]
    selected = [row for row in packages if row.get("source") == SOURCE]
    legacy = [row for row in packages if row.get("source") == LEGACY]
    require(len(selected) == 44 and len(legacy) == 6, "wrong Airbender lock closure size")
    names = {row["name"] for row in selected}
    require(all(row.get("source") in {SOURCE, LEGACY} for row in packages
                if row["name"] in names and row["version"] == "0.1.0"),
            "aliased Airbender package source")
    normalized = copy.deepcopy(selected)
    for row in normalized:
        row["source"] = OLD_SOURCE
        if "dependencies" in row:
            row["dependencies"] = [
                dep.replace(SOURCE.split("#")[0], OLD_SOURCE.split("#")[0])
                for dep in row["dependencies"]
            ]
    require(semantic_hash(normalized) == SELECTED_CLOSURE, "selected Airbender closure drift")
    require(semantic_hash(legacy) == LEGACY_CLOSURE, "legacy Airbender closure drift")
    refs = Counter(dep for row in packages for dep in row.get("dependencies", []))
    for source, expected in ((SOURCE, SELECTED_REFS), (LEGACY, LEGACY_REFS)):
        prefix = source.split("#")[0]
        actual = {dep: count for dep, count in refs.items() if prefix in dep}
        require(actual == {f"{name} 0.1.0 ({prefix})": count
                           for name, count in expected.items()},
                "Airbender qualified references drift")
    for row in packages:
        value = row.get("source", "")
        if "zksync-airbender" in value:
            require(value in {SOURCE, LEGACY}, "unknown Airbender package source")


def check_graph(directory):
    check_manifest(directory / "Cargo.toml")
    check_lock(directory / "Cargo.lock")


def check_fork_objects(repository):
    require(repository.is_dir() and not repository.is_symlink(), "missing Airbender object store")
    require(git("--git-dir", repository, "rev-parse", "--is-bare-repository") == "true",
            "invalid Airbender object store")
    require(git("--git-dir", repository, "rev-parse", COMMIT) == COMMIT,
            "Airbender commit drift")
    require(git("--git-dir", repository, "rev-parse", f"{COMMIT}^{{tree}}") == TREE,
            "Airbender tree drift")
    require(git("--git-dir", repository, "rev-parse", f"{COMMIT}^") == PARENT,
            "Airbender parent drift")


def check_local_source(repository):
    require(git("-C", repository, "rev-parse", "--show-toplevel") == str(repository),
            "local Airbender source must be a checkout root")
    require(git("-C", repository, "rev-parse", COMMIT) == COMMIT,
            "local Airbender commit unavailable")
    require(git("-C", repository, "rev-parse", f"{COMMIT}^{{tree}}") == TREE,
            "local Airbender tree drift")
    require(git("-C", repository, "rev-parse", f"{COMMIT}^") == PARENT,
            "local Airbender parent drift")


def check_directory(directory):
    path = directory.resolve(strict=True)
    require(directory == path and path.is_dir() and not path.is_symlink(),
            f"noncanonical directory: {directory}")
    return path


def verify(server, workspace):
    record = json.loads((workspace / RECORD).read_text())
    require(set(record) == {"server", "workspace", "attestation_repository", "attestation_kind",
                            "server_inputs", "workspace_inputs"},
            "invalid Airbender source record")
    require(record["server"] == str(server) and record["workspace"] == str(workspace),
            "Airbender source record route drift")
    require(record["server_inputs"] == inputs(server)
            and record["workspace_inputs"] == inputs(workspace),
            "Airbender manifest or lock drift")
    source = check_directory(Path(record["attestation_repository"]))
    if record["attestation_kind"] == "local":
        local = os.environ.get("SERVER_AIRBENDER_ATTESTATION_REPO")
        require(bool(local), "local Airbender source route missing")
        require(source == check_directory(Path(local)),
                "local Airbender source route drift")
        check_local_source(source)
    else:
        require(record["attestation_kind"] == "fetched"
                and source.parent == workspace.parent
                and source.name.startswith("server-airbender-"),
                "unrelated Airbender object store")
        check_fork_objects(source)
    check_graph(server)
    check_graph(workspace)


def prepare(server, workspace):
    require(server == ROOT and server != workspace, "wrong server workspace")
    require(not (workspace / RECORD).exists(), "Airbender source record already exists")
    check_graph(server)
    check_graph(workspace)
    before_server, before_workspace = inputs(server), inputs(workspace)
    local = os.environ.get("SERVER_AIRBENDER_ATTESTATION_REPO")
    if local:
        source = check_directory(Path(local))
        check_local_source(source)
        attestation_kind = "local"
    else:
        source = Path(tempfile.mkdtemp(prefix="server-airbender-", dir=workspace.parent)).resolve()
        git("init", "--quiet", "--bare", source)
        git("--git-dir", source, "fetch", "--quiet", "--depth=2", "--filter=blob:none",
            FORK, COMMIT)
        check_fork_objects(source)
        attestation_kind = "fetched"
    require(inputs(server) == before_server and inputs(workspace) == before_workspace,
            "Airbender graph changed during preparation")
    (workspace / RECORD).write_text(json.dumps({
        "server": str(server), "workspace": str(workspace),
        "attestation_repository": str(source), "attestation_kind": attestation_kind,
        "server_inputs": before_server, "workspace_inputs": before_workspace,
    }, indent=2) + "\n")
    verify(server, workspace)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--verify", action="store_true")
    arguments = parser.parse_args()
    try:
        server = check_directory(arguments.server)
        workspace = check_directory(arguments.workspace)
        (verify if arguments.verify else prepare)(server, workspace)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"server Airbender source check failed: {error}", file=sys.stderr)
        sys.exit(1)
