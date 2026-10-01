import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import roster
import service as s
from test_service import a, h


def candidates(count):
    return [{"index": index, "account": a(index + 1), "operator": a(index + 100), "beneficiary": a(index + 200)}
            for index in range(count)]


def registry_frontier_root(values):
    frontier = [None] * 32
    for count, candidate in enumerate(values, 1):
        node, size = s.raw_hex(s.wrapper_leaf(candidate)), count
        for level in range(32):
            if size & 1:
                frontier[level] = node
                break
            node = roster.pair(frontier[level], node)
            size >>= 1
    node, zero, size = bytes(32), bytes(32), len(values)
    for level in range(32):
        node = roster.pair(frontier[level], node) if size & 1 else roster.pair(node, zero)
        zero = roster.pair(zero, zero)
        size >>= 1
    return "0x" + node.hex()


class RosterTests(unittest.TestCase):
    def test_cli_builds_the_complete_artifact_without_chain_calls(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, destination = root / "candidates.json", root / "roster.json"
            values = candidates(2)
            s.write_new(source, values)
            with patch("sys.argv", ["roster.py", "build", "--period", "5", "--candidates", str(source), "--output", str(destination)]):
                roster.main()
            self.assertEqual(s.read_json(destination), roster.construct(5, values))

    def test_roots_match_registry_frontier_and_all_proofs_verify(self):
        for count in (1, 2, 3, 4, 5):
            with self.subTest(count=count):
                values = candidates(count)
                artifact = roster.construct(5, values)
                self.assertEqual(artifact["root"], registry_frontier_root(values))
                self.assertEqual(artifact["count"], count)
                expected = {field: artifact[field] for field in ("period", "root", "count")}
                self.assertEqual(roster.authenticate(artifact, expected), artifact)
                for candidate, proof in zip(values, artifact["proofs"]):
                    self.assertEqual(len(proof), 32)
                    s.check_candidate(candidate, proof, artifact["root"])
                    self.assertEqual(roster.selected(artifact, expected, candidate["index"]),
                                     {"candidate": candidate, "candidate_proof": proof})

    def test_indices_must_be_complete_and_accounts_sorted_unique(self):
        for modify, reason in ((lambda v: v[1].update(index=0), "noncontiguous_roster_indices"),
            (lambda v: v[1].update(index=2), "noncontiguous_roster_indices"),
            (lambda v: v[1].update(account=v[0]["account"]), "roster_accounts_not_sorted_unique"),
            (lambda v: v.reverse(), "noncontiguous_roster_indices")):
            values = candidates(2)
            modify(values)
            with self.assertRaisesRegex(s.Error, reason):
                roster.construct(5, values)

    def test_every_leaf_and_proof_are_authenticated(self):
        artifact = roster.construct(5, candidates(3))
        expected = {field: artifact[field] for field in ("period", "root", "count")}
        bad = copy.deepcopy(artifact)
        bad["candidates"][2]["beneficiary"] = a(240)
        with self.assertRaisesRegex(s.Error, "roster_root_mismatch"):
            roster.selected(bad, expected, 0)
        bad = copy.deepcopy(artifact)
        bad["proofs"][2][0] = h(241)
        with self.assertRaisesRegex(s.Error, "roster_proof_mismatch"):
            roster.selected(bad, expected, 0)
        bad["proofs"][2] = []
        with self.assertRaisesRegex(s.Error, "invalid_roster_proof"):
            roster.authenticate(bad, expected)
        with self.assertRaisesRegex(s.Error, "selected_wrapper_out_of_range"):
            roster.selected(artifact, expected, 3)

    def test_invalid_counts_zero_identities_and_noninteger_indices_fail(self):
        with self.assertRaisesRegex(s.Error, "invalid_roster_count"):
            roster.construct(5, [])
        with self.assertRaisesRegex(s.Error, "invalid_roster_count"):
            roster.construct(5, [None] * (roster.MAX_CANDIDATES + 1))
        for field, value in (("operator", s.ZERO_ADDRESS), ("index", False), ("index", "0x0")):
            values = candidates(1)
            values[0][field] = value
            with self.assertRaises(s.Error):
                roster.construct(5, values)


if __name__ == "__main__":
    unittest.main()
