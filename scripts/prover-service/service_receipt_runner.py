"""Single-flight service-receipt delivery state machine.

The caller owns an exclusive, durable journal and supplies authenticated chain
observation, proof construction, simulation and final-state checks through hooks.
No network or signer is trusted merely because an item appears in the journal.
"""
import copy
import time
from zksys_keeper_transactions import FinalizedRevert, TransactionError, finalized_receipt


class BatchRejected(TransactionError):
    """A point-in-time simulation rejected the exact proposed batch."""


class ReceiptRunner:
    def __init__(self, journal, save, driver, l1_rpc, l2_rpc, hooks, *, clock=time.time):
        self.journal, self.save, self.driver = journal, save, driver
        self.l1_rpc, self.l2_rpc, self.hooks, self.clock = l1_rpc, l2_rpc, hooks, clock
        journal.setdefault('records', {})
        journal.setdefault('transactions', {})
        journal.setdefault('split', [])
        journal.setdefault('history', [])
        if not isinstance(journal['records'], dict) or not isinstance(journal['split'], list):
            raise TransactionError('invalid receipt journal')

    def discover(self):
        # The hook must reject gaps and return only authenticated, canonical source
        # events with their complete matching retained artifact. Persist discovery
        # and its source cursors in one write so a crash cannot skip an event.
        cursor, found = self.hooks.discover(self.journal.get('cursor'))
        if not isinstance(found, list):
            raise TransactionError('invalid discovery result')
        records = copy.deepcopy(self.journal['records'])
        for record in records.values():
            if record['status'] == 'pending' and record['report']['kind'] == 'unresolved':
                self.hooks.validate_report(record['report'])
                resolved = self.hooks.hydrate(record['report'])
                if resolved is not None:
                    self.hooks.validate_resolution(record['report'], resolved)
                    self.hooks.validate_report(resolved)
                    record['report'] = resolved
        for report in found:
            self.hooks.validate_report(report)
            key = report['id']
            if key in records:
                if records[key]['report'] != report:
                    raise TransactionError('source report identity changed')
            else:
                records[key] = {'report': report, 'status': 'pending', 'failures': 0, 'nextRetryAt': 0}
        self.journal['records'] = records
        self.journal['cursor'] = cursor
        self.save(self.journal)

    def run_once(self):
        if self.hooks.verify_deployment() is not True:
            raise TransactionError('deployment identity verification failed')
        entries = self.journal['transactions']
        if set(entries) - {'receipt'}:
            raise TransactionError('unexpected receipt transaction')
        if 'receipt' in entries:
            return self.resume()
        self.discover()
        chosen = self.select()
        if not chosen:
            if any(value['status'] == 'pending' and value['report']['kind'] == 'unresolved'
                   for value in self.journal['records'].values()):
                return 'awaiting_retained_artifacts'
            return 'idle'
        reports = [self.journal['records'][key]['report'] for key in chosen]
        # Hooks revalidate source inclusion and finalized destination state and
        # simulate the exact request. None means the source is not yet settled.
        try:
            prepared = self.hooks.prepare(reports)
        except BatchRejected:
            self.requeue(chosen, 'simulation_reverted')
            return 'simulation_reverted_requeued'
        if prepared is None:
            return 'awaiting_source_or_prerequisites'
        request, metadata = prepared
        if not isinstance(metadata, dict):
            raise TransactionError('invalid receipt metadata')
        metadata = {**metadata, 'report_ids': chosen}
        self.driver.begin('receipt', self.l1_rpc, kind='delivery', metadata=metadata, **request)
        return 'submitted'

    def select(self):
        records, now = self.journal['records'], int(self.clock())
        eligible = {key for key, value in records.items() if value['status'] == 'pending'
                    and value['nextRetryAt'] <= now}
        split_ids = set()
        for group in self.journal['split']:
            if not isinstance(group, list) or not group or len(group) != len(set(group)):
                raise TransactionError('invalid split batch')
            if any(key not in records or key in split_ids for key in group):
                raise TransactionError('unknown split report')
            split_ids.update(group)
            if any(records[key]['report']['kind'] == 'activation' for key in group) and any(
                value['status'] == 'pending' and value['report'].get('bootstrap') for value in records.values()
            ):
                continue
            if all(key in eligible for key in group):
                return group
        def position(key):
            report = records[key]['report']
            block = report.get('sourceBlockNumber', 0)
            block = int(block, 16) if isinstance(block, str) and block.startswith('0x') else int(block)
            index = report.get('sourceLogIndex', 0)
            index = int(index, 16) if isinstance(index, str) and index.startswith('0x') else int(index)
            return bool(report.get('fromRoot')), block, index, key
        accepts = sorted((key for key in eligible - split_ids if records[key]['report']['kind'] == 'accepted'), key=position)
        if accepts:
            return accepts
        # Do not activate while a previously discovered bootstrap report remains
        # unresolved, even if it is waiting for its next retry.
        if any(v['status'] == 'pending' and v['report'].get('bootstrap') for v in records.values()):
            return []
        activations = sorted((key for key in eligible - split_ids if records[key]['report']['kind'] == 'activation'), key=position)
        return activations[:1]

    def resume(self):
        entry = self.journal['transactions']['receipt']
        ids = entry.get('metadata', {}).get('report_ids')
        if not isinstance(ids, list) or not ids or len(ids) != len(set(ids)):
            raise TransactionError('invalid journaled receipt batch')
        records = self.journal['records']
        if any(key not in records or records[key]['status'] != 'pending' for key in ids):
            raise TransactionError('journaled receipt report changed')
        reports = [records[key]['report'] for key in ids]
        for report in reports:
            self.hooks.validate_report(report)
        self.hooks.verify_journal(entry, reports)
        try:
            receipt = self.driver.receipt('receipt', self.l1_rpc)
        except FinalizedRevert as error:
            self.fail(ids, 'l1_reverted', error.receipt)
            return 'l1_reverted_requeued'
        if receipt is None:
            self.driver.rebroadcast_if_missing('receipt', self.l1_rpc)
            return 'l1_pending'
        # The hook must verify both the sink's event and the canonical Bridgehub
        # priority-request tuple, including sender, destination and exact input.
        canonical_hash = self.hooks.priority_hash(receipt, entry, reports)
        old = entry.get('canonicalHash')
        if old is not None and old != canonical_hash:
            raise TransactionError('priority operation hash changed')
        if old is None:
            entry['canonicalHash'] = canonical_hash
            self.save(self.journal)
        if self.hooks.l2_chain_id(self.l2_rpc) is not True:
            raise TransactionError('destination chain identity mismatch')
        child = finalized_receipt(self.l2_rpc, canonical_hash)
        if child is None:
            return 'l2_pending'
        status = child.get('status')
        status = int(status, 16) if isinstance(status, str) and status.startswith('0x') else int(status)
        if status != 1:
            # A consumed reverted priority operation cannot be retried. A new
            # L1 transaction may be signed only after the exact revert finalizes.
            self.fail(ids, 'l2_reverted', child)
            return 'l2_reverted_requeued'
        if self.hooks.verify_delivery(child, reports) is not True:
            raise TransactionError('finalized destination state mismatch')
        for key in ids:
            records[key]['status'] = 'complete'
            records[key]['finalizedTransaction'] = canonical_hash
            records[key]['finalizedBlockHash'] = child['blockHash'].lower()
        self.journal['history'].append({'ids': ids, 'status': 'complete', 'l1Hash': entry['txHash'],
                                        'l2Hash': canonical_hash})
        # completion and outbox retirement are saved together; ServiceTransactionDriver
        # alone does not enforce L2 finality for kind=delivery.
        del self.journal['transactions']['receipt']
        self.remove_split(ids)
        self.save(self.journal)
        return 'delivered'

    def fail(self, ids, reason, receipt):
        entry = self.journal['transactions']['receipt']
        self.journal['history'].append({'ids': ids, 'status': reason, 'l1Hash': entry['txHash'],
                                        'l2Hash': entry.get('canonicalHash'),
                                        'receiptBlockHash': receipt['blockHash'].lower()})
        del self.journal['transactions']['receipt']
        self.requeue(ids, reason)

    def requeue(self, ids, reason):
        records, now = self.journal['records'], int(self.clock())
        self.remove_split(ids)
        if len(ids) > 1:
            midpoint = len(ids) // 2
            self.journal['split'] = [ids[:midpoint], ids[midpoint:]] + self.journal['split']
        else:
            self.journal['split'].append(ids)
            record = records[ids[0]]
            count = record['failures'] + 1
            record['failures'] = count
            record['lastFailure'] = reason
            record['nextRetryAt'] = now + min(600, 60 * (2 ** min(count - 1, 4)))
        self.save(self.journal)

    def remove_split(self, ids):
        self.journal['split'] = [group for group in self.journal['split'] if group != ids]
