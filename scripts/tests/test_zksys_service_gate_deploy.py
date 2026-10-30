import copy
import contextlib
import io
import json
import importlib.util
import pathlib
import sys
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'scripts' / 'prover-service'))
spec = importlib.util.spec_from_file_location('service_gate_deploy', ROOT / 'scripts/prover-service/deploy_gate.py')
deploy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deploy)

A = '0x' + '11' * 20
B = '0x' + '22' * 20
C = '0x' + '33' * 20
D = '0x' + '44' * 20
E = '0x' + '55' * 20
H = '0x' + '66' * 32
Z = '0x' + '00' * 32


def sample():
    return {'chain_id': 57, 'genesis_hash': H, 'sender': A, 'chain': B, 'chain_code_hash': H,
            'timelock': C, 'timelock_code_hash': H, 'checkpoint': D, 'checkpoint_code_hash': H,
            'message_sink': deploy.ZERO, 'message_sink_code_hash': Z, 'sequencer': E, 'policy_hash': H,
            'production_vk_hash': H, 'child_chain_id': 57001, 'max_snapshot_age': 600,
            'max_proof_work_seconds': 1200, 'max_priority_per_package': 64, 'factory_salt': H,
            'create2_code_hash': H, 'gas_limit': 9000000, 'gas_price': 10,
            'keystore': '/private/keystore', 'password_file': '/private/password'}


class ConfigTest(unittest.TestCase):
    def test_only_supported_configuration_and_sink_binding(self):
        valid = deploy.config(sample())
        self.assertEqual(valid['message_sink'], deploy.ZERO)
        for field, value in [('message_sink_code_hash', H), ('child_chain_id', 57),
                             ('max_priority_per_package', 1025), ('gas_limit', True), ('extra', 1)]:
            with self.subTest(field=field):
                changed = copy.deepcopy(valid)
                changed[field] = value
                with self.assertRaises(deploy.TransactionError): deploy.config(changed)

    def test_plan_uses_reciprocal_predicted_addresses_and_actual_constructor_encoding(self):
        with mock.patch.object(deploy, 'bytecode', return_value='0x60006000'):
            first = deploy.build(deploy.config(sample()))
            second = deploy.build(deploy.config(sample()))
        self.assertEqual(first, second)
        self.assertEqual(first['guard'], deploy.cast('compute-address', first['factory'], '--nonce', '1').split()[-1].lower())
        self.assertEqual(first['gate'], deploy.cast('compute-address', first['factory'], '--nonce', '2').split()[-1].lower())
        guard_args = '0x' + first['guard_init'][len('0x60006000'):]
        decoded = deploy.cast('abi-decode', 'f()(address,address,address,bytes32,uint64,uint64,uint64)', guard_args)
        self.assertIn(first['gate'], decoded.lower())
        gate_args = '0x' + first['gate_init'][len('0x60006000'):]
        decoded = deploy.cast('abi-decode', 'f()(address,address,address,bytes32,bytes32,address,address)', gate_args)
        self.assertIn(first['guard'], decoded.lower())

    def test_plan_produces_unverified_addresses_without_rpc_or_journal(self):
        plan = {'factory': A, 'guard': B, 'gate': C, 'commitment': H}
        output = io.StringIO()
        with mock.patch.object(sys, 'argv', ['deploy_gate.py', '--config', '/private/config', '--plan']), \
             mock.patch.object(deploy.service, 'read_json', return_value=sample()), \
             mock.patch.object(deploy, 'build', return_value=plan), \
             mock.patch.object(deploy.relay, 'Rpc', side_effect=AssertionError('plan must not access RPC')), \
             contextlib.redirect_stdout(output):
            deploy.main()
        self.assertEqual(json.loads(output.getvalue()), {'status': 'unverified_plan', **plan})

    def test_queue_drain_is_required_before_broadcast(self):
        values = {'getTotalBatchesVerified()': 5, 'getTotalBatchesExecuted()': 5,
                  'getFirstUnprocessedPriorityTx()': 2, 'getTotalPriorityTxs()': 2,
                  'isPriorityQueueActive()': 0}
        def word(_rpc, _address, signature):
            return '0x' + format(values[signature], '064x')
        with mock.patch.object(deploy, 'call_word', side_effect=word):
            deploy.verify_queue_drained(None, sample())
            for field, value in [('getTotalBatchesExecuted()', 4), ('getTotalPriorityTxs()', 3),
                                 ('isPriorityQueueActive()', 1)]:
                old = values[field]
                values[field] = value
                with self.assertRaises(deploy.TransactionError): deploy.verify_queue_drained(None, sample())
                values[field] = old


if __name__ == '__main__':
    unittest.main()
