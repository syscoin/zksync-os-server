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
        return rpc.contract_page(registry, signature, arguments, anchor)
    return [registry_view(rpc, registry, anchor, signature, *args) for args in arguments]


def eligible_subscribers(rpc, settings, period, anchor):
    registry, sequencer = settings["registry"], settings["sequencer"]
    count = fixed_word(registry_view(rpc, registry, anchor, "friSubscriberCount(address,uint64)", sequencer, period))
    seen = set()
    for start in range(0, count, ENROLLMENT_PAGE_SIZE):
        indices = range(start, min(start + ENROLLMENT_PAGE_SIZE, count))
        encoded = registry_page(rpc, registry, anchor, "friSubscriberAt(address,uint64,uint256)",
                                [(sequencer, period, index) for index in indices])
        s.require(len(encoded) == len(indices), "incomplete_enrollment_page")
        accounts = []
        for word in encoded:
            s.require(len(word) == 32 and word[:12] == bytes(12), "invalid_enrollment_account")
            account = s.nonzero("0x" + word[12:].hex(), 20)
            s.require(account not in seen, "duplicate_enrollment_account")
            seen.add(account)
            accounts.append(account)
        flags = registry_page(rpc, registry, anchor, "isEligibleFriSubscriber(address,address,uint64)",
                              [(account, sequencer, period) for account in accounts])
        s.require(len(flags) == len(accounts), "incomplete_enrollment_page")
        for account, word in zip(accounts, flags):
            eligible = fixed_word(word)
            s.require(eligible in (0, 1), "invalid_enrollment_eligibility")
            if eligible:
                yield account


def enrollment_snapshot(settings, subscriptions, period, rpc, block_hash=None):
    s.config(settings)
    s.uint(period, 64)
    s.require(type(subscriptions) is list, "invalid_subscription_count")
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
    lane = view("supportedLane(uint256)", s.uint(settings["execution_chain_id"]))
    s.require(len(lane) == 64 and lane[:32] == b"\0" * 12 + s.raw_hex(settings["chain_address"]),
              "registry_chain_address_mismatch")
    s.require(fixed_word(view("dutiesPerRound()")) == settings["duties_per_round"], "registry_quota_mismatch")
    start, seconds = fixed_word(view("startTime()")), fixed_word(view("periodSeconds()"))
    first = fixed_word(view("firstServicePeriod()"))
    active = fixed_word(view("serviceActive()"))
    s.require(seconds > 0 and active in (0, 1), "invalid_registry_clock")
    pinned_vk = lane[32:]
    # The first native-accepted bootstrap package installs this pin. The configured nonzero VK
    # and canonical native evidence still bind the initial work before that package exists.
    s.require(pinned_vk == s.raw_hex(settings["vk_hash"]) or not active and pinned_vk == bytes(32),
              "registry_vk_mismatch")
    phase = "service" if active else "bootstrap"
    if active:
        s.require(timestamp >= start and period == (timestamp - start) // seconds,
                  "current_service_period_required")
    else:
        s.require(period == first and timestamp < start + first * seconds, "bootstrap_window_closed")
    s.require(subscriptions or phase == "bootstrap", "empty_snapshot_requires_bootstrap")
    end = start + (period + 1) * seconds if active else start + period * seconds
    eligible_accounts = set(eligible_subscribers(rpc, settings, period, anchor))
    normalized, accounts, operators = [], set(), set()
    for signed in sorted(subscriptions, key=lambda entry: entry["subscription"]["account"]):
        s.exact(signed, ("subscription", "signature"))
        subscription = signed["subscription"]
        request = s.subscription_request(settings, subscription)
        # subscribe() already authenticated both consents for the exact stored tuple. Rechecking
        # a historical ERC-1271 signature can revoke a complete roster after enrollment.
        s.raw_hex(signed["signature"])
        account, operator = subscription["account"], subscription["operator"]
        s.require(account not in accounts and operator not in operators, "duplicate_subscription_identity")
        s.require(subscription["firstPeriod"] <= period <= subscription["lastPeriod"]
                  and subscription["services"] == 3, "inactive_or_nonunified_subscription")
        hashed = request["struct_hash"]
        s.require(view("subscriptionAt(address,address,uint64)", account, settings["sequencer"], period)
                  == s.raw_hex(hashed), "subscription_not_enrolled")
        s.require(view("operatorAccountAt(address,uint64)", operator, period)
                  == b"\0" * 12 + s.raw_hex(account), "operator_not_enrolled")
        s.require(view("subscription(bytes32)", hashed) == s.encode_fields(s.SUBSCRIPTION, subscription),
                  "stored_subscription_mismatch")
        s.require(fixed_word(view("seniorBonus(address)", account)) > 0, "senior_membership_required")
        accounts.add(account)
        operators.add(operator)
        normalized.append(copy.deepcopy(signed))
    s.require(accounts == eligible_accounts, "subscription_snapshot_omits_or_adds_eligible_accounts")
    return normalized, {"block_hash": anchor, "timestamp": timestamp, "phase": phase, "ends_at": end}
