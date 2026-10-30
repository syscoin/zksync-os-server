import contextlib
import copy
import importlib.util
import io
import json
import pathlib
import sys
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'scripts' / 'prover-service'))
spec = importlib.util.spec_from_file_location('service_component_deploy', ROOT / 'scripts/prover-service/deploy_component.py')
deploy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deploy)

A = '0x' + '11' * 20
B = '0x' + '22' * 20
C = '0x' + '33' * 20
D = '0x' + '44' * 20
E = '0x' + '55' * 20
H = '0x' + '66' * 32


def base():
    return {'chain_id': 57, 'genesis_hash': H, 'sender': A, 'factory_salt': H, 'create2_code_hash': H,
            'gas_limit': 9000000, 'gas_price': 10, 'keystore': '/private/keystore',
            'password_file': '/private/password', 'policy_hash': H}


def source():
    return {**base(), 'kind': 'root_priority_source', 'mailbox': B, 'mailbox_code_hash': H,
            'bridgehub': C, 'bridgehub_code_hash': H, 'child_chain_id': 57057, 'gateway_receiver': D,
            'gateway_chain_id': 57001, 'inclusion_delay': 100}


def receiver():
    return {**base(), 'kind': 'gateway_checkpoint_receiver', 'chain_id': 57001,
            'root_source': B, 'child_chain_id': 57057}


def sink():
    return {**base(), 'kind': 'root_message_sink', 'publisher': A, 'gateway_gate': E, 'bridgehub': C, 'bridgehub_code_hash': H,
            'gateway_chain_id': 57001, 'gateway_chain_address': B, 'gateway_chain_code_hash': H,
            'registry_chain_id': 57057, 'registry_receiver': D}


def roster():
    return {**base(), 'kind': 'roster_receiver', 'chain_id': 57001, 'child_chain_id': 57057,
            'child_registry': A, 'message_sender': B, 'mailbox': C, 'mailbox_code_hash': H,
            'mailbox_chain_id': 57057, 'start_time': 10000, 'period_seconds': 86400, 'first_service_period': 0}


def draw_receiver():
    return {**base(), 'kind': 'root_draw_receiver', 'chain_id': 57001, 'root_source': B}


def draw_source():
    return {**base(), 'kind': 'root_draw', 'mailbox': B, 'mailbox_code_hash': H, 'bridgehub': C,
            'bridgehub_code_hash': H, 'gateway_coordinator': A, 'gateway_receiver': D,
            'gateway_chain_id': 57001, 'future_blocks': 3, 'confirmations': 2}


def native_draw():
    return {**base(), 'kind': 'native_root_draw', 'coordinator': A, 'future_blocks': 3, 'confirmations': 2}


def coordinator():
    return {**base(), 'kind': 'coordinator', 'chain_id': 57001, 'gate': A, 'gate_code_hash': H,
            'root_source': B, 'root_source_code_hash': H, 'roster_source': C, 'roster_source_code_hash': H,
            'sequencer': D, 'child_chain_id': 57057, 'child_chain_address': E, 'child_chain_code_hash': H,
            'production_vk_hash': H, 'initial_parent': H, 'first_batch': 4, 'first_service_period': 0,
            'turn_seconds': 300, 'native_root_draw': False}


class ComponentTest(unittest.TestCase):
    def test_reject_invalid_or_ambiguous_lane_identities(self):
        for original, field, value in [(source(), 'gateway_receiver', deploy.ZERO),
                                       (source(), 'child_chain_id', 57),
                                       (source(), 'inclusion_delay', 2**64),
                                       (receiver(), 'child_chain_id', 57001),
                                       (sink(), 'registry_chain_id', 57001),
                                       (sink(), 'bridgehub_code_hash', '0x' + '00' * 32),
                                       (roster(), 'mailbox_chain_id', 57001),
                                       (roster(), 'first_service_period', True),
                                       (draw_source(), 'gateway_receiver', deploy.ZERO),
                                       (native_draw(), 'confirmations', 256),
                                       (coordinator(), 'native_root_draw', 1),
                                       (coordinator(), 'first_batch', 0)]:
            with self.subTest(kind=original['kind'], field=field):
                changed = copy.deepcopy(original)
                changed[field] = value
                with self.assertRaises(deploy.TransactionError): deploy.component_config(changed)

    def test_actual_constructor_encodings_and_prediction_are_independent(self):
        with mock.patch.object(deploy.common, 'bytecode', return_value='0x60006000'), \
             mock.patch.object(deploy, 'component_bytecode', return_value='0x60006000'):
            plans = [deploy.build(deploy.component_config(item)) for item in (source(), receiver(), sink())]
            changed = source()
            changed['gateway_receiver'] = B
            updated = deploy.build(deploy.component_config(changed))
        self.assertEqual(plans[0]['component'], updated['component'])
        self.assertNotEqual(plans[0]['commitment'], updated['commitment'])
        signatures = ['f()(address,address,address,uint256,bytes32,uint64)', 'f()(address,uint256,bytes32)',
                      'f()((address,address,address,uint256,address,uint256,address,bytes32))']
        expected = [D, B, D]
        for plan, signature, target in zip(plans, signatures, expected):
            args = '0x' + plan['component_init'][len('0x60006000'):]
            decoded = deploy.common.cast('abi-decode', signature, args)
            self.assertIn(target, decoded.lower())
            self.assertEqual(plan['component'], deploy.common.cast('compute-address', plan['factory'], '--nonce', '1').split()[-1].lower())

    def test_plan_has_no_rpc_or_journal(self):
        plan = {'factory': A, 'component': B, 'commitment': H}
        output = io.StringIO()
        with mock.patch.object(sys, 'argv', ['deploy_component.py', '--config', '/private/config', '--plan']), \
             mock.patch.object(deploy.service, 'read_json', return_value=source()), \
             mock.patch.object(deploy, 'build', return_value=plan), \
             mock.patch.object(deploy.relay, 'Rpc', side_effect=AssertionError('plan must not access RPC')), \
             contextlib.redirect_stdout(output):
            deploy.main()
        self.assertEqual(json.loads(output.getvalue()), {'status': 'unverified_plan', 'kind': 'root_priority_source', **plan})

    def test_source_checks_queue_but_receiver_does_not(self):
        with mock.patch.object(deploy.common, 'verify_queue_drained') as check:
            deploy.verify_source_drained(None, source())
            check.assert_called_once_with(None, {'chain': B})
            check.reset_mock()
            deploy.verify_source_drained(None, receiver())
            check.assert_not_called()

    def test_roster_draw_and_coordinator_constructor_graph(self):
        entries = (roster(), draw_receiver(), draw_source(), native_draw(), coordinator())
        signatures = (
            'f()((uint256,address,address,address,bytes32,uint256,uint256,uint64))',
            'f()(address)',
            'f()(address,address,address,address,uint256,uint64,uint64)',
            'f()(address,uint64,uint64)',
            'f()((address,address,address,address,uint256,address,bytes32,bytes32,bytes32,uint64,uint64,uint64,bool))',
        )
        expected = (C, B, D, A, E)
        with mock.patch.object(deploy.common, 'bytecode', return_value='0x60006000'), \
             mock.patch.object(deploy, 'component_bytecode', return_value='0x60006000'):
            plans = [deploy.build(deploy.component_config(item)) for item in entries]
        for plan, signature, address in zip(plans, signatures, expected):
            args = '0x' + plan['component_init'][len('0x60006000'):]
            self.assertIn(address, deploy.common.cast('abi-decode', signature, args).lower())

    def test_coordinator_parent_must_not_advance_after_bootstrap(self):
        cfg = deploy.component_config(coordinator())
        values = {'serviceActive()': 0, 'priorityWorkId()': 0, 'lastAcceptedPackage()': int(H, 16),
                  'getTotalBatchesVerified()': 3}
        def word(_rpc, _address, signature):
            return '0x' + format(values[signature], '064x')
        with mock.patch.object(deploy.common, 'call_word', side_effect=word), \
             mock.patch.object(deploy.common, 'call_address', return_value=deploy.ZERO):
            deploy.verify_coordinator_parent(None, cfg, {'component': B})
            for key in ('priorityWorkId()', 'lastAcceptedPackage()', 'getTotalBatchesVerified()'):
                old = values[key]
                values[key] = 1
                with self.assertRaises(deploy.TransactionError):
                    deploy.verify_coordinator_parent(None, cfg, {'component': B})
                values[key] = old


if __name__ == '__main__':
    unittest.main()
