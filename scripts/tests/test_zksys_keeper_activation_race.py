import copy
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import zksys_keeper_hooks as hooks_module
import zksys_keeper_runner as runner_module
import zksys_keeper_transactions as tx_module

A = '0x' + '11' * 20
B = '0x' + '22' * 20
H = '0x' + '33' * 32
BLOCK = '0x' + '44' * 32
RAW = '0x1234'


def word(value):
    return value.to_bytes(32, 'big').hex()


class Rpc:
    def __init__(self):
        self.final = 4
        self.receipt = None

    def __call__(self, method, args):
        if method == 'eth_chainId':
            return '0x39'
        if method == 'eth_getTransactionCount':
            return '0x0'
        if method == 'eth_sendRawTransaction':
            return H
        if method == 'eth_getTransactionReceipt':
            return self.receipt
        if method == 'eth_getBlockByNumber':
            return {'number': hex(self.final), 'hash': BLOCK} if args[0] == 'finalized' else {'hash': BLOCK}
        raise AssertionError(method)


class ActivationRaceTest(unittest.TestCase):
    def setUp(self):
        self.journal = {}
        self.saved = []
        self.rpc = Rpc()
        self.decoded = None

        def sign(tx):
            self.decoded = tx
            return RAW

        self.driver = tx_module.TransactionDriver(self.journal, lambda state: self.saved.append(copy.deepcopy(state)),
                                                  sign, lambda _: self.decoded, lambda _: H)
        self.driver.begin('l2', self.rpc, 57, A, B, '0x1234', 0, 50000, 7,
                          kind='activation', metadata={'account': A, 'target': 100})
        self.rpc.receipt = {'transactionHash': H, 'blockHash': BLOCK, 'blockNumber': '0x5', 'status': '0x0'}

    def test_only_finalized_satisfied_activation_is_archived_and_removal_can_proceed(self):
        class Hooks:
            satisfied = True
            def verify_deployment(self, cfg): return True
            def discover(self): return {'accounts': [A, B], 'changes': [{'account': B}]}
            def verify_activation_superseded(self, receipt, entry): return self.satisfied
            def bridge_request(self, batch): return {}
        class Bridge:
            def __init__(self): self.batches = []
            def begin(self, name, rpc, kind, metadata, **kwargs): self.batches.append(metadata['accounts'])
        hooks, bridge = Hooks(), Bridge()
        runner = runner_module.KeeperRunner(self.journal, lambda state: self.saved.append(copy.deepcopy(state)),
                                           bridge, self.driver, self.rpc, self.rpc, hooks, {'max_batch_size': 512})
        self.assertEqual(runner.run_once(), 'activation_pending')
        self.assertIn('l2', self.journal['transactions'])
        self.rpc.final = 5
        hooks.satisfied = False
        with self.assertRaisesRegex(tx_module.TransactionError, 'not independently satisfied'):
            runner.run_once()
        self.assertEqual(self.journal['transactions']['l2']['state'], 'failed')
        self.assertNotIn('superseded_activations', self.journal)
        # A restart must retain the finalized failure and still reconcile it.
        persisted = copy.deepcopy(self.saved[-1])
        self.driver.journal = persisted
        runner.journal = persisted
        hooks.satisfied = True
        self.assertEqual(runner.run_once(), 'activation_superseded')
        self.assertNotIn('l2', persisted['transactions'])
        self.assertEqual(persisted['superseded_activations'][H]['target'], 100)
        self.assertEqual(runner.run_once(), 'bridge_submitted')
        self.assertEqual(bridge.batches, [[B]])

    def test_finalized_failure_never_archives_without_verification_or_after_reorg(self):
        self.rpc.final = 5
        with self.assertRaises(tx_module.FinalizedRevert):
            self.driver.receipt('l2', self.rpc)
        with self.assertRaisesRegex(tx_module.TransactionError, 'not independently satisfied'):
            self.driver.supersede_activation('l2', self.rpc, lambda *_: False)
        self.rpc.receipt = None
        with self.assertRaisesRegex(tx_module.TransactionError, 'receipt changed'):
            self.driver.supersede_activation('l2', self.rpc, lambda *_: True)
        self.assertIn('l2', self.journal['transactions'])


class SupersededStateTest(unittest.TestCase):
    def test_requires_finalized_membership_active_weight_and_no_pending_sentry(self):
        later = '0x' + '55' * 32
        class L2:
            weight = 100
            pending = 0
            final = 7
            reorg = False
            def call(self, method, args):
                if method == 'eth_getBlockByNumber':
                    return {'number': hex(self.final),
                            'hash': BLOCK if args[0] != 'finalized' and self.reorg else later}
                selector = args[0]['data'][:10]
                if selector == '0x' + hooks_module.k.selector('weightComponents(address)').hex():
                    return '0x' + word(0) + word(self.weight)
                return '0x' + word(0) + word(0) + word(self.pending) + word(0)
        l2 = L2()
        hook = hooks_module.Hooks(None, None, l2, {}, {'membership': A, 'weights': B}, lambda _: None)
        entry = {'metadata': {'account': A, 'target': 100}}
        receipt = {'blockHash': BLOCK, 'blockNumber': '0x5'}
        with patch.object(hooks_module.k, 'member_at', return_value=(8, 100)) as member_at:
            self.assertTrue(hook.verify_activation_superseded(receipt, entry))
            self.assertEqual(member_at.call_args.args[3], later)
            l2.weight = 99
            self.assertFalse(hook.verify_activation_superseded(receipt, entry))
            l2.weight = 100
            l2.pending = 101
            self.assertFalse(hook.verify_activation_superseded(receipt, entry))
            l2.pending = 0
            l2.final = 4
            self.assertFalse(hook.verify_activation_superseded(receipt, entry))
            l2.final = 7
            l2.reorg = True
            with self.assertRaisesRegex(tx_module.TransactionError, 'snapshot changed'):
                hook.verify_activation_superseded(receipt, entry)
            l2.reorg = False
        with patch.object(hooks_module.k, 'member_at', return_value=(0, 0)):
            self.assertFalse(hook.verify_activation_superseded(receipt, entry))
        with patch.object(hooks_module.k, 'member_at', return_value=(8, 99)):
            self.assertFalse(hook.verify_activation_superseded(receipt, entry))


if __name__ == '__main__':
    unittest.main()
