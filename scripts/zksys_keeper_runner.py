"""Bounded serial registry relay and activation loop.

All I/O is injected; callers must exclusively lock and durably persist the
journal, authenticate deployment identities on every pass, and provide verified
fee quotes and receipt-state checks. One call performs at most one new send.
"""
import zksys_membership_keeper as membership
from zksys_keeper_transactions import FinalizedRevert, TransactionError, bridge_priority_hash, priority_payload


class KeeperRunner:
    def __init__(self, journal, save, l1_driver, l2_driver, l1_rpc, l2_rpc, hooks, config, *, sync_only=False):
        self.journal = journal
        self.save = save
        self.l1_driver = l1_driver
        self.l2_driver = l2_driver
        self.l1_rpc = l1_rpc
        self.l2_rpc = l2_rpc
        self.hooks = hooks
        self.config = config
        self.sync_only = sync_only

    def run_once(self):
        cfg = self.config
        transactions = self.journal.setdefault("transactions", {})
        if self.sync_only and 'l2' in transactions:
            raise TransactionError("sync-only cannot resume a journaled activation; it cannot cancel an already broadcast transaction")
        if self.hooks.verify_deployment(cfg) is not True:
            raise TransactionError("deployment identity verification failed")
        discovery = self.hooks.discover()
        accounts = [membership.address(a) for a in discovery['accounts']]
        if len(accounts) != len(set(accounts)):
            raise TransactionError("duplicate discovered account")
        self.journal['ever_seen'] = sorted(set(accounts) | set(self.journal.get('ever_seen', [])))
        # Discovery must be durable even when delivery is stalled or the next
        # broadcast times out. The serial outbox still delays new submissions.
        self.save(self.journal)
        if 'l1' in transactions:
            return self._resume_bridge()
        if 'l2' in transactions:
            return self._resume_activation()

        changes = discovery['changes']
        if changes:
            size = cfg['max_batch_size']
            if type(size) is not int or not 1 <= size <= 512:
                raise TransactionError("invalid batch size")
            batch = [membership.address(item['account']) for item in changes[:size]]
            if len(batch) != len(set(batch)) or not set(batch).issubset(accounts):
                raise TransactionError("invalid reconciliation batch")
            request = self.hooks.bridge_request(batch)
            self.l1_driver.begin('l1', self.l1_rpc, kind='bridge', metadata={'accounts': batch}, **request)
            return 'bridge_submitted'
        if self.sync_only:
            return 'sync_only_idle'
        for account in accounts:
            if self.hooks.activation_ready(account):
                request = self.hooks.activation_request(account)
                target = self.hooks.activation_target(account)
                if type(target) is not int or target <= 0:
                    raise TransactionError("invalid pending activation weight")
                self.l2_driver.begin('l2', self.l2_rpc, kind='activation', metadata={'account': account, 'target': target}, **request)
                return 'activation_submitted'
        return 'reconciled'

    def _resume_bridge(self):
        entry = self.journal['transactions']['l1']
        receipt = self.l1_driver.receipt('l1', self.l1_rpc)
        if receipt is None:
            self.l1_driver.rebroadcast_if_missing('l1', self.l1_rpc)
            return 'bridge_l1_pending'
        cfg = self.config
        digest = bridge_priority_hash(receipt, cfg['bridge'], cfg['bridge_event_topic'])
        event = priority_payload(receipt, cfg['diamond'], cfg['priority_event_topic'], digest, self.hooks.hash_bytes)
        if event['from'] != membership.address(cfg['l1_alias']) or event['to'] != membership.address(cfg['membership']):
            raise TransactionError("priority sender or target mismatch")
        if event['transactionType'] != 127:
            raise TransactionError("unexpected priority transaction type")
        updates, height, observed_at = membership.decode_updates(event['input'])
        if set(a for a, _, _ in updates) != set(entry['metadata']['accounts']):
            raise TransactionError("priority operation account set mismatch")
        def verify(receipt_l2, entry_l1):
            return self.hooks.verify_delivery(receipt_l2, entry_l1, updates, height, observed_at)
        applied = self.l1_driver.confirm_bridge_delivery('l1', self.l2_rpc, cfg['l2_chain_id'], digest, verify)
        if applied is None:
            return 'bridge_l2_pending'
        self.l1_driver.complete('l1')
        return 'bridge_finalized'

    def _resume_activation(self):
        if self.sync_only:
            raise TransactionError("sync-only cannot resume a journaled activation; it cannot cancel an already broadcast transaction")
        try:
            receipt = self.l2_driver.receipt('l2', self.l2_rpc)
        except FinalizedRevert:
            self.l2_driver.supersede_activation('l2', self.l2_rpc, self.hooks.verify_activation_superseded)
            return 'activation_superseded'
        if receipt is None:
            self.l2_driver.rebroadcast_if_missing('l2', self.l2_rpc)
            return 'activation_pending'
        entry = self.journal['transactions']['l2']
        if self.hooks.verify_activation(receipt, entry) is not True:
            raise TransactionError("activation state verification failed")
        self.l2_driver.complete('l2')
        return 'activation_finalized'
