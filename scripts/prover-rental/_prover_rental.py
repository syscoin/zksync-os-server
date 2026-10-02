"""Compatibility boundary for prover-owned rental tooling."""

import os
from pathlib import Path
import sys


def implementation(name):
    configured = os.environ.get("ZKSYNC_AIRBENDER_PROVER_DIR")
    repository = Path(configured).expanduser() if configured else Path(__file__).resolve().parents[3] / "zksync-airbender-prover"
    directory = repository.resolve() / "scripts" / "prover-rental"
    source = directory / name
    if directory == Path(__file__).resolve().parent or not source.is_file():
        raise RuntimeError(
            "Rental tooling moved to zksync-airbender-prover; set "
            "ZKSYNC_AIRBENDER_PROVER_DIR to that repository checkout."
        )
    sys.path.insert(0, str(directory))
    # Service duties still use this server checkout's contract-specific keeper.
    os.environ.setdefault("ZKSYNC_OS_SERVER_DIR", str(Path(__file__).resolve().parents[2]))
    return source


def run(name, namespace):
    source = implementation(name)
    namespace["__file__"] = str(source)
    exec(compile(source.read_bytes(), str(source), "exec"), namespace)
