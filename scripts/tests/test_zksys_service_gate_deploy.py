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
            'timelock': C, 'timelock_code_hash': H,
            'message_sink': deploy.ZERO, 'message_sink_code_hash': Z, 'sequencer': E, 'policy_hash': H,
            'production_vk_hash': H, 'child_chain_id': 57001,
            'factory_salt': H,
            'create2_code_hash': H, 'gas_limit': 9000000, 'gas_price': 10,
            'keystore': '/private/keystore', 'password_file': '/private/password'}


class ConfigTest(unittest.TestCase):
    def test_only_supported_configuration_and_sink_binding(self):
        valid = deploy.config(sample())
        self.assertEqual(valid['message_sink'], deploy.ZERO)
        for field, value in [('message_sink_code_hash', H), ('child_chain_id', 57),
                             ('gas_limit', True), ('extra', 1)]:
            with self.subTest(field=field):
                changed = copy.deepcopy(valid)
                changed[field] = value
                with self.assertRaises(deploy.TransactionError): deploy.config(changed)

    def test_plan_matches_factory_address_constructor_and_commitment(self):
        cfg = deploy.config(sample())
        with mock.patch.object(deploy, 'bytecode', return_value='0x60006000'):
            plan = deploy.build(cfg)
        self.assertEqual(plan['gate'], deploy.cast('compute-address', plan['factory'], '--nonce', '1').split()[-1].lower())
        gate_args = '0x' + plan['gate_init'][len('0x60006000'):]
        types = 'address,address,address,bytes32,bytes32,address'
        self.assertEqual(deploy.cast('abi-decode', f'f()({types})', gate_args).lower().splitlines(),
                         [B, C, E, H, H, deploy.ZERO])
        self.assertEqual(plan['commitment'], deploy.digest(plan['gate_init']))

    def test_plan_produces_unverified_addresses_without_rpc_or_journal(self):
        plan = {'factory': A, 'gate': C, 'commitment': H}
        output = io.StringIO()
        with mock.patch.object(sys, 'argv', ['deploy_gate.py', '--config', '/private/config', '--plan']), \
             mock.patch.object(deploy.service, 'read_json', return_value=sample()), \
             mock.patch.object(deploy, 'build', return_value=plan), \
             mock.patch.object(deploy.relay, 'Rpc', side_effect=AssertionError('plan must not access RPC')), \
             contextlib.redirect_stdout(output):
            deploy.main()
        self.assertEqual(json.loads(output.getvalue()), {'status': 'unverified_plan', **plan})

    def test_pins_the_root_sink_to_the_planned_gate(self):
        cfg = deploy.config({**sample(), 'message_sink': D, 'message_sink_code_hash': H})
        rpc = mock.Mock()
        rpc.call.side_effect = ['0x39', {'hash': H}]
        with mock.patch.object(deploy, 'rpc_code', return_value='0x01'), \
             mock.patch.object(deploy, 'digest', return_value=H), \
             mock.patch.object(deploy, 'call_word', side_effect=[hex(cfg['child_chain_id']), H]), \
             mock.patch.object(deploy, 'assert_address') as binding:
            deploy.verify_inputs(rpc, cfg, {'gate': A})
        binding.assert_called_once_with(rpc, D, 'publisher()', A)


if __name__ == '__main__':
    unittest.main()
