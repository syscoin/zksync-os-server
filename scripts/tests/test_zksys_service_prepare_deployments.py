import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'prover-service'))
import prepare_deployments as prepare
from scripts.tests.test_zksys_service_component_deploy import sink
from scripts.tests.test_zksys_service_gate_deploy import sample


class PreparationTest(unittest.TestCase):
    def manifest(self):
        gate = sample()
        root_sink = sink()
        gate['message_sink'] = {'ref': 'sink.component'}
        gate['message_sink_code_hash'] = gate['chain_code_hash']
        root_sink['publisher'] = {'ref': 'gate.gate'}
        root_sink['factory_salt'] = '0x' + '44' * 32
        return {'schema_version': 1, 'stages': [
            {'id': 'gate', 'type': 'gate', 'config': gate},
            {'id': 'sink', 'type': 'component', 'config': root_sink}]}

    def prepare(self, manifest):
        with patch.object(prepare.deploy_gate, 'bytecode', return_value='0x60006000'), \
             patch.object(prepare.deploy_component, 'component_bytecode', return_value='0x60006000'):
            return prepare.prepare(manifest)

    def test_cyclic_addresses_and_private_output(self):
        result = self.prepare(self.manifest())
        self.assertEqual(result[0]['config']['message_sink'], result[1]['plan']['component'])
        self.assertEqual(result[1]['config']['publisher'], result[0]['plan']['gate'])
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / 'generated'
            prepare.write_output(outdir, result)
            self.assertEqual(os.stat(outdir / 'gate.json').st_mode & 0o777, 0o600)
            self.assertEqual(os.stat(outdir / 'gate-state').st_mode & 0o777, 0o700)
            self.assertEqual(json.loads((outdir / 'plans.json').read_text())['status'], 'unverified_plan')
            with self.assertRaises(FileExistsError):
                prepare.write_output(outdir, result)

    def test_unknown_references_and_duplicate_factories_fail(self):
        manifest = self.manifest()
        manifest['stages'][0]['config']['message_sink'] = {'ref': 'missing.component'}
        with self.assertRaisesRegex(prepare.deploy_gate.TransactionError, 'unknown stage reference'):
            self.prepare(manifest)
        manifest = self.manifest()
        manifest['stages'].append({**manifest['stages'][0], 'id': 'second-gate'})
        with self.assertRaises(prepare.deploy_gate.TransactionError):
            self.prepare(manifest)


if __name__ == '__main__':
    unittest.main()
