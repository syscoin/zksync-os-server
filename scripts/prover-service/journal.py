"""Durable indexed journals without a lifetime limit on retained work."""

from collections.abc import MutableMapping, Sequence
import copy
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import weakref

import service as s

MAX_ROWS = 512
MAX_INLINE_EVENTS = 1024
MAX_INLINE_BYTES = 8 * 1024 * 1024
MAX_CACHE = 32
TERMINAL_ROWS = {"accepted", "accepted_elsewhere", "reverted", "confirmed", "settled", "superseded",
                 "returned", "expired", "obsolete", "rejected", "unexported_pick_abandoned", "no_job"}


class Database:
    def __init__(self, connection):
        self.connection = connection
        self._close = weakref.finalize(self, connection.close)

    def __getattr__(self, name):
        return getattr(self.connection, name)

    def close(self):
        if self.connection is not None:
            self._close()
            self.connection = None


def _decode(raw):
    return json.loads(raw, object_pairs_hook=s._unique,
                      parse_constant=lambda _: s.require(False, "nonfinite_json_value"))


def _connection(path):
    path = Path(path)
    s.require(not path.is_symlink(), "journal_database_symlink")
    if path.exists():
        s.require(path.is_file() and path.stat().st_mode & 0o077 == 0, "private_journal_database_required")
    else:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(descriptor)
    connection = sqlite3.connect(path, isolation_level=None)
    connection.execute("PRAGMA synchronous=FULL")
    connection.execute("PRAGMA journal_mode=DELETE")
    return Database(connection)


class SQLiteMap(MutableMapping):
    def __init__(self, connection, name, *, owns_connection=False):
        s.require(name.replace("_", "").isalnum(), "invalid_journal_table")
        self.connection, self.name, self.owns_connection = connection, name, owns_connection
        self.cache, self.original, self.deleted = {}, {}, set()
        connection.execute(f"CREATE TABLE IF NOT EXISTS {name} (position INTEGER PRIMARY KEY AUTOINCREMENT, "
                           "key TEXT UNIQUE NOT NULL, value BLOB NOT NULL, account TEXT, lane TEXT, batch TEXT, status TEXT)")
        connection.execute(f"CREATE INDEX IF NOT EXISTS {name}_batch ON {name}(lane,batch,position)")
        connection.execute(f"CREATE INDEX IF NOT EXISTS {name}_account ON {name}(account,status)")

    def __getitem__(self, key):
        if key in self.deleted:
            raise KeyError(key)
        if key not in self.cache:
            row = self.connection.execute(f"SELECT value FROM {self.name} WHERE key=?", (key,)).fetchone()
            if row is None:
                raise KeyError(key)
            self.cache[key] = _decode(row[0])
            self.original[key] = bytes(row[0])
        return self.cache[key]

    def __setitem__(self, key, value):
        self.deleted.discard(key)
        self.cache[key] = value

    def __delitem__(self, key):
        if key not in self:
            raise KeyError(key)
        self.cache.pop(key, None)
        self.original.pop(key, None)
        self.deleted.add(key)

    def __contains__(self, key):
        return key not in self.deleted and (key in self.cache or self.connection.execute(
            f"SELECT 1 FROM {self.name} WHERE key=?", (key,)).fetchone() is not None)

    def __iter__(self):
        for key, _ in self.items():
            yield key

    def __len__(self):
        count = self.connection.execute(f"SELECT COUNT(*) FROM {self.name}").fetchone()[0]
        count -= sum(self.connection.execute(f"SELECT 1 FROM {self.name} WHERE key=?", (key,)).fetchone()
                     is not None for key in self.deleted)
        count += sum(self.connection.execute(f"SELECT 1 FROM {self.name} WHERE key=?", (key,)).fetchone()
                     is None for key in self.cache)
        return count

    def items(self):
        seen = set(self.cache)
        for key, raw in self.connection.execute(f"SELECT key,value FROM {self.name} ORDER BY position"):
            if key not in self.deleted:
                yield key, self.cache[key] if key in self.cache else _decode(raw)
            seen.discard(key)
        for key in self.cache:
            if key in seen and key not in self.deleted:
                yield key, self.cache[key]

    def values(self):
        for _, value in self.items():
            yield value

    def query(self, clause, arguments, predicate):
        seen = set()
        for key, raw in self.connection.execute(
                f"SELECT key,value FROM {self.name} WHERE {clause} ORDER BY position", arguments):
            seen.add(key)
            value = self.cache.get(key) if key in self.cache else _decode(raw)
            if key not in self.deleted and predicate(value):
                yield key, value
        for key, value in self.cache.items():
            if key not in seen and key not in self.deleted and predicate(value):
                yield key, value

    def flush(self):
        for key in self.deleted:
            self.connection.execute(f"DELETE FROM {self.name} WHERE key=?", (key,))
        for key, value in self.cache.items():
            raw = s.canonical(value)
            s.require(len(raw) <= s.MAX_FILE, "journal_record_too_large")
            if self.original.get(key) == raw:
                continue
            assignment = value.get("assignment") if isinstance(value, dict) else None
            indexed = value if isinstance(value, dict) else {}
            batch = assignment.get("batch_number") if isinstance(assignment, dict) else None
            self.connection.execute(f"INSERT INTO {self.name}(key,value,account,lane,batch,status) VALUES(?,?,?,?,?,?) "
                "ON CONFLICT(key) DO UPDATE SET value=excluded.value,account=excluded.account,lane=excluded.lane,"
                "batch=excluded.batch,status=excluded.status", (key, raw, indexed.get("account"),
                indexed.get("lane"), f"{batch:020d}" if batch is not None else None, indexed.get("status")))

    def clear_pending(self):
        # Callers retain live row references across saves and native RPC responses.
        # Evict only old clean reads; live assignments stay resident until resolved.
        self.original = {key: s.canonical(value) for key, value in self.cache.items()}
        if self.owns_connection:
            self.cache.clear()
            self.original.clear()
        elif len(self.cache) > MAX_CACHE:
            for key in list(self.cache):
                if len(self.cache) <= MAX_CACHE:
                    break
                value = self.cache[key]
                if isinstance(value, dict) and "status" in value and value["status"] not in TERMINAL_ROWS:
                    continue
                self.cache.pop(key)
                self.original.pop(key, None)
        self.deleted.clear()

    def sync(self):
        self.connection.execute("BEGIN IMMEDIATE")
        try:
            self.flush()
            self.connection.execute("COMMIT")
        except BaseException:
            self.connection.execute("ROLLBACK")
            raise
        self.clear_pending()

    def close(self):
        if self.owns_connection:
            self.connection.close()


class SQLiteEvents(Sequence):
    def __init__(self, connection, name):
        self.connection, self.name, self.pending = connection, name, []
        connection.execute(f"CREATE TABLE IF NOT EXISTS {name}(sequence INTEGER PRIMARY KEY,value BLOB NOT NULL)")
        self.committed = connection.execute(f"SELECT COUNT(*) FROM {name}").fetchone()[0]

    def __len__(self):
        return self.committed + len(self.pending)

    def __getitem__(self, index):
        if isinstance(index, slice):
            return [self[number] for number in range(*index.indices(len(self)))]
        if index < 0:
            index += len(self)
        if not 0 <= index < len(self):
            raise IndexError(index)
        if index >= self.committed:
            return self.pending[index - self.committed]
        row = self.connection.execute(f"SELECT value FROM {self.name} WHERE sequence=?", (index,)).fetchone()
        s.require(row is not None, "journal_event_gap")
        return _decode(row[0])

    def __iter__(self):
        for _, raw in self.connection.execute(f"SELECT sequence,value FROM {self.name} ORDER BY sequence"):
            yield _decode(raw)
        yield from self.pending

    def append(self, event):
        self.pending.append(event)

    def flush(self):
        for index, event in enumerate(self.pending, self.committed):
            self.connection.execute(f"INSERT INTO {self.name}(sequence,value) VALUES(?,?)", (index, s.canonical(event)))

    def clear_pending(self):
        self.committed += len(self.pending)
        self.pending.clear()


class State(dict):
    database = None
    marker_pending = False


def _field(state, path):
    keys = path.split(".")
    target = state
    for key in keys[:-1]:
        target = target[key]
    return target, keys[-1]


def _metadata(state, map_fields, list_fields):
    result = dict(state)
    for path in (*map_fields, *list_fields):
        keys = path.split(".")
        target = result
        original = state
        for key in keys[:-1]:
            original = original[key]
            target[key] = dict(original)
            target = target[key]
        target[keys[-1]] = {} if path in map_fields else []
    result.pop("storage_version", None)
    return result


def _attach(state, connection, map_fields, list_fields):
    result = State(state)
    result.database = connection
    for path in map_fields:
        parent, key = _field(result, path)
        name = "map_" + path.replace(".", "_")
        exists = connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone()
        # Attaching a newly introduced optional map must not change a dry-run journal.
        parent[key] = SQLiteMap(connection, name) if exists else parent.get(key, {})
    for path in list_fields:
        if path == "audit.events" and "audit" not in result:
            continue
        parent, key = _field(result, path)
        parent[key] = SQLiteEvents(connection, "list_" + path.replace(".", "_"))
    return result


def _atomic(path, value):
    path = Path(path)
    raw = s.canonical(value)
    s.require(len(raw) <= s.MAX_FILE, "journal_metadata_too_large")
    descriptor, name = tempfile.mkstemp(prefix=".journal-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
        _sync_directory(path.parent)
    finally:
        Path(name).unlink(missing_ok=True)


def _sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def load(path, *, map_fields=("operations",), list_fields=()):
    path = Path(path)
    raw = s.read(path, private=True)
    marker = _decode(raw)
    database = path.with_name(path.name + ".sqlite3")
    if not database.exists():
        s.require(marker.get("storage_version") != 2, "journal_database_missing")
        return State(marker)
    connection = _connection(database)
    try:
        row = connection.execute("SELECT value FROM metadata WHERE key='state'").fetchone()
        s.require(row is not None, "journal_metadata_missing")
        if marker.get("storage_version") != 2:
            origin = connection.execute("SELECT value FROM metadata WHERE key='origin'").fetchone()
            s.require(origin is not None and origin[0] == hashlib.sha256(raw).hexdigest(),
                      "journal_migration_source_changed")
        state = _attach(_decode(row[0]), connection, map_fields, list_fields)
        state.marker_pending = marker.get("storage_version") != 2
        return state
    except BaseException:
        connection.close()
        raise


def save(path, state, *, map_fields=("operations",), list_fields=()):
    path = Path(path)
    connection = getattr(state, "database", None)
    if connection is None:
        migrate = any(len(_field(state, field)[0][_field(state, field)[1]]) >= MAX_ROWS for field in map_fields)
        migrate |= any(len(_field(state, field)[0][_field(state, field)[1]]) >= MAX_INLINE_EVENTS for field in list_fields)
        if not migrate:
            raw = s.canonical(state)
            if len(raw) <= MAX_INLINE_BYTES:
                _atomic(path, state)
                return
        descriptor, temporary = tempfile.mkstemp(prefix=".journal-db-", dir=path.parent)
        os.close(descriptor)
        connection = _connection(temporary)
        metadata = _metadata(state, map_fields, list_fields)
        connection.execute("CREATE TABLE metadata(key TEXT PRIMARY KEY,value BLOB NOT NULL)")
        replacement = _attach(copy.deepcopy(metadata), connection, map_fields, list_fields)
        for field in map_fields:
            parent, key = _field(replacement, field)
            if not isinstance(parent[key], SQLiteMap):
                parent[key] = SQLiteMap(connection, "map_" + field.replace(".", "_"))
            source, source_key = _field(state, field)
            parent[key].update(source[source_key])
        for field in list_fields:
            parent, key = _field(replacement, field)
            source, source_key = _field(state, field)
            for event in source[source_key]:
                parent[key].append(event)
        try:
            connection.execute("BEGIN IMMEDIATE")
            for field in (*map_fields, *list_fields):
                parent, key = _field(replacement, field)
                parent[key].flush()
            connection.execute("INSERT INTO metadata VALUES('state',?)", (s.canonical(metadata),))
            origin = hashlib.sha256(s.read(path, private=True)).hexdigest() if path.exists() else ""
            connection.execute("INSERT INTO metadata VALUES('origin',?)", (origin,))
            connection.execute("COMMIT")
            connection.close()
            database = path.with_name(path.name + ".sqlite3")
            s.require(not database.exists(), "journal_database_already_exists")
            os.replace(temporary, database)
            _sync_directory(path.parent)
            # The database commit becomes authoritative before publishing the small marker.
            # Recovery recognizes the exact old JSON if a crash interrupts marker publication.
            connection = _connection(database)
            for field in (*map_fields, *list_fields):
                parent, key = _field(replacement, field)
                parent[key].connection = connection
            state.clear()
            state.update(replacement)
            state.database = connection
            state.marker_pending = True
            for field in (*map_fields, *list_fields):
                parent, key = _field(state, field)
                parent[key].clear_pending()
            _atomic(path, {**metadata, "schema_version": 2, "storage_version": 2})
            state.marker_pending = False
            return
        except BaseException:
            if connection.connection is not None and connection.in_transaction:
                connection.execute("ROLLBACK")
            if getattr(state, "database", None) is not connection:
                connection.close()
            Path(temporary).unlink(missing_ok=True)
            raise
    metadata = _metadata(state, map_fields, list_fields)
    connection.execute("BEGIN IMMEDIATE")
    attached = []
    try:
        for field in map_fields:
            parent, key = _field(state, field)
            if not isinstance(parent[key], SQLiteMap):
                original = parent[key]
                replacement = SQLiteMap(connection, "map_" + field.replace(".", "_"))
                replacement.update(original)
                parent[key] = replacement
                attached.append((parent, key, original))
        for field in (*map_fields, *list_fields):
            parent, key = _field(state, field)
            parent[key].flush()
        connection.execute("UPDATE metadata SET value=? WHERE key='state'", (s.canonical(metadata),))
        connection.execute("COMMIT")
    except BaseException:
        connection.execute("ROLLBACK")
        for parent, key, original in attached:
            parent[key] = original
        raise
    for field in (*map_fields, *list_fields):
        parent, key = _field(state, field)
        parent[key].clear_pending()
    if state.marker_pending:
        _atomic(path, {**metadata, "schema_version": 2, "storage_version": 2})
        state.marker_pending = False


def disk_map(root, name):
    connection = _connection(Path(root) / (name + ".sqlite3"))
    # Audit replay is disposable; only successful independent checkpoints are retained.
    connection.execute("PRAGMA synchronous=OFF")
    connection.execute("PRAGMA journal_mode=MEMORY")
    return SQLiteMap(connection, name, owns_connection=True)


def range_items(mapping, lane, start, end):
    predicate = lambda value: value.get("lane") == lane and value.get("assignment") is not None \
        and start <= value["assignment"]["batch_number"] <= end
    if isinstance(mapping, SQLiteMap):
        yield from mapping.query("lane=? AND batch BETWEEN ? AND ?", (lane, f"{start:020d}", f"{end:020d}"), predicate)
    else:
        yield from ((key, value) for key, value in mapping.items() if predicate(value))


def last_batch(mapping, lane, batch):
    result = None
    for item in range_items(mapping, lane, batch, batch):
        result = item
    return result


def live_account(mapping, account, statuses):
    predicate = lambda value: value.get("account") == account and value.get("status") in statuses
    if isinstance(mapping, SQLiteMap):
        clause = "account=? AND status IN (" + ",".join("?" for _ in statuses) + ")"
        return next(mapping.query(clause, (account, *statuses), predicate), None) is not None
    return any(predicate(value) for value in mapping.values())


def status_items(mapping, statuses):
    predicate = lambda value: isinstance(value, dict) and value.get("status") in statuses
    if isinstance(mapping, SQLiteMap):
        clause = "status IN (" + ",".join("?" for _ in statuses) + ")"
        yield from mapping.query(clause, tuple(statuses), predicate)
    else:
        yield from ((key, value) for key, value in mapping.items() if predicate(value))


def peek(mapping, key, default=None):
    if not isinstance(mapping, SQLiteMap):
        return mapping.get(key, default)
    if key in mapping.deleted:
        return default
    if key in mapping.cache:
        return mapping.cache[key]
    row = mapping.connection.execute(f"SELECT value FROM {mapping.name} WHERE key=?", (key,)).fetchone()
    return _decode(row[0]) if row is not None else default
