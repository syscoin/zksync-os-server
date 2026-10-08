import copy
import functools
import unittest
from unittest.mock import patch

import audit
import enrollment
import service as s


def address(number):
    return f"0x{number:040x}"


def word_hash(number):
    return f"0x{number:064x}"


class FullRosterRegistry:
    def __init__(self, settings, gateway, subscriptions, anchor):
        self.settings, self.anchor = settings, anchor
        self.calls = []
        self.views = {
            "policyHash()": {(): s.raw_hex(settings["policy_hash"])},
            "supportedLane(uint256)": {
                (s.uint(lane["execution_chain_id"]),):
                bytes(12) + s.raw_hex(lane["chain_address"]) + s.raw_hex(lane["vk_hash"])
                for lane in (settings, gateway)
            },
            "dutiesPerRound()": {(): s.word(settings["duties_per_round"])},
            "startTime()": {(): s.word(0)},
            "periodSeconds()": {(): s.word(100)},
            "firstServicePeriod()": {(): s.word(5)},
            "serviceActive()": {(): s.word(1)},
            "friSubscriberCount(address,uint64)": {(settings["sequencer"], 5): s.word(len(subscriptions))},
            "friSubscriberAt(address,uint64,uint256)": {},
            "isEligibleFriSubscriber(address,address,uint64)": {},
            "subscriptionAt(address,address,uint64)": {},
            "operatorAccountAt(address,uint64)": {},
            "subscription(bytes32)": {},
            "seniorBonus(address)": {},
        }
        for index, signed in enumerate(subscriptions):
            subscription = signed["subscription"]
            account, operator = subscription["account"], subscription["operator"]
            hashed = s.struct_hash("ProverSubscriptionV1", s.SUBSCRIPTION, subscription)
            self.views["friSubscriberAt(address,uint64,uint256)"][(settings["sequencer"], 5, index)] = (
                bytes(12) + s.raw_hex(account))
            self.views["isEligibleFriSubscriber(address,address,uint64)"][(account, settings["sequencer"], 5)] = s.word(1)
            self.views["subscriptionAt(address,address,uint64)"][(account, settings["sequencer"], 5)] = s.raw_hex(hashed)
            self.views["operatorAccountAt(address,uint64)"][(operator, 5)] = bytes(12) + s.raw_hex(account)
            self.views["subscription(bytes32)"][(hashed,)] = s.encode_fields(s.SUBSCRIPTION, subscription)
            self.views["seniorBonus(address)"][(account,)] = s.word(35000)

    def call(self, method, params):
        self.calls.append((method, params))
        if method == "eth_chainId" and params == []:
            return self.settings["registry_chain_id"]
        if method == "eth_getBlockByHash" and params == [self.anchor["block_hash"], False]:
            return {"hash": self.anchor["block_hash"], "timestamp": hex(self.anchor["timestamp"])}
        raise AssertionError((method, params))

    def contract(self, registry, signature, values, anchor):
        if registry != self.settings["registry"] or anchor != self.anchor["block_hash"]:
            raise AssertionError((registry, anchor))
        self.calls.append(("eth_call", signature, values, anchor))
        return self.views[signature][values]


class AuditEnrollmentCostTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Cache real deterministic cast results so the full roster does not repeat hash subprocesses.
        cached_cast = functools.lru_cache(maxsize=None)(s.cast)
        cast_patch = patch.object(s, "cast", cached_cast)
        cast_patch.start()
        cls.addClassCleanup(cast_patch.stop)
        cls.addClassCleanup(cached_cast.cache_clear)
        cls.settings = {
            "schema_version": 1, "execution_chain_id": "0x23a", "registry_chain_id": "0x23a",
            "chain_address": address(10), "settlement_chain_id": "0x1644", "registry": address(11),
            "coordinator": address(12), "proof_gate": address(13), "policy_hash": word_hash(21),
            "vk_hash": word_hash(22), "sequencer": address(14), "duties_per_round": 4,
        }
        cls.gateway = {**cls.settings, "execution_chain_id": "0x1644", "chain_address": address(15),
                       "settlement_chain_id": "0x1", "vk_hash": word_hash(23)}
        cls.anchor = {"block_hash": word_hash(100), "timestamp": 550, "phase": "service", "ends_at": 600}
        # Historical account signatures are opaque; the registry authenticates each exact stored tuple.
        cls.subscriptions = [{"subscription": {
            "account": address(1000 + index), "operator": address(2000 + index),
            "beneficiary": address(3000 + index), "sequencer": cls.settings["sequencer"],
            "firstPeriod": 5, "lastPeriod": 8, "nonce": 7, "services": 3,
        }, "signature": "0x1234"} for index in range(256)]
        cls.child_lane = {"settings": cls.settings, "endpoint_commitment": s.keccak(b"https://child.example/")}
        cls.gateway_lane = {"settings": cls.gateway, "endpoint_commitment": s.keccak(b"https://gateway.example/")}

    def validate_full_roster(self, lanes, expected_calls):
        identity = copy.deepcopy({"schema_version": 1, "settings": self.settings,
                                  "subscriptions": self.subscriptions, "period": 5,
                                  "enrollment": self.anchor, "lanes": lanes})
        journal_id = s.keccak(s.canonical(identity))
        rpc = FullRosterRegistry(identity["settings"], self.gateway, identity["subscriptions"], identity["enrollment"])
        created = []
        original_init = s.EnrollmentAuthority.__init__

        def record_authority(authority, settings, subscriptions, period, rpc, block_hash):
            original_init(authority, settings, subscriptions, period, rpc, block_hash)
            created.append((settings["execution_chain_id"], authority))

        with patch.object(s.EnrollmentAuthority, "__init__", record_authority), \
                patch.object(enrollment, "enrollment_snapshot", wraps=enrollment.enrollment_snapshot) as snapshots:
            authority = audit.validate_identity(identity, journal_id,
                                                {"enrollment_block_hash": self.anchor["block_hash"]}, rpc)

        self.assertEqual(snapshots.call_count, len(lanes))
        self.assertEqual([call.args[0]["execution_chain_id"] for call in snapshots.call_args_list],
                         [lane["settings"]["execution_chain_id"] for lane in lanes.values()])
        self.assertEqual(len(rpc.calls), expected_calls)
        for signature in ("friSubscriberAt(address,uint64,uint256)", "isEligibleFriSubscriber(address,address,uint64)",
                          "subscriptionAt(address,address,uint64)", "operatorAccountAt(address,uint64)",
                          "subscription(bytes32)", "seniorBonus(address)"):
            self.assertEqual(sum(call[0] == "eth_call" and call[1] == signature for call in rpc.calls), 256 * len(lanes))
        self.assertEqual(sum(call[0] == "eth_call" and call[1] == "supportedLane(uint256)" for call in rpc.calls), len(lanes))
        child_authorities = [value for chain_id, value in created if chain_id == self.settings["execution_chain_id"]]
        self.assertEqual(len(child_authorities), 1)
        self.assertIs(authority, child_authorities[0])
        self.assertEqual(authority.normalized_snapshot, s.canonical(identity["subscriptions"]))
        self.assertEqual(authority.anchor, s.canonical(identity["enrollment"]))
        authority.check(identity["settings"], identity["subscriptions"], identity["period"])

    def test_child_full_roster_needs_one_1546_call_scan(self):
        self.validate_full_roster({"child": self.child_lane}, 1546)

    def test_child_gateway_full_roster_needs_two_3092_call_scans(self):
        for gateway_first in (False, True):
            with self.subTest(gateway_first=gateway_first):
                lanes = ({"gateway": self.gateway_lane, "child": self.child_lane} if gateway_first else
                         {"child": self.child_lane, "gateway": self.gateway_lane})
                self.validate_full_roster(lanes, 3092)


if __name__ == "__main__":
    unittest.main()
