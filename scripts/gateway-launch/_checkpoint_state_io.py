import json
import os
import stat
import tempfile
from pathlib import Path


def atomic_write_json(path: Path, value: object) -> None:
    """Durably replace an owner-only checkpoint JSON file."""
    parent = path.parent
    parent_info = os.lstat(parent)
    if stat.S_ISLNK(parent_info.st_mode) or not stat.S_ISDIR(parent_info.st_mode):
        raise SystemExit(f"checkpoint state directory is unsafe: {parent}")
    if parent_info.st_uid != os.geteuid():
        raise SystemExit(f"checkpoint state directory has wrong owner: {parent}")

    temporary_path = None
    try:
        fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=parent)
        temporary_path = Path(temporary_name)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(value, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary_path, path)
        temporary_path = None
        directory_fd = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
