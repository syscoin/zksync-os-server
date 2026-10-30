import tempfile
import unittest
from pathlib import Path

import publication_config as publication
import service as s
from test_keeper import NativeRpc, setup
from test_service import fixture, h


class PublicationConfigTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixture()
        self.config, _, item = setup(self.fixture)
        self.rpc = NativeRpc(self.fixture, self.config, item)
        self.rpc.opened = False
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)

    def preview(self, lane='child', execution=None, settlement=None, vk=None):
        settings = self.config['settings']
        return publication.preview(self.config, self.rpc, self.rpc.now, lane,
            s.uint(settings['execution_chain_id']) if execution is None else execution,
            s.uint(settings['settlement_chain_id']) if settlement is None else settlement,
            str(Path(self.directory.name).resolve()), vk or settings['vk_hash'])

    def test_preview_checks_canonical_contracts_and_emits_one_lane(self):
        output = self.preview()['service_publication']
        self.assertEqual(output['gate'], self.config['settings']['proof_gate'])
        self.assertEqual(output['coordinator'], self.config['settings']['coordinator'])
        self.assertEqual(output['directory'], str(Path(self.directory.name).resolve()))
        self.assertFalse(self.rpc.sent)

    def test_rejects_wrong_lane_chain_and_vk(self):
        for kwargs in ({'lane': 'gateway'}, {'execution': 999}, {'settlement': 999}, {'vk': h(55)}):
            with self.subTest(kwargs=kwargs), self.assertRaises(s.Error):
                self.preview(**kwargs)

    def test_bootstrap_uses_zero_coordinator_pins(self):
        self.config['settings']['coordinator'] = s.ZERO_ADDRESS
        self.config['policy']['coordinator_code_hash'] = s.ZERO
        self.rpc.item['mode'] = 'bootstrap'
        self.rpc.guard_work_missing = True
        output = self.preview()['service_publication']
        self.assertEqual(output['coordinator'], s.ZERO_ADDRESS)
        self.assertEqual(output['coordinator_code_hash'], s.ZERO)

    def test_rejects_configuration_with_uninstalled_coordinator(self):
        self.rpc.item['mode'] = 'bootstrap'
        self.rpc.guard_work_missing = True
        with self.assertRaisesRegex(s.Error, 'phase_pin_mismatch'):
            self.preview()

    def test_rejects_stale_role_code_or_reorg(self):
        for field in ('bad_role', 'bad_code', 'reorg_anchor'):
            with self.subTest(field=field):
                setattr(self.rpc, field, True)
                with self.assertRaises(s.Error):
                    self.preview()
                setattr(self.rpc, field, False)

    def test_source_vk_is_unambiguous(self):
        self.assertRegex(publication.local_vk_hash(), r'^0x[0-9a-f]{64}$')


if __name__ == '__main__':
    unittest.main()
