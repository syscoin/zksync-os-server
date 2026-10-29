import copy
import importlib.util
import pathlib
import unittest

path = pathlib.Path(__file__).resolve().parents[1] / "zksys_keeper_transactions.py"
spec = importlib.util.spec_from_file_location("keeper_transactions", path)
keeper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(keeper)

A = "0x" + "11" * 20
B = "0x" + "22" * 20
H = "0x" + "33" * 32
BLOCK = "0x" + "44" * 32
PRIORITY = "0x" + "55" * 32
TOPIC = "0x" + "66" * 32
RAW = "0x1234"


class Rpc:
    def __init__(self):
        self.sent = []
        self.final = 0
        self.transactions = {}
        self.latest = 0
        self.pending = 0
        self.fail_send = False
        self.block = BLOCK

    def __call__(self, method, args):
        if method == "eth_chainId": return "0x39"
        if method == "eth_getTransactionCount": return hex(self.pending if args[1] == "pending" else self.latest)
        if method == "eth_sendRawTransaction":
            self.sent.append(args[0])
            if self.fail_send: raise OSError("timeout")
            return H
        if method == "eth_getTransactionReceipt": return self.transactions.get(args[0])
        if method == "eth_getBlockByNumber":
            if args[0] == "finalized": return {"number": hex(self.final), "hash": BLOCK}
            return {"hash": self.block}
        raise AssertionError(method)


def receipt(hash_):
    return {"transactionHash": hash_, "blockHash": BLOCK, "blockNumber": "0x5", "status": "0x1", "logs": []}


class DriverTest(unittest.TestCase):
    def setUp(self):
        self.journal = {}
        self.saved = []
        self.tx = None
        self.rpc = Rpc()
        def save(journal): self.saved.append(copy.deepcopy(journal))
        def sign(tx): self.tx = tx; return RAW
        self.driver = keeper.TransactionDriver(self.journal, save, sign, lambda _: self.tx, lambda _: H)

    def begin(self, **kwargs):
        return self.driver.begin("l1", self.rpc, 57, A, B, "0x1234", 3, 50000, 7,
                                 kind=kwargs.pop("kind", "bridge"), **kwargs)

    def test_signed_bytes_are_saved_before_broadcast_and_timeout_recovers_exact_bytes(self):
        self.rpc.fail_send = True
        with self.assertRaises(OSError): self.begin()
        self.assertEqual(self.saved[0]["transactions"]["l1"]["state"], "signed")
        self.assertEqual(self.saved[0]["transactions"]["l1"]["raw"], RAW)
        with self.assertRaises(keeper.TransactionError): self.begin()
        self.rpc.fail_send = False
        self.driver.broadcast("l1", self.rpc)
        self.assertEqual(self.rpc.sent, [RAW, RAW])
        self.assertEqual(self.journal["transactions"]["l1"]["state"], "submitted")

    def test_external_pending_nonce_fails_closed(self):
        self.rpc.pending = 1
        with self.assertRaises(keeper.TransactionError): self.begin()
        self.assertEqual(self.rpc.sent, [])

    def test_persistence_failure_poison_stops_all_further_writes(self):
        self.driver.save = lambda _: (_ for _ in ()).throw(OSError("disk error"))
        with self.assertRaises(OSError): self.begin()
        self.assertEqual(self.rpc.sent, [])
        with self.assertRaises(keeper.TransactionError): self.driver.broadcast("l1", self.rpc)
        self.assertEqual(self.rpc.sent, [])

    def test_finality_and_delivery_are_both_required(self):
        self.begin()
        self.rpc.transactions[H] = receipt(H)
        self.assertIsNone(self.driver.receipt("l1", self.rpc))
        self.rpc.final = 5
        self.driver.receipt("l1", self.rpc)
        with self.assertRaises(keeper.TransactionError): self.driver.complete("l1")
        self.rpc.transactions[PRIORITY] = receipt(PRIORITY)
        with self.assertRaises(keeper.TransactionError):
            self.driver.confirm_bridge_delivery("l1", self.rpc, 57, PRIORITY, lambda *_: False)
        self.driver.confirm_bridge_delivery("l1", self.rpc, 57, PRIORITY, lambda *_: True)
        self.driver.complete("l1")
        self.assertEqual(self.journal["transactions"], {})

    def test_reorg_is_not_finality(self):
        self.begin(kind="activation")
        self.rpc.transactions[H] = receipt(H)
        self.rpc.final = 5
        self.rpc.block = "0x" + "77" * 32
        with self.assertRaises(keeper.TransactionError): self.driver.receipt("l1", self.rpc)

    def test_bridge_event_is_from_expected_address(self):
        r = receipt(H)
        r["logs"] = [{"address": B, "topics": [TOPIC, PRIORITY]}]
        self.assertEqual(keeper.bridge_priority_hash(r, B, TOPIC), PRIORITY)
        with self.assertRaises(keeper.TransactionError): keeper.bridge_priority_hash(r, A, TOPIC)
        r["logs"].append(r["logs"][0])
        with self.assertRaises(keeper.TransactionError): keeper.bridge_priority_hash(r, B, TOPIC)


if __name__ == "__main__": unittest.main()
