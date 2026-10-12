import copy
from pathlib import Path
import unittest
from unittest.mock import patch

import relay as r
import transactions
import workflow_io as io
import test_coordinator_cross_epoch as ce
from test_service import h


class RelayGraceTests(unittest.TestCase):
    setUpClass = classmethod(ce.CrossEpochCoordinatorTests.setUpClass.__func__)
    setUp = ce.CrossEpochCoordinatorTests.setUp
    reload = ce.CrossEpochCoordinatorTests.reload
    ready = ce.CrossEpochCoordinatorTests.ready
    evidence = ce.CrossEpochCoordinatorTests.evidence
    pick = ce.CrossEpochCoordinatorTests.pick
    offered = ce.CrossEpochCoordinatorTests.offered
    signed_proof = ce.CrossEpochCoordinatorTests.signed_proof
    accepted = ce.CrossEpochCoordinatorTests.accepted
    controller_reload = ce.CrossEpochCoordinatorTests.controller_reload
    acquired = ce.CrossEpochCoordinatorTests.acquired
    publication = ce.CrossEpochCoordinatorTests.publication
    handoff = ce.CrossEpochCoordinatorTests.handoff
    frozen = ce.CrossEpochCoordinatorTests.frozen
    returned = ce.CrossEpochCoordinatorTests.returned

    def staged(self):
        self.handoff()
        identifier, bundle = self.frozen()
        first_work = self.controller.step(True)
        prepared = self.returned(first_work)
        self.native.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.assertEqual(self.controller_reload().step(True)["next_action"], "await_node_publication")
        self.publication(prepared)
        self.rpc.item["digest"] = prepared["sequencer_request"]["digest"]
        staged = self.controller_reload().step(True)
        self.assertEqual(staged["next_action"], "relay_dual_endorsed_package")
        manager = r.Relay(self.f["settings"], self.config["relay_policy"], self.rpc,
                          store=r.Store(self.control_store.root / "relay"))
        self.rpc.item = manager.load_artifact(staged["operation_id"])
        return identifier, bundle, first_work, staged["operation_id"], manager

    def close_grace(self):
        self.registry_rpc.finalize(650, 102)
        self.now = self.rpc.now = 655
        self.rpc.head["timestamp"] = hex(self.now)
        self.rpc.blocks[self.rpc.head["number"]] = copy.deepcopy(self.rpc.head)

    def test_grace_closure_after_unsigned_relay_stage_reaches_repair_and_reuses_proof(self):
        identifier, bundle, first_work, relay_id, _ = self.staged()
        directory = self.controller.directory(identifier)
        proof_bytes = (directory / "proof.json").read_bytes()
        leases = copy.deepcopy(self.controller.state["operations"][identifier]["leases"])
        native_calls = len(self.native.calls)
        self.close_grace()

        repaired = self.controller_reload().step(True)
        self.assertEqual(repaired["status"], "broadcast")
        old = r.Store(self.control_store.root / "relay").load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(old["status"], "stale_unsigned")
        self.assertIsNone(old["unsigned_transaction"])
        self.assertIsNone(old["raw_transaction"])
        self.assertIsNone(self.controller.state["operations"][identifier]["relay"])
        self.assertEqual(self.relay_wallet.calls, [])
        manager = transactions.Transactions(self.f["settings"], self.config["transaction_policy"], self.rpc,
                   self.wallet, transactions.Store(self.control_store.root / "transactions"), clock=lambda: self.now)
        operation = manager.state["operations"][repaired["operation_id"]]
        self.assertEqual(operation["call"]["action"], "repairPackage")
        revised = self.controller.snapshot(identifier, *self.controller.status(), True)
        self.assertEqual(revised["request"]["duties"], [])
        self.assertEqual(revised["request"]["manifest"], bundle["request"]["manifest"])
        self.assertEqual((directory / "proof.json").read_bytes(), proof_bytes)
        self.assertEqual(self.controller.state["operations"][identifier]["leases"], leases)

        self.rpc.receipt = {"transactionHash": operation["transaction_hash"],
            "from": self.f["settings"]["sequencer"], "to": self.f["settings"]["proof_gate"],
            "blockNumber": self.rpc.head["number"], "blockHash": self.rpc.head["hash"], "status": "0x1", "logs": []}
        self.rpc.nonce = self.rpc.pending_nonce = 1
        self.rpc.item["sidecar"] = self.controller.prepare(revised["request"], self.f["evidence"], self.f["fri_payload"])
        self.rpc.advance(seconds=1)
        self.now = self.rpc.now
        replacement_work = self.controller_reload().step(True)
        self.assertEqual(replacement_work["next_action"], "await_selected_wrapper")
        self.assertNotEqual(replacement_work["work_hash"], first_work["work_hash"])
        source = Path(self.controller.state["operations"][identifier]["works"][replacement_work["work_hash"]]["directory"])
        self.assertEqual(io.private_json(source / "work.json")["body"]["proof"], self.f["snark"])
        self.assertEqual((directory / "proof.json").read_bytes(), proof_bytes)
        self.assertEqual(len(self.native.calls), native_calls)

    def test_grace_closure_does_not_release_a_reserved_unsigned_transaction(self):
        identifier, _, _, relay_id, manager = self.staged()
        row = manager.state["operations"][relay_id]
        unsigned = manager._new_transaction(row, manager.load_artifact(relay_id), row["context"])
        row.update(status="signing_uncertain", unsigned_transaction=unsigned)
        manager.save(True)
        self.close_grace()
        self.rpc.simulation_error = True
        result = self.controller_reload().step(True)
        self.assertNotEqual(result.get("next_action"), "relay_dual_endorsed_package")
        retained = r.Store(self.control_store.root / "relay").load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(retained["unsigned_transaction"], unsigned)
        self.assertIsNone(retained["raw_transaction"])
        self.assertEqual(self.controller.state["operations"][identifier]["relay"], relay_id)
        self.assertIsNone(self.controller.state["operations"][identifier]["maintenance"])

    def test_restart_after_relay_retirement_cannot_sign_the_expired_report(self):
        identifier, _, _, relay_id, _ = self.staged()
        self.close_grace()
        controller = self.controller_reload()
        with patch.object(controller, "save", side_effect=OSError("interrupted coordinator pointer save")):
            with self.assertRaisesRegex(OSError, "interrupted coordinator pointer save"):
                controller.step(True)
        old = r.Store(self.control_store.root / "relay").load(self.f["settings"], self.config["relay_policy"])["operations"][relay_id]
        self.assertEqual(old["status"], "stale_unsigned")
        self.assertIsNone(old["unsigned_transaction"])
        self.assertIsNone(old["raw_transaction"])
        restarted = self.controller_reload()
        self.assertEqual(restarted.state["operations"][identifier]["relay"], relay_id)
        result = restarted.step(True)
        self.assertEqual(result["status"], "broadcast")
        self.assertIsNone(restarted.state["operations"][identifier]["relay"])
        self.assertEqual(self.relay_wallet.calls, [])
        manager = transactions.Transactions(self.f["settings"], self.config["transaction_policy"], self.rpc,
                   self.wallet, transactions.Store(self.control_store.root / "transactions"), clock=lambda: self.now)
        self.assertEqual(manager.state["operations"][result["operation_id"]]["call"]["action"], "repairPackage")


if __name__ == "__main__":
    unittest.main()
