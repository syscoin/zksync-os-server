import unittest
from unittest.mock import Mock, patch

import deploy_component as component
import deploy_gate as gate
import relay as r
import service as s
from test_service import a, h


class PriorityAdmissionSetupTests(unittest.TestCase):
    def setUp(self):
        self.chain, self.admission, self.admin = a(31), a(32), a(33)
        self.cfg = {"chain": self.chain, "chain_id": 57}
        self.plan = {"component": self.admission}
        self.admin_role = gate.ZERO
        self.stored_hash = h(37)
        self.frontier = 2
        self.policy_frontier = 2
        self.rpc = Mock()
        self.rpc.call.side_effect = self.rpc_call

    def call_address(self, rpc, target, signature):
        self.assertIs(rpc, self.rpc)
        self.assertEqual(target, self.chain)
        return {"getSyscoinPriorityCommitPolicy()": self.admin_role, "getAdmin()": self.admin}[signature]

    def call_word(self, rpc, target, signature):
        self.assertIs(rpc, self.rpc)
        self.assertIn(target, (self.chain, self.admission))
        values = {
            "getTotalBatchesCommitted()": self.frontier,
            "getTotalBatchesVerified()": self.frontier,
            "getTotalBatchesExecuted()": self.frontier,
            "getFirstUnprocessedPriorityTx()": 7,
            "getTotalPriorityTxs()": 7,
            "isPriorityQueueActive()": 0,
            "initialBatch()": self.policy_frontier,
            "initialPriorityCursor()": 7,
            "initialStoredHash()": self.stored_hash,
        }
        value = values[signature]
        return hex(value) if type(value) is int else value

    def rpc_call(self, method, arguments):
        self.assertEqual(method, "eth_call")
        tx, _ = arguments
        if tx["data"][2:10] == r.selector("storedBatchHash(uint256)").hex():
            return self.stored_hash
        self.assertEqual(tx["to"], self.chain)
        self.assertEqual(tx["from"], self.admin)
        return "0x"

    def preview(self):
        with patch.object(gate, "call_address", self.call_address), patch.object(gate, "call_word", self.call_word), \
                patch.object(gate, "digest", lambda code: s.keccak(s.raw_hex(code))), \
                patch.object(gate, "rpc_code", return_value="0x01"):
            return component.prepare_admission_install(self.rpc, self.cfg, self.plan)

    def test_prepares_exact_admin_call_before_install_and_attests_after(self):
        result = self.preview()
        tx = result["transaction"]
        self.assertFalse(result["broadcast"])
        self.assertFalse(result["installed"])
        self.assertEqual(tx["to"], self.chain)
        self.assertEqual(tx["from"], self.admin)
        expected = r.selector("setSyscoinPriorityCommitPolicy(address)") + bytes(12) + s.raw_hex(self.admission, 20)
        self.assertEqual(tx["data"], "0x" + expected.hex())
        self.assertIn(unittest.mock.call("eth_call", [tx, "latest"]), self.rpc.call.call_args_list)

        self.rpc.reset_mock()
        self.admin_role = self.admission
        ready = self.preview()
        self.assertTrue(ready["installed"])
        self.assertIsNone(ready["transaction"])
        self.rpc.call.assert_not_called()

    def test_changed_frontier_or_replacement_policy_rejects_before_simulation(self):
        self.policy_frontier = 1
        with self.assertRaisesRegex(gate.TransactionError, "deployment frontier changed"):
            self.preview()
        self.rpc.call.assert_not_called()
        self.admin_role = a(95)
        with self.assertRaisesRegex(gate.TransactionError, "different priority admission"):
            self.preview()
        self.rpc.call.assert_not_called()

    def test_deployed_gate_checks_admission_runtime_and_activation(self):
        with patch.object(gate, "call_address", side_effect=[a(35), self.admission]), \
                patch.object(gate, "rpc_code", return_value="0x01"), \
                patch.object(gate, "digest", return_value=h(55)), \
                patch.object(gate, "call_word", return_value=h(55)), \
                patch.object(r, "call_word", return_value=False):
            with self.assertRaisesRegex(gate.TransactionError, "priority admission not active"):
                gate.verify_active_admission(self.rpc, a(34), a(36))


if __name__ == "__main__":
    unittest.main()
