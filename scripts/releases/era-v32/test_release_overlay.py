import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("overlay", ROOT / "check-release-overlay.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def inventory():
    names = sorted(MODULE.ROWS) + [f"contract-{i}" for i in range(276)]
    return [dict(contractName=name, zkBytecodePath=f"zk/{name}", evmBytecodePath=f"evm/{name}",
                 evmDeployedBytecodeLength=10, **{key: "0x" + "1" * 64 for key in MODULE.FIELDS})
            for name in names]


class OverlayTests(unittest.TestCase):
    def test_two_exact_rows_permitted(self):
        before = inventory()
        after = copy.deepcopy(before)
        for row in after[:2]:
            row["evmBytecodeHash"] = "0x" + "2" * 64
            row["evmDeployedBytecodeLength"] = 20
        MODULE.check_inventory(before, after)

    def test_helper_hash_need_not_change(self):
        MODULE.check_inventory(inventory(), inventory())

    def test_other_row_rejected(self):
        before = inventory()
        after = copy.deepcopy(before)
        after[2]["evmBytecodeHash"] = "0x" + "2" * 64
        with self.assertRaisesRegex(ValueError, "unexpected inventory"):
            MODULE.check_inventory(before, after)

    def test_path_change_rejected(self):
        before = inventory()
        after = copy.deepcopy(before)
        after[0]["evmBytecodePath"] += "-changed"
        with self.assertRaisesRegex(ValueError, "unexpected inventory"):
            MODULE.check_inventory(before, after)

    def test_duplicate_identity_rejected(self):
        rows = inventory()
        rows[2] = copy.deepcopy(rows[0])
        with self.assertRaisesRegex(ValueError, "duplicate inventory"):
            MODULE.inventory_projection(rows)

    def test_zero_hash_rejected(self):
        rows = inventory()
        rows[0]["evmBytecodeHash"] = "0x" + "0" * 64
        with self.assertRaisesRegex(ValueError, "invalid generated"):
            MODULE.inventory_projection(rows)

    def test_boolean_length_rejected(self):
        rows = inventory()
        rows[0]["evmDeployedBytecodeLength"] = True
        with self.assertRaisesRegex(ValueError, "invalid deployed"):
            MODULE.inventory_projection(rows)

    def test_wrong_digest_rejected(self):
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            MODULE.checked_file(ROOT / "generated-verifier-overlay.patch", "0" * 64)

    def test_symlink_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            link = Path(directory) / "overlay"
            link.symlink_to(ROOT / "generated-verifier-overlay.patch")
            with self.assertRaisesRegex(ValueError, "non-symlink"):
                MODULE.checked_file(link, MODULE.OVERLAY_SHA)


if __name__ == "__main__":
    unittest.main()
