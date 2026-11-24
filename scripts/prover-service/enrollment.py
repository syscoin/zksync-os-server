"""Canonical subscription snapshots; independent of native/rental tooling."""

import copy

import service as s


ENROLLMENT_PAGE_SIZE = 32


def fixed_word(raw):
    s.require(len(raw) == 32, "invalid_rpc_word")
    return int.from_bytes(raw, "big")


def registry_view(rpc, registry, anchor, signature, *values):
    if hasattr(rpc, "contract"):
        return rpc.contract(registry, signature, values, anchor)
    return s.raw_hex(rpc.call("eth_call", [{"to": registry,
                     "data": s.cast("calldata", signature, *map(str, values))},
                     {"blockHash": anchor, "requireCanonical": True}]))


def registry_page(rpc, registry, anchor, signature, arguments):
    if hasattr(rpc, "contract_page"):
        result = rpc.contract_page(registry, signature, arguments, anchor)
    else:
        result = [registry_view(rpc, registry, anchor, signature, *args) for args in arguments]
    s.require(len(result) == len(arguments), "incomplete_enrollment_page")
    return result


def eligible_subscribers(rpc, settings, period, anchor):
    registry, sequencer = settings["registry"], settings["sequencer"]
    count = fixed_word(registry_view(rpc, registry, anchor, "friSubscriberCount(address,uint64)", sequencer, period))
    seen = set()
    for start in range(0, count, ENROLLMENT_PAGE_SIZE):
        indices = range(start, min(start + ENROLLMENT_PAGE_SIZE, count))
        encoded = registry_page(rpc, registry, anchor, "friSubscriberAt(address,uint64,uint256)",
                                [(sequencer, period, index) for index in indices])
        accounts = []
        for word in encoded:
            s.require(len(word) == 32 and word[:12] == bytes(12), "invalid_enrollment_account")
            account = s.nonzero("0x" + word[12:].hex(), 20)
            s.require(account not in seen, "duplicate_enrollment_account")
            seen.add(account)
            accounts.append(account)
        flags = registry_page(rpc, registry, anchor, "isEligibleFriSubscriber(address,address,uint64)",
                              [(account, sequencer, period) for account in accounts])
        for account, word in zip(accounts, flags):
            eligible = fixed_word(word)
            s.require(eligible in (0, 1), "invalid_enrollment_eligibility")
            if eligible:
                yield account


def validate_lane(settings, rpc, anchor_hash, phase):
    lane = registry_view(rpc, settings["registry"], anchor_hash, "supportedLane(uint256)",
                         s.uint(settings["execution_chain_id"]))
    s.require(len(lane) == 64 and lane[:32] == b"\0" * 12 + s.raw_hex(settings["chain_address"]),
              "registry_chain_address_mismatch")
    pinned_vk = lane[32:]
    # The first native-accepted bootstrap package installs this pin. The configured nonzero VK
    # and canonical native evidence still bind the initial work before that package exists.
    s.require(pinned_vk == s.raw_hex(settings["vk_hash"]) or phase == "bootstrap" and pinned_vk == bytes(32),
              "registry_vk_mismatch")


def _decode_subscription(encoded):
    s.require(len(encoded) == 32 * len(s.SUBSCRIPTION), "invalid_stored_subscription")
    subscription = {}
    for index, (name, kind) in enumerate(s.SUBSCRIPTION):
        word = encoded[index * 32:(index + 1) * 32]
        if kind == "address":
            s.require(word[:12] == bytes(12), "invalid_subscription_address")
            subscription[name] = "0x" + word[12:].hex()
        else:
            subscription[name] = s.uint(int.from_bytes(word, "big"), int(kind[4:]))
    return subscription


def enrollment_snapshot(settings, subscriptions, period, rpc, block_hash=None):
    s.require(type(subscriptions) is list, "invalid_subscription_count")
    return _snapshot(settings, subscriptions, period, rpc, block_hash)


def registry_enrollment_snapshot(settings, period, rpc, block_hash):
    """Build rollover enrollment from the registry that authenticated both historical consents."""
    return _snapshot(settings, None, period, rpc, block_hash)


def _snapshot(settings, subscriptions, period, rpc, block_hash):
    s.config(settings)
    s.uint(period, 64)
    s.require(rpc.call("eth_chainId", []) == settings["registry_chain_id"], "wrong_rpc_chain")
    block = (rpc.call("eth_getBlockByHash", [s.nonzero(block_hash), False]) if block_hash is not None
             else rpc.call("eth_getBlockByNumber", ["finalized", False]))
    s.require(type(block) is dict, "finalized_block_required")
    anchor = s.nonzero(block["hash"])
    s.require(block_hash is None or anchor == block_hash, "enrollment_block_changed")
    timestamp = s.uint(block["timestamp"])
    registry = settings["registry"]
    def view(signature, *values):
        return registry_view(rpc, registry, anchor, signature, *values)
    s.require(view("policyHash()") == s.raw_hex(settings["policy_hash"]), "registry_policy_mismatch")
    s.require(fixed_word(view("dutiesPerRound()")) == settings["duties_per_round"], "registry_quota_mismatch")
    start, seconds = fixed_word(view("startTime()")), fixed_word(view("periodSeconds()"))
    first = fixed_word(view("firstServicePeriod()"))
    active = fixed_word(view("serviceActive()"))
    s.require(seconds > 0 and active in (0, 1), "invalid_registry_clock")
    phase = "service" if active else "bootstrap"
    validate_lane(settings, rpc, anchor, phase)
    if active:
        s.require(timestamp >= start and period == (timestamp - start) // seconds,
                  "current_service_period_required")
    else:
        s.require(period == first and timestamp < start + first * seconds, "bootstrap_window_closed")
    end = start + (period + 1) * seconds if active else start + period * seconds

    s.require(subscriptions is None or subscriptions or phase == "bootstrap", "empty_snapshot_requires_bootstrap")
    eligible = sorted(eligible_subscribers(rpc, settings, period, anchor))
    supplied = None
    if subscriptions is None:
        s.require(eligible, "period_has_no_eligible_subscribers")
    else:
        supplied = {}
        for signed in subscriptions:
            s.exact(signed, ("subscription", "signature"))
            account = signed["subscription"]["account"]
            s.require(account not in supplied, "duplicate_subscription_identity")
            supplied[account] = signed
        eligible_accounts = set(eligible)
        s.require(supplied.keys() <= eligible_accounts, "subscription_not_enrolled")
        s.require(supplied.keys() == eligible_accounts, "subscription_snapshot_omits_or_adds_eligible_accounts")

    def page(signature, arguments):
        return registry_page(rpc, registry, anchor, signature, arguments)

    normalized, operators = [], set()
    for start_index in range(0, len(eligible), ENROLLMENT_PAGE_SIZE):
        accounts = eligible[start_index:start_index + ENROLLMENT_PAGE_SIZE]
        hashes = page("subscriptionAt(address,address,uint64)",
                      [(account, settings["sequencer"], period) for account in accounts])
        for hashed in hashes:
            s.require(len(hashed) == 32 and any(hashed), "subscription_not_enrolled")
        records = page("subscription(bytes32)", [("0x" + hashed.hex(),) for hashed in hashes])
        signed_page = ([supplied[account] for account in accounts] if supplied is not None else
                       [{"subscription": _decode_subscription(record), "signature": "0x"} for record in records])
        for account, signed, hashed, record in zip(accounts, signed_page, hashes, records):
            subscription = signed["subscription"]
            request = s.subscription_request(settings, subscription)
            # subscribe() authenticated both consents. A historical ERC-1271 signature can later be revoked.
            s.raw_hex(signed["signature"])
            operator = subscription["operator"]
            s.require(operator not in operators, "duplicate_subscription_identity")
            operators.add(operator)
            s.require(subscription["firstPeriod"] <= period <= subscription["lastPeriod"]
                      and subscription["services"] == 3, "inactive_or_nonunified_subscription")
            s.require(s.raw_hex(request["struct_hash"]) == hashed, "subscription_not_enrolled")
            s.require(subscription["account"] == account and s.encode_fields(s.SUBSCRIPTION, subscription) == record,
                      "stored_subscription_mismatch")
        bindings = page("operatorAccountAt(address,uint64)", [(signed["subscription"]["operator"], period)
                                                              for signed in signed_page])
        bonuses = page("seniorBonus(address)", [(account,) for account in accounts])
        for account, binding, bonus in zip(accounts, bindings, bonuses):
            s.require(binding == bytes(12) + s.raw_hex(account), "operator_not_enrolled")
            s.require(fixed_word(bonus) > 0, "senior_membership_required")
        normalized.extend(copy.deepcopy(signed_page))
    return normalized, {"block_hash": anchor, "timestamp": timestamp, "phase": phase, "ends_at": end}
