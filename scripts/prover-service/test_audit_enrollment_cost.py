import copy
import functools
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import audit
import dispatcher
import enrollment
import period_routing
import service as s
from test_service import KEYS, sign


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

    def test_startup_and_rollover_keep_the_complete_257_subscriber_roster(self):
        extra = copy.deepcopy(self.subscriptions[-1])
        extra["subscription"].update(account=address(1256), operator=address(2256))
        expected = self.subscriptions + [extra]
        rpc = FullRosterRegistry(self.settings, self.gateway, list(reversed(expected)), self.anchor)
        batches = []

        def contract_page(registry, signature, arguments, anchor):
            batches.append((signature, len(arguments), anchor))
            return [rpc.contract(registry, signature, args, anchor) for args in arguments]

        rpc.contract_page = contract_page
        block_hash = self.anchor["block_hash"]
        pinned = period_routing.PinnedRpc(rpc, {"hash": block_hash, "timestamp": hex(self.anchor["timestamp"])})
        rolled, anchor = enrollment.registry_enrollment_snapshot(self.settings, 5, pinned, block_hash)
        with tempfile.TemporaryDirectory() as root:
            store = dispatcher._initialize_from_snapshot(Path(root) / "period", self.settings, rolled, 5,
                    "https://child.example/", pinned, anchor, {"settings": self.gateway, "endpoint": "https://gateway.example/"})
            journal = s.read_json(store.root / "state.json", private=True)
        self.assertEqual(journal["enrollment"], self.anchor)
        self.assertEqual(journal["cursor"], journal["journal_id"])
        self.assertEqual(set(journal["accounts"]), {entry["subscription"]["account"] for entry in expected})
        self.assertEqual([entry["subscription"] for entry in journal["subscriptions"]],
                         [entry["subscription"] for entry in expected])
        signatures = ["friSubscriberAt(address,uint64,uint256)", "isEligibleFriSubscriber(address,address,uint64)",
                      "subscriptionAt(address,address,uint64)", "subscription(bytes32)", "operatorAccountAt(address,uint64)",
                      "seniorBonus(address)"]
        for signature in signatures:
            self.assertEqual([size for name, size, _ in batches if name == signature], [32] * 8 + [1])
        self.assertEqual([call[2] for call in rpc.calls if call[:2] == ("eth_call", "supportedLane(uint256)")],
                         [(s.uint(self.settings["execution_chain_id"]),), (s.uint(self.gateway["execution_chain_id"]),)])
        self.assertEqual({anchor for _, _, anchor in batches}, {block_hash})
        with self.assertRaisesRegex(s.Error, "omits_or_adds"):
            enrollment.enrollment_snapshot(self.settings, rolled[:-1], 5, pinned)

    def test_child_gateway_full_roster_needs_two_3092_call_scans(self):
        for gateway_first in (False, True):
            with self.subTest(gateway_first=gateway_first):
                lanes = ({"gateway": self.gateway_lane, "child": self.child_lane} if gateway_first else
                         {"child": self.child_lane, "gateway": self.gateway_lane})
                self.validate_full_roster(lanes, 3092)

    def test_manifest_accepts_authenticated_1001_member_snapshot(self):
        settings = {**self.settings, "sequencer": s.cast("wallet", "address", "--private-key", KEYS["sequencer"]).lower()}
        template = self.subscriptions[0]["subscription"]
        subscriptions = [{"subscription": {**template, "account": address(1000 + n), "operator": address(3000 + n),
                                           "sequencer": settings["sequencer"]}, "signature": "0x"}
                         for n in range(1001)]
        registry = FullRosterRegistry(settings, self.gateway, subscriptions, self.anchor)
        authority = s.EnrollmentAuthority(settings, subscriptions, 5, registry, self.anchor["block_hash"])
        payload = {"schema_version": 1, "chain_id": settings["execution_chain_id"], "chain_address": settings["chain_address"],
                   "sequencer": settings["sequencer"], "period": 5, "previous_cursor": word_hash(1),
                   "subscription_snapshot_hash": s.keccak(s.canonical(subscriptions)), "assignments": [], "retries": []}
        manifest = {"payload": payload, "sequencer_signature": sign(s.manifest_request(settings, payload), "sequencer")}
        _, validated = s.validate_manifest(settings, manifest, subscriptions, {}, allow_empty=True, enrollment=authority)
        self.assertEqual(len(validated), 1001)
        with self.assertRaisesRegex(s.Error, "invalid_subscription_snapshot"):
            s.validate_manifest(settings, manifest, subscriptions, {}, allow_empty=True)


if __name__ == "__main__":
    unittest.main()
