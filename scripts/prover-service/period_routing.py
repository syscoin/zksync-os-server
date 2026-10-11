"""Choose immutable period journals using finalized registry time and enrollment."""

import copy
import os
from pathlib import Path
import re
import uuid

import enrollment
import journal
import service as s


def _state(store):
    state = journal.load(store.root / "state.json", list_fields=("audit.events",))
    try:
        return {key: copy.deepcopy(state[key]) for key in
                ("settings", "lanes", "endpoint", "period", "enrollment", "journal_id")}
    finally:
        if state.database is not None:
            state.database.close()


def _view(rpc, settings, anchor, signature, *values):
    if hasattr(rpc, "contract"):
        return rpc.contract(settings["registry"], signature, values, anchor)
    return s.raw_hex(rpc.call("eth_call", [{"to": settings["registry"],
        "data": s.cast("calldata", signature, *map(str, values))},
        {"blockHash": anchor, "requireCanonical": True}]))


class PinnedRpc:
    def __init__(self, rpc, block):
        self.rpc, self.block = rpc, block

    def call(self, method, params):
        if method == "eth_getBlockByNumber" and params == ["finalized", False]:
            return copy.deepcopy(self.block)
        return self.rpc.call(method, params)

    def contract(self, address, signature, values, anchor):
        s.require(anchor == self.block["hash"], "period_enrollment_anchor_changed")
        if hasattr(self.rpc, "contract"):
            return self.rpc.contract(address, signature, values, anchor)
        return s.raw_hex(self.rpc.call("eth_call", [{"to": address,
            "data": s.cast("calldata", signature, *map(str, values))},
            {"blockHash": anchor, "requireCanonical": True}]))


def _subscriptions(rpc, settings, period, anchor):
    view = lambda signature, *values: _view(rpc, settings, anchor, signature, *values)
    count = enrollment.fixed_word(view("friSubscriberCount(address,uint64)", settings["sequencer"], period))
    s.require(count <= 256, "enrollment_exceeds_dispatcher_capacity")
    subscriptions, accounts = [], set()
    for index in range(count):
        raw = view("friSubscriberAt(address,uint64,uint256)", settings["sequencer"], period, index)
        s.require(len(raw) == 32 and raw[:12] == bytes(12), "invalid_enrollment_account")
        account = s.nonzero("0x" + raw[12:].hex(), 20)
        s.require(account not in accounts, "duplicate_enrollment_account")
        accounts.add(account)
        eligible = enrollment.fixed_word(view("isEligibleFriSubscriber(address,address,uint64)", account,
                                             settings["sequencer"], period))
        s.require(eligible in (0, 1), "invalid_enrollment_eligibility")
        if not eligible:
            continue
        hashed = view("subscriptionAt(address,address,uint64)", account, settings["sequencer"], period)
        s.nonzero("0x" + hashed.hex())
        s.require(len(hashed) == 32, "invalid_subscription_hash")
        encoded = view("subscription(bytes32)", "0x" + hashed.hex())
        s.require(len(encoded) == 32 * len(s.SUBSCRIPTION), "invalid_stored_subscription")
        subscription = {}
        for index, (name, kind) in enumerate(s.SUBSCRIPTION):
            word = encoded[index * 32:(index + 1) * 32]
            if kind == "address":
                s.require(word[:12] == bytes(12), "invalid_subscription_address")
                subscription[name] = "0x" + word[12:].hex()
            else:
                subscription[name] = s.uint(int.from_bytes(word, "big"), int(kind[4:]))
        s.require(subscription["account"] == account and s.raw_hex(s.subscription_request(settings, subscription)["struct_hash"])
                  == hashed and s.encode_fields(s.SUBSCRIPTION, subscription) == encoded,
                  "stored_subscription_mismatch")
        # Canonical subscribe() already authenticated both historical consents, including ERC-1271.
        subscriptions.append({"subscription": subscription, "signature": "0x"})
    s.require(subscriptions, "period_has_no_eligible_subscribers")
    return subscriptions


def _existing(base_store, base, period, journal_id=None, phase=None, enrollment_block_hash=None):
    import dispatcher as d
    candidates = [base_store]
    parent = base_store.root / "periods"
    if parent.exists():
        s.require(not parent.is_symlink(), "period_directory_symlink")
        for path in sorted(parent.iterdir()):
            match = re.fullmatch(r"([0-9]+)-(bootstrap|service)", path.name)
            if match and (period is None or int(match[1]) == period):
                s.require(not path.is_symlink(), "period_directory_symlink")
                candidates.append(d.Store(path))
    found = []
    for store in candidates:
        state = _state(store)
        if period is not None and state["period"] != period or journal_id is not None and state["journal_id"] != journal_id:
            continue
        if phase is not None and state["enrollment"]["phase"] != phase:
            continue
        if enrollment_block_hash is not None and state["enrollment"]["block_hash"] != enrollment_block_hash:
            continue
        s.require(state["settings"] == base["settings"] and state["lanes"] == base["lanes"],
                  "period_configuration_changed")
        found.append(store)
    s.require(len(found) <= 1, "ambiguous_period_journal")
    return found[0] if found else None


def root_store(store):
    import dispatcher as d
    if store.root.parent.name == "periods" and re.fullmatch(r"[0-9]+-(bootstrap|service)", store.root.name):
        parent = store.root.parent.parent
        if (parent / "state.json").exists():
            return d.Store(parent)
    return store


def current_store(base_store, registry_rpc, now, *, requested_period=None, requested_journal_id=None,
                  existing_only=False, enrollment_block_hash=None, requested_phase=None):
    """Caller holds the base dispatcher lock; existing journals are never reset or modified."""
    import dispatcher as d
    base = _state(base_store)
    settings = s.config(base["settings"])
    s.uint(now, 64)
    if requested_period is not None:
        s.uint(requested_period, 64)
    if requested_journal_id is not None:
        s.nonzero(requested_journal_id)
        found = _existing(base_store, base, requested_period, requested_journal_id)
        s.require(found is not None, "unknown_period_journal")
        return found
    if existing_only:
        s.require(requested_period is not None, "requested_period_required")
        if enrollment_block_hash is not None:
            s.nonzero(enrollment_block_hash)
        s.require(requested_phase is None or requested_phase in ("bootstrap", "service"), "invalid_period_phase")
        found = _existing(base_store, base, requested_period, phase=requested_phase,
                          enrollment_block_hash=enrollment_block_hash)
        s.require(found is not None, "unknown_period_journal")
        return found
    s.require(registry_rpc is not None and registry_rpc.call("eth_chainId", []) == settings["registry_chain_id"],
              "wrong_rpc_chain")
    block = registry_rpc.call("eth_getBlockByNumber", ["finalized", False])
    s.require(type(block) is dict, "finalized_block_required")
    anchor, timestamp = s.nonzero(block["hash"]), s.uint(block["timestamp"])
    s.require(timestamp <= now, "future_finalized_registry_time")
    view = lambda signature: enrollment.fixed_word(_view(registry_rpc, settings, anchor, signature))
    start, seconds, active, first = view("startTime()"), view("periodSeconds()"), view("serviceActive()"), view("firstServicePeriod()")
    s.require(seconds > 0 and active in (0, 1), "invalid_registry_clock")
    phase = "service" if active else "bootstrap"
    if active:
        s.require(timestamp >= start, "invalid_registry_clock")
        period, end = (timestamp - start) // seconds, start + ((timestamp - start) // seconds + 1) * seconds
    else:
        period, end = first, start + first * seconds
        s.require(timestamp < end, "bootstrap_window_closed")
    if requested_period is not None and requested_period != period:
        s.require(requested_period < period, "period_boundary_not_finalized")
        found = _existing(base_store, base, requested_period, phase=phase)
        s.require(found is not None, "unknown_period_journal")
        return found
    s.require(now < end, "period_boundary_not_finalized")
    found = _existing(base_store, base, period, phase=phase)
    if found is not None:
        return found
    pinned = PinnedRpc(registry_rpc, block)
    subscriptions = _subscriptions(pinned, settings, period, anchor)
    parent = base_store.root / "periods"
    if not parent.exists():
        parent.mkdir(mode=0o700)
        d.sync_dir(base_store.root)
    d.sentry.private_directory(parent)
    destination = parent / f"{period}-{phase}"
    temporary = parent / (".period-" + uuid.uuid4().hex)
    gateway = base["lanes"].get("gateway")
    d.initialize(temporary, settings, subscriptions, period, base["endpoint"], pinned, gateway=gateway)
    os.replace(temporary, destination)
    d.sync_dir(parent)
    return d.Store(destination)
