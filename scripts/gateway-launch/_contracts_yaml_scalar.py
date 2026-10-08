#!/usr/bin/env python3
"""SYSCOIN: Persist only the attested gas-tank scalar without reserializing bytecode."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import re
import secrets
import stat
import sys

import yaml
from yaml.nodes import MappingNode, ScalarNode, SequenceNode
from yaml.tokens import AliasToken, AnchorToken, DocumentEndToken

FIELD = "zksys_gas_tank_addr"
MAX_BYTES = 8 * 1024 * 1024


def document(text: str):
    # SYSCOIN: BaseLoader keeps huge hexadecimal scalars as strings. Aliases,
    # duplicate keys and custom tags cannot unambiguously identify one edit.
    if any(isinstance(t, (AliasToken, AnchorToken, DocumentEndToken)) for t in yaml.scan(text)):
        raise ValueError("unsupported YAML alias, anchor or document end")
    root = yaml.compose(text, Loader=yaml.BaseLoader)
    if not isinstance(root, MappingNode) or root.flow_style:
        raise ValueError("contracts YAML must be a block mapping")

    def project(node):
        if isinstance(node, ScalarNode) and node.tag == "tag:yaml.org,2002:str":
            return node.value
        if isinstance(node, SequenceNode) and node.tag == "tag:yaml.org,2002:seq":
            return [project(child) for child in node.value]
        if isinstance(node, MappingNode) and node.tag == "tag:yaml.org,2002:map":
            result = {}
            for key, value in node.value:
                if not isinstance(key, ScalarNode) or key.tag != "tag:yaml.org,2002:str" or key.value in result:
                    raise ValueError("duplicate or complex YAML key")
                result[key.value] = project(value)
            return result
        raise ValueError("unsupported YAML node or tag")

    return root, project(root)


def updated_bytes(raw: bytes, address: str) -> bytes:
    address = address.strip().lower()
    if not re.fullmatch(r"0x[0-9a-f]{40}", address) or int(address[2:], 16) < 1 << 16:
        raise ValueError("gas tank must be a nonreserved 20-byte hex address")
    text = raw.decode("utf-8")
    root, before = document(text)
    pairs = {key.value: (key, value) for key, value in root.value}
    newline = "\r\n" if "\r\n" in text else "\n"
    value_text = json.dumps(address)
    if "l2" not in pairs:
        separator = "" if text.endswith(("\n", "\r")) else newline
        result = text + separator + "l2:" + newline + "  " + FIELD + ": " + value_text + newline
    else:
        key, l2 = pairs["l2"]
        if not isinstance(l2, MappingNode):
            raise ValueError("l2 must be a mapping")
        fields = {k.value: v for k, v in l2.value}
        if FIELD in fields:
            scalar = fields[FIELD]
            if not isinstance(scalar, ScalarNode) or scalar.style not in (None, "'", '"') or not scalar.value:
                raise ValueError("gas tank field must be a simple scalar")
            if scalar.value == address:
                return raw
            start, end = scalar.start_mark.index, scalar.end_mark.index
            result = text[:start] + value_text + text[end:]
        elif l2.flow_style:
            # Empty flow mappings occur in fresh templates; avoid rewriting the
            # rest of a nonempty flow mapping or relocating its comments.
            start, end = l2.start_mark.index, l2.end_mark.index
            if l2.value or not re.fullmatch(r"\{[ \t]*\}", text[start:end]):
                raise ValueError("unsupported nonempty flow l2 mapping")
            result = text[:end - 1] + FIELD + ": " + value_text + text[end - 1:]
        else:
            end = text.find("\n", key.end_mark.index)
            if end < 0 or not re.fullmatch(r"[ \t]*:[ \t]*(?:#[^\r\n]*)?\r?", text[key.end_mark.index:end]):
                raise ValueError("unsupported l2 header")
            indent = " " * (key.start_mark.column + 2)
            result = text[:end + 1] + indent + FIELD + ": " + value_text + newline + text[end + 1:]
    expected = copy.deepcopy(before)
    expected.setdefault("l2", {})[FIELD] = address
    if document(result)[1] != expected:
        raise ValueError("edit changed more than the gas tank field")
    return result.encode("utf-8")


def identity(s):
    return tuple(getattr(s, k) for k in ("st_dev", "st_ino", "st_uid", "st_gid", "st_mode", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns"))


def persist(path: Path, address: str) -> bool:
    path = path.absolute()
    parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    temporary = None
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
        try:
            before = os.fstat(fd)
            mode = stat.S_IMODE(before.st_mode)
            if not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid() or before.st_nlink != 1 or mode & 0o133 or mode & 0o600 != 0o600 or not 0 < before.st_size <= MAX_BYTES:
                raise ValueError("contracts YAML must be an owned protected regular file")
            raw = b""
            while len(raw) < before.st_size:
                part = os.read(fd, min(65536, before.st_size - len(raw)))
                if not part:
                    raise ValueError("contracts YAML changed during read")
                raw += part
            if identity(before) != identity(os.fstat(fd)):
                raise ValueError("contracts YAML changed during read")
        finally:
            os.close(fd)
        updated = updated_bytes(raw, address)
        if identity(before) != identity(os.stat(path.name, dir_fd=parent, follow_symlinks=False)):
            raise ValueError("contracts YAML identity changed")
        if updated == raw:
            return False
        temporary = "." + path.name + ".gas-tank-" + secrets.token_hex(8)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
        try:
            os.fchown(fd, -1, before.st_gid)
            os.fchmod(fd, mode)
            with os.fdopen(fd, "wb", closefd=False) as stream:
                stream.write(updated)
                stream.flush()
                os.fsync(fd)
        finally:
            os.close(fd)
        if identity(before) != identity(os.stat(path.name, dir_fd=parent, follow_symlinks=False)):
            raise ValueError("contracts YAML identity changed before replace")
        # SYSCOIN: Only this scalar's token/line is changed, and replacement is
        # atomic under the caller's normal launch lock; no whole-file YAML dump.
        os.replace(temporary, path.name, src_dir_fd=parent, dst_dir_fd=parent)
        temporary = None
        os.fsync(parent)
        return True
    finally:
        if temporary is not None:
            os.unlink(temporary, dir_fd=parent)
        os.close(parent)


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise ValueError("usage: _contracts_yaml_scalar.py CONTRACTS_YAML GAS_TANK_ADDRESS")
        persist(Path(sys.argv[1]), sys.argv[2])
    except (OSError, ValueError, yaml.YAMLError) as exc:
        raise SystemExit("gas-tank config persistence refused: " + str(exc))
