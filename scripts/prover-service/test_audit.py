import base64
import copy
from pathlib import Path
import unittest
from unittest.mock import patch

import audit
import dispatcher as d
import service as s
import test_dispatcher as td
from test_service import sign, h, a, KEYS


class AuditTests(unittest.TestCase):
    setUpClass = classmethod(td.DispatcherTests.setUpClass.__func__)
    setUp = td.DispatcherTests.setUp
    reload = td.DispatcherTests.reload
    ready = td.DispatcherTests.ready
    evidence = td.DispatcherTests.evidence
    pick = td.DispatcherTests.pick
    offered = td.DispatcherTests.offered
    signed_proof = td.DispatcherTests.signed_proof

    def accepted(self, number=1, token=None):
        operation = self.offered(number, token)
        proof, signature = self.signed_proof(operation, number)
        self.network.responses.append((204, {"x-syscoin-prover-disposition": "accepted"}, b""))
        self.dispatcher.submit(operation, proof, signature)
        return operation

    def package(self, start=1, end=None):
        if end is None:
            end = max((op["assignment"]["batch_number"] for op in self.dispatcher.state["operations"].values()
                       if op["assignment"] is not None), default=2)
        payload = self.dispatcher.range_manifest_payload("child", start, end)
        manifest = {"payload": payload, "sequencer_signature": sign(s.manifest_request(self.f["settings"], payload), "sequencer")}
        bundle = self.dispatcher.audit_payload("child", start, end)
        bundle["sequencer_signature"] = sign(audit.checkpoint_request(bundle), "sequencer")
        journal_id = bundle["journal_id"]
        duties = [s.read_json(self.dispatcher.store.root / key / "duty.json", private=True)
                  for key, op in self.dispatcher.state["operations"].items()
                  if op["status"] == "accepted" and start <= op["assignment"]["batch_number"] <= end]
        trust = {"schema_version": 1, "journal_id": journal_id, "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": journal_id, "assignment_cursor": journal_id},
                 "required_readiness": [], "required_duties": []}
        evidence = self.f["evidence"] if (start, end) == (1, 2) else self.evidence(start)
        return {"settings": self.f["settings"], "evidence": evidence, "manifest": manifest,
                "subscriptions": self.dispatcher.state["subscriptions"], "duties": duties,
                "fri_payload": {"from_batch_number": start, "to_batch_number": end,
                                "vk_hash": self.f["settings"]["vk_hash"],
                                "fri_proofs": [self.f["proofs"][number - 1]["proof"] for number in range(start, end + 1)]},
                "bundle": bundle, "trust": trust, "rpc": self.rpc}

    def invalid_proof(self, operation, number=1):
        proof = {**self.f["proofs"][number - 1], "proof": base64.b64encode(bytes([199]) * 96).decode()}
        return proof, sign(self.dispatcher.proof_request(operation, proof), "operator")

    def rehash(self, bundle):
        head = bundle["journal_id"]
        for sequence, event in enumerate(bundle["events"], 1):
            event.update(sequence=sequence, previous=head)
            event["hash"] = s.keccak(s.canonical({key: value for key, value in event.items() if key != "hash"}))
            head = event["hash"]
        bundle["checkpoint"].update(eventCount=len(bundle["events"]), eventHead=head)
        bundle["sequencer_signature"] = sign(audit.checkpoint_request(bundle), "sequencer")

    def two_accounts(self):
        subscription = copy.deepcopy(self.f["subscription"])
        subscription["account"] = s.cast("wallet", "address", "--private-key", KEYS["wrapper"]).lower()
        subscription["operator"] = self.account
        signed = {"subscription": subscription,
                  "signature": sign(s.subscription_request(self.f["settings"], subscription), "wrapper")}
        subscriptions = [signed, *self.f["subscriptions"]]
        self.rpc = td.Rpc(self.f["settings"], subscriptions)
        self.root = Path(self.tmp.name) / "two-accounts"
        self.store = d.initialize(self.root, self.f["settings"], subscriptions, 5, "https://trusted.example/", self.rpc)
        self.reload()
        return subscription["account"]

    def test_full_history_replays_and_contains_no_host_capabilities(self):
        first, second = self.accepted(), self.accepted(2)
        package = self.package()
        summary = audit.verify_package(**package)
        self.assertEqual(summary["accounts"][self.account]["offered_opportunities"], 2)
        self.assertEqual(summary["required_duties"], package["duties"])
        encoded = s.canonical(package["bundle"])
        for secret in (b"Basic host-only", b"trusted.example", b"lease_token", h(102).encode(), b"prover_input"):
            self.assertNotIn(secret, encoded)
        before = len(package["bundle"]["events"])
        proof, signature = self.signed_proof(second, 2)
        self.dispatcher.submit(second, proof, signature)
        self.assertEqual(len(self.dispatcher.state["audit"]["events"]), before)
        self.assertEqual([first, second], ["job-000000", "job-000001"])

    def test_selective_accepted_duty_omission_is_rejected(self):
        self.accepted()
        self.accepted(2)
        package = self.package()
        package["duties"] = package["duties"][1:]
        with self.assertRaisesRegex(s.Error, "accepted_duties_incomplete"):
            audit.verify_package(**package)

    def test_resigned_partial_assignment_manifest_is_rejected(self):
        self.accepted()
        self.accepted(2)
        package = self.package()
        package["manifest"]["payload"]["assignments"].pop(0)
        package["manifest"]["sequencer_signature"] = sign(
            s.manifest_request(self.f["settings"], package["manifest"]["payload"]), "sequencer")
        with self.assertRaisesRegex(s.Error, "assignments_or_retries_incomplete"):
            audit.verify_package(**package)

    def test_signed_checkpoint_cannot_override_biased_ready_selection(self):
        other = self.two_accounts()
        self.ready()
        self.ready(other, key="account")
        self.pick()
        package = self.package()
        event = next(event for event in package["bundle"]["events"] if event["kind"] == "pick_started")
        self.assertEqual(event["payload"]["account"], other)
        event["payload"]["account"] = self.account
        self.rehash(package["bundle"])
        with self.assertRaisesRegex(s.Error, "biased_ready_selection"):
            audit.verify_package(**package)

    def test_independently_known_readiness_cannot_be_hidden(self):
        other = self.two_accounts()
        omitted = self.dispatcher.request(other, 590)
        self.accepted()
        package = self.package()
        audit.verify_package(**package)
        package["trust"]["required_readiness"] = [{"request": omitted, "signature": sign(omitted, "account"),
                                                   "observed_before_event_count": 1}]
        with self.assertRaisesRegex(s.Error, "known_readiness_omitted_or_delayed"):
            audit.verify_package(**package)

    def test_readiness_cannot_be_appended_after_an_independent_observation_bound(self):
        other = self.two_accounts()
        self.accepted()
        request = self.ready(other, key="account")
        package = self.package()
        sequence = package["bundle"]["events"][-1]["sequence"]
        package["trust"]["required_readiness"] = [{"request": request, "signature": sign(request, "account"),
                                                   "observed_before_event_count": sequence - 1}]
        with self.assertRaisesRegex(s.Error, "known_readiness_omitted_or_delayed"):
            audit.verify_package(**package)
        package["trust"]["required_readiness"][0]["observed_before_event_count"] = sequence
        audit.verify_package(**package)

    def test_registry_enumeration_rejects_an_eligible_account_omitted_from_snapshot(self):
        self.accepted()
        package = self.package()
        self.rpc.extra_enrolled.append(a(99))
        with self.assertRaisesRegex(s.Error, "omits_or_adds"):
            audit.verify_package(**package)

    def test_identity_rejects_changed_anchor_metadata_and_unsorted_roster(self):
        identity = self.dispatcher.audit_payload("child", 1, 2)["identity"]
        trust = {"enrollment_block_hash": h(100)}
        for field, value in (("timestamp", 551), ("phase", "bootstrap"), ("ends_at", 601)):
            changed = copy.deepcopy(identity)
            changed["enrollment"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(s.Error, "audit_enrollment_snapshot_changed"):
                audit.validate_identity(changed, s.keccak(s.canonical(changed)), trust, self.rpc)
        self.two_accounts()
        identity = self.dispatcher.audit_payload("child", 1, 2)["identity"]
        identity["subscriptions"].reverse()
        with self.assertRaisesRegex(s.Error, "audit_enrollment_snapshot_changed"):
            audit.validate_identity(identity, s.keccak(s.canonical(identity)), trust, self.rpc)

    def test_independent_checkpoint_rejects_forks_truncation_and_wrong_cursor(self):
        self.accepted()
        first = self.package()
        checkpoint = first["bundle"]["checkpoint"]
        pin = {"event_count": checkpoint["eventCount"], "event_head": checkpoint["eventHead"],
               "assignment_cursor": checkpoint["assignmentCursor"]}
        self.accepted(2)
        package = self.package()
        package["trust"]["minimum_checkpoint"] = pin
        audit.verify_package(**package)
        package["trust"]["minimum_checkpoint"] = {**pin, "assignment_cursor": h(199)}
        with self.assertRaisesRegex(s.Error, "prefix_changed"):
            audit.verify_package(**package)
        first["trust"]["minimum_checkpoint"] = {"event_count": len(package["bundle"]["events"]),
                                                 "event_head": package["bundle"]["checkpoint"]["eventHead"],
                                                 "assignment_cursor": package["bundle"]["checkpoint"]["assignmentCursor"]}
        with self.assertRaisesRegex(s.Error, "checkpoint_truncated"):
            audit.verify_package(**first)
        package["trust"]["minimum_checkpoint"] = pin
        package["bundle"]["events"][0]["at"] += 1
        with self.assertRaisesRegex(s.Error, "event_hash_changed"):
            audit.verify_package(**package)

    def test_known_accepted_receipt_blocks_an_earlier_valid_checkpoint(self):
        self.accepted()
        package = self.package()
        operation = self.accepted(2)
        receipt = s.read_json(self.root / operation / "duty.json", private=True)
        package["trust"]["required_duties"] = [receipt]
        with self.assertRaisesRegex(s.Error, "known_accepted_duty_omitted"):
            audit.verify_package(**package)

    def test_pending_assignments_in_range_wait_but_unrelated_range_does_not(self):
        self.accepted()
        self.offered(2)
        with self.assertRaisesRegex(s.Error, "unresolved_assignment"):
            audit.verify_package(**self.package())
        audit.verify_package(**self.package(1, 1))

    def test_empty_or_idle_history_requires_explicit_control_exemption(self):
        self.ready()
        self.network.responses.append((204, {"x-syscoin-prover-pick-outcome": "unleased"}, b""))
        self.assertEqual(self.dispatcher.pick(), "no_job")
        package = self.package()
        with self.assertRaisesRegex(s.Error, "nonempty_batch_without_service_duty_wait"):
            audit.verify_package(**package)
        result = audit.verify_package(**package, allow_control=True)
        self.assertEqual(result["accounts"][self.account]["offered_opportunities"], 0)
        self.assertEqual(result["zero_duty_reason"], "control")

    def test_empty_native_range_preserves_quiet_chain_progress_without_credit(self):
        package = self.package()
        package["evidence"] = copy.deepcopy(package["evidence"])
        for item in package["evidence"]["batches"]:
            item["output"].update(l1TxCount="0x0", l2TxCount="0x0")
            item["stored"].update(numberOfLayer1Txs="0x0", commitment=s.output_hash(item["output"]))
        result = audit.verify_package(**package)
        self.assertEqual(result["zero_duty_reason"], "empty_native_range")
        self.assertEqual(result["required_duties"], [])
        self.assertEqual(result["accounts"][self.account]["offered_opportunities"], 0)

    def test_exhausted_signed_opportunities_allow_unscored_native_progress(self):
        for attempt in range(self.f["settings"]["duties_per_round"]):
            operation = self.offered(token=h(120 + attempt))
            proof, signature = self.invalid_proof(operation)
            self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
            self.dispatcher.submit(operation, proof, signature)
            package = self.package()
            if attempt + 1 < self.f["settings"]["duties_per_round"]:
                with self.assertRaisesRegex(s.Error, "nonempty_batch_without_service_duty_wait"):
                    audit.verify_package(**package)
            else:
                result = audit.verify_package(**package)
                self.assertEqual(result["zero_duty_reason"], "quota_exhausted")
                self.assertEqual(result["required_duties"], [])

    def test_accepted_favored_duty_cannot_hide_other_nonempty_unassigned_batches(self):
        self.accepted()
        with self.assertRaisesRegex(s.Error, "nonempty_batch_without_service_duty_wait"):
            audit.verify_package(**self.package(1, 2))

    def test_empty_companion_needs_no_duty_or_opportunity(self):
        self.accepted()
        package = self.package(1, 2)
        package["evidence"] = copy.deepcopy(package["evidence"])
        item = package["evidence"]["batches"][1]
        item["output"].update(l1TxCount="0x0", l2TxCount="0x0")
        item["stored"].update(numberOfLayer1Txs="0x0", commitment=s.output_hash(item["output"]))
        result = audit.verify_package(**package)
        self.assertEqual(result["unrewarded_batches"], [])
        self.assertEqual(len(result["required_duties"]), 1)

    def test_mixed_credited_uncredited_range_can_advance_after_quota(self):
        self.accepted()
        for attempt in range(self.f["settings"]["duties_per_round"] - 1):
            operation = self.offered(2, token=h(140 + attempt))
            proof, signature = self.invalid_proof(operation, 2)
            self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
            self.dispatcher.submit(operation, proof, signature)
        result = audit.verify_package(**self.package(1, 2))
        self.assertIsNone(result["zero_duty_reason"])
        self.assertEqual(result["unrewarded_reason"], "quota_exhausted")
        self.assertEqual(result["unrewarded_batches"], [2])
        self.assertEqual(len(result["required_duties"]), 1)

    def test_unsigned_expiry_and_retry_preserve_slot_and_full_history(self):
        self.ready(expires=551)
        first = self.pick()
        self.reload(551)
        self.dispatcher.expire(first)
        second = self.accepted(token=h(123))
        package = self.package()
        result = audit.verify_package(**package)
        self.assertEqual(result["accounts"][self.account]["offered_opportunities"], 1)
        self.assertEqual(package["duties"][0]["slot"], 0)
        self.assertEqual(package["duties"][0]["attempt"], 2)
        self.assertEqual(package["manifest"]["payload"]["retries"][0]["reason"], "expired")
        self.assertEqual(self.dispatcher.state["operations"][second]["status"], "accepted")

    def test_rejected_attempt_consumes_one_opportunity_without_double_credit(self):
        first = self.offered()
        proof, signature = self.invalid_proof(first)
        self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
        self.dispatcher.submit(first, proof, signature)
        self.accepted(token=h(124))
        package = self.package()
        result = audit.verify_package(**package)
        self.assertEqual(result["accounts"][self.account]["offered_opportunities"], 2)
        self.assertEqual(len(result["required_duties"]), 1)
        self.assertEqual(package["manifest"]["payload"]["retries"][0]["reason"], "invalid")

    def test_rejected_operator_proof_cannot_be_reused_without_credit_or_for_another_attempt(self):
        first = self.offered()
        proof, signature = self.signed_proof(first)
        self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
        self.dispatcher.submit(first, proof, signature)
        for attempt in range(self.f["settings"]["duties_per_round"] - 1):
            operation = self.offered(token=h(180 + attempt))
            invalid, signature = self.invalid_proof(operation)
            self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
            self.dispatcher.submit(operation, invalid, signature)
        with self.assertRaisesRegex(s.Error, "rejected_operator_proof_reused"):
            audit.verify_package(**self.package())

    def test_matching_rejected_proof_is_not_cleared_by_a_new_accepted_retry(self):
        first = self.offered()
        proof, signature = self.signed_proof(first)
        self.network.responses.append((422, {"x-syscoin-prover-disposition": "rejected"}, b""))
        self.dispatcher.submit(first, proof, signature)
        self.accepted(token=h(187))
        with self.assertRaisesRegex(s.Error, "rejected_operator_proof_reused"):
            audit.verify_package(**self.package())

    def test_ambiguous_submission_cannot_be_retired_in_a_resigned_ledger(self):
        operation = self.offered()
        proof, signature = self.signed_proof(operation)
        self.network.responses.append((503, {}, b""))
        with self.assertRaises(s.Error):
            self.dispatcher.submit(operation, proof, signature)
        package = self.package()
        event = copy.deepcopy(package["bundle"]["events"][-1])
        event.update(at=590, kind="expired", payload={"operation": operation})
        package["bundle"]["events"].append(event)
        package["bundle"]["checkpoint"]["issuedAt"] = 590
        self.rehash(package["bundle"])
        with self.assertRaisesRegex(s.Error, "ambiguous_assignment_expired"):
            audit.verify_package(**package)

    def test_export_reuses_signed_snapshot_only_while_journal_is_unchanged(self):
        bundle = self.dispatcher.audit_payload("child", 1, 2)
        signature = sign(audit.checkpoint_request(bundle), "sequencer")
        self.reload(551)
        exported = self.dispatcher.export_audit("child", 1, 2, signature, prepared=bundle)
        self.assertEqual(exported["checkpoint"]["issuedAt"], 550)
        self.ready()
        with self.assertRaisesRegex(s.Error, "changed_after_signing_request"):
            self.dispatcher.export_audit("child", 1, 2, signature, prepared=bundle)

    def test_legacy_journals_cannot_claim_a_fabricated_audit_prefix(self):
        del self.dispatcher.state["audit"]
        self.dispatcher.save()
        self.reload()
        with self.assertRaisesRegex(s.Error, "legacy_journal"):
            self.dispatcher.audit_payload("child", 1, 2)

    def test_audit_event_is_saved_atomically_with_dispatch_transition(self):
        request = self.dispatcher.request(self.account, 590)
        with patch.object(d.os, "replace", side_effect=OSError("crash before journal rename")):
            with self.assertRaises(OSError):
                self.dispatcher.ready(request, sign(request, "operator"))
        self.reload()
        self.assertEqual(self.dispatcher.state["audit"]["events"], [])
        self.assertEqual(self.dispatcher.state["accounts"][self.account]["nonce"], 0)
        self.dispatcher.ready(request, sign(request, "operator"))
        self.assertEqual(self.dispatcher.state["audit"]["events"][0]["kind"], "ready")


class SharedLaneAuditTests(unittest.TestCase):
    setUpClass = classmethod(td.SharedLaneDispatcherTests.setUpClass.__func__)
    setUp = td.SharedLaneDispatcherTests.setUp
    ready = td.SharedLaneDispatcherTests.ready
    pick = td.SharedLaneDispatcherTests.pick
    complete = td.SharedLaneDispatcherTests.complete

    def package(self, lane, operation):
        settings = (self.child if lane == "child" else self.gateway)["settings"]
        evidence = copy.deepcopy((self.child if lane == "child" else self.gateway)["evidence"])
        evidence["batches"] = evidence["batches"][:1]
        payload = self.dispatcher.range_manifest_payload(lane, 1, 1)
        manifest = {"payload": payload, "sequencer_signature": sign(s.manifest_request(settings, payload), "sequencer")}
        bundle = self.dispatcher.audit_payload(lane, 1, 1)
        bundle["sequencer_signature"] = sign(audit.checkpoint_request(bundle), "sequencer")
        journal = bundle["journal_id"]
        trust = {"schema_version": 1, "journal_id": journal, "enrollment_block_hash": h(100),
                 "minimum_checkpoint": {"event_count": 0, "event_head": journal, "assignment_cursor": journal},
                 "required_readiness": [], "required_duties": []}
        duties = [s.read_json(self.root / operation / "duty.json", private=True)]
        return {"settings": settings, "evidence": evidence, "manifest": manifest,
                "subscriptions": self.dispatcher.state["subscriptions"], "duties": duties,
                "fri_payload": {"from_batch_number": 1, "to_batch_number": 1, "vk_hash": settings["vk_hash"],
                                "fri_proofs": [(self.child if lane == "child" else self.gateway)["proofs"][0]["proof"]]},
                "bundle": bundle, "trust": trust, "rpc": self.rpc}

    def test_shared_slots_replay_across_lanes_without_cross_credit_or_map_order_dependence(self):
        child, gateway = self.complete("child"), self.complete("gateway")
        child_package, gateway_package = self.package("child", child), self.package("gateway", gateway)
        for package in (child_package, gateway_package):
            result = audit.verify_package(**package)
            self.assertEqual(result["accounts"][self.account]["offered_opportunities"], 2)
            self.assertEqual(len(result["required_duties"]), 1)
        lanes = gateway_package["bundle"]["identity"]["lanes"]
        gateway_package["bundle"]["identity"]["lanes"] = {"gateway": lanes["gateway"], "child": lanes["child"]}
        audit.verify_package(**gateway_package)
        gateway_package["duties"] = child_package["duties"]
        with self.assertRaisesRegex(s.Error, "accepted_duties_incomplete"):
            audit.verify_package(**gateway_package)


if __name__ == "__main__":
    unittest.main()
