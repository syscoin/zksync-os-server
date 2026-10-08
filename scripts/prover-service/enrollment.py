"""Canonical subscription snapshots; independent of native/rental tooling."""

import copy

import service as s


MAX_OPERATIONS = 2000


def fixed_word(raw):
    s.require(len(raw) == 32, "invalid_rpc_word")
    return int.from_bytes(raw, "big")


def enrollment_snapshot(settings, subscriptions, period, rpc, block_hash=None):
    s.config(settings)
    s.uint(period, 64)
    s.require(type(subscriptions) is list and 0 < len(subscriptions) <= 256, "invalid_subscription_count")
    s.require(rpc.call("eth_chainId", []) == settings["registry_chain_id"], "wrong_rpc_chain")
    block = (rpc.call("eth_getBlockByHash", [s.nonzero(block_hash), False]) if block_hash is not None
             else rpc.call("eth_getBlockByNumber", ["finalized", False]))
    s.require(type(block) is dict, "finalized_block_required")
    anchor = s.nonzero(block["hash"])
    s.require(block_hash is None or anchor == block_hash, "enrollment_block_changed")
    timestamp = s.uint(block["timestamp"])
    registry = settings["registry"]
    def view(signature, *values):
        if hasattr(rpc, "contract"):
            return rpc.contract(registry, signature, values, anchor)
        return s.raw_hex(rpc.call("eth_call", [{"to": registry,
                         "data": s.cast("calldata", signature, *map(str, values))},
                         {"blockHash": anchor, "requireCanonical": True}]))
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
    end = start + (period + 1) * seconds if active else start + period * seconds
    enrolled_count = fixed_word(view("friSubscriberCount(address,uint64)", settings["sequencer"], period))
    s.require(enrolled_count <= 256, "enrollment_exceeds_dispatcher_capacity")
    eligible_accounts, enumerated = set(), set()
    for index in range(enrolled_count):
        encoded = view("friSubscriberAt(address,uint64,uint256)", settings["sequencer"], period, index)
        s.require(len(encoded) == 32 and encoded[:12] == bytes(12), "invalid_enrollment_account")
        account = s.nonzero("0x" + encoded[12:].hex(), 20)
        s.require(account not in enumerated, "duplicate_enrollment_account")
        enumerated.add(account)
        eligible = fixed_word(view("isEligibleFriSubscriber(address,address,uint64)", account,
                                   settings["sequencer"], period))
        s.require(eligible in (0, 1), "invalid_enrollment_eligibility")
        if eligible:
            eligible_accounts.add(account)
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
