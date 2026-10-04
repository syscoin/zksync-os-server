"""SYSCOIN: Open a private launch log before re-executing with its descriptor."""

import os
import stat
import sys
from pathlib import Path


def open_private_log(path: Path) -> int:
    parent = path.absolute().parent
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(directory)
        # SYSCOIN: A readable parent is harmless; a writable one permits replacing the name.
        if info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise ValueError("launch log parent must be owned by the current user and not writable by others")
        flags = os.O_WRONLY | os.O_NONBLOCK | os.O_NOFOLLOW
        try:
            descriptor = os.open(path.name, flags, dir_fd=directory)
        except FileNotFoundError:
            descriptor = os.open(path.name, flags | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=directory)
        try:
            info = os.fstat(descriptor)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_uid != os.getuid()
                or info.st_nlink != 1
                or info.st_mode & 0o077
            ):
                raise ValueError("existing launch log must be a private, singly-linked regular file owned by the current user")
            os.ftruncate(descriptor, 0)
            return descriptor
        except BaseException:
            os.close(descriptor)
            raise
    finally:
        os.close(directory)


def main() -> None:
    if len(sys.argv) == 3 and sys.argv[1] == "--tee":
        # SYSCOIN: Write through the inherited descriptor, never a /dev/fd pathname reopen.
        with os.fdopen(int(sys.argv[2]), "wb", closefd=False) as output:
            while chunk := sys.stdin.buffer.read1(64 * 1024):
                output.write(chunk)
                output.flush()
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
        return
    if len(sys.argv) < 4:
        raise SystemExit("usage: _private_log.py LOG COMMAND ARG...")
    try:
        descriptor = open_private_log(Path(sys.argv[1]))
    except (OSError, ValueError) as error:
        raise SystemExit(f"cannot open private gateway launch log: {error}") from error
    os.set_inheritable(descriptor, True)
    os.environ["GATEWAY_LAUNCH_LOG_FD"] = str(descriptor)
    os.execvp(sys.argv[2], sys.argv[2:])


if __name__ == "__main__":
    main()
