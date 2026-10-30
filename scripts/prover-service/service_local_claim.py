#!/usr/bin/env python3
"""Claim authenticated service reports on zkSYS after their roots arrive.

Uses a separate zkSYS signer journal. Source RPCs and deployment identities are
pinned as for the root relay; root authorization is checked at finalized state.
"""
import argparse
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import time

import bootstrap_enrollment
import journal
import relay
import receipt_storage as storage
import service as s
import service_receipt_relay as receipts
import service_receipt_runner as machine
import workflow_io as io
from service_transaction_driver import ServiceTransactionDriver

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import FinalizedRevert, TransactionError, _same_transaction

PACKAGE = relay.abi_tuple(s.PACKAGE)
DUTY = relay.abi_tuple(s.DUTY + [('operatorSignature', 'bytes')])
PROOF = '(uint256,uint256,uint16,bytes32[])'
CHILD = '(' + PACKAGE + ',' + DUTY + '[],bool,' + PROOF + ')'
ROOT = '(' + PACKAGE + ',' + DUTY + '[],bool)'
MIXED = 'claimMixedBatch(' + CHILD + '[],' + ROOT + '[])'
ACTIVATION = 'claimActivation(bytes32,bytes32,' + PROOF + ')'
MAX_GAS = 1 << 24
METADATA = '0x010f0001' + '00' * 28


def claim_tuple(report, proof=None):
    accepted, duties = report['accepted'], report['duties']
    s.require(s.report_hash(duties) == accepted['reportHash'], 'claim_report_hash_mismatch')
    fields = [(s.encode_fields(s.PACKAGE, accepted), False),
              (receipts.dynamic_array([s.encode_duty(duty) for duty in duties]), True),
              (s.word(int(report['bootstrap'])), False)]
    if proof is not None:
        fields.append((receipts.inclusion_bytes(proof), True))
    return s.encode_dynamic_tuple(fields)


def calldata(reports, proofs):
    s.require(len(reports) == len(proofs) and reports, 'claim_count_mismatch')
    if reports[0]['kind'] == 'activation':
        s.require(len(reports) == 1 and not reports[0]['fromRoot'] and proofs[0] is not None,
                  'unsupported_claim_activation')
        report = reports[0]
        return receipts.call_data(ACTIVATION, [(s.raw_hex(report['vk_hash'], 32), False),
              (s.raw_hex(report['roster_root'], 32), False), (receipts.inclusion_bytes(proofs[0]), True)])
    child, root = [], []
    for report, proof in zip(reports, proofs):
        s.require(report['kind'] == 'accepted', 'mixed_claim_kinds')
        if report['fromRoot']:
            s.require(proof is None, 'unexpected_root_claim_proof')
            root.append(claim_tuple(report))
        else:
            s.require(proof is not None, 'missing_child_claim_proof')
            child.append(claim_tuple(report, proof))
    return receipts.call_data(MIXED, [(receipts.dynamic_array(child), True), (receipts.dynamic_array(root), True)])


class Hooks(receipts.Hooks):
    def __init__(self, cfg, l1, gateway, zksys, signer, gas_price, inbox=None, claim_path=None, claim_identity=None):
        super().__init__(cfg, l1, gateway, zksys, signer)
        self.gas_price = gas_price
        self.inbox = inbox
        self.claim_path, self.claim_identity = claim_path, claim_identity

    def collect_requests(self, records, requests, assigned, unrooted):
        batches = set()
        hashes = set()
        resolved = []
        for _, value in journal.status_items(records, ('pending',)):
            report = value['report']
            if report['fromRoot'] and report['kind'] in ('accepted', 'unresolved'):
                digest = s.keccak(receipts.report_message(report))
                if digest not in assigned:
                    hashes.add(digest)
        for key in list(unrooted):
            value = records[key]
            proof = value.get('rootProof') or self.proof(value['report'])
            if proof is not None:
                batch = s.uint(proof['blockOrBatchNumber'])
                s.require(batch > 0, 'invalid_root_request_batch')
                if str(batch) not in requests:
                    batches.add(batch)
                resolved.append((key, proof))
        for key, proof in resolved:
            records[key]['rootProof'] = proof
            del unrooted[key]
        unassigned = iter(sorted(hashes))
        for batch in sorted(batches):
            digest = next(unassigned, s.ZERO)
            requests[str(batch)] = {'batch': batch, 'messageHash': digest}
            if digest != s.ZERO:
                assigned[digest] = True
        return requests

    def publish_requests(self, requests):
        if self.inbox is not None:
            identity = hashlib.sha256(s.canonical(self.cfg)).hexdigest()
            storage.publish_descriptor(self.inbox, identity, self.claim_path, self.claim_identity)

    def already_claimed(self, report):
        block, anchor = self.anchor(self.zksys, self.cfg['zksys_chain_id'], self.cfg['zksys_genesis'])
        digest = s.raw_hex(s.keccak(receipts.report_message(report)), 32)
        mapping = 'relayedRootMessages(bytes32)' if report['fromRoot'] else 'relayedMessages(bytes32)'
        if not relay.call_word(self.zksys, self.cfg['receiver'], mapping, anchor, digest, 'bool'):
            return None
        if report['kind'] != 'activation':
            package = report['packageHash'] if report['kind'] == 'unresolved' else s.struct_hash(
                'AcceptedPackageV1', s.PACKAGE, report['accepted'])
            if not relay.call_word(self.zksys, self.cfg['registry'], 'acceptedPackages(bytes32)', anchor,
                                   s.raw_hex(package, 32), 'bool'):
                raise TransactionError('relayed claim missing registry state')
        return block['hash']

    def readiness(self, report):
        # Root activation remains on the existing root relay path.
        if report['kind'] == 'unresolved' or report['kind'] == 'activation' and report['fromRoot']:
            return None
        self.validate_report(report)
        _, anchor = self.anchor(self.zksys, self.cfg['zksys_chain_id'], self.cfg['zksys_genesis'])
        receiver = self.cfg['receiver']
        digest = s.raw_hex(s.keccak(receipts.report_message(report)), 32)
        if report['fromRoot']:
            if not relay.call_word(self.zksys, receiver, 'authorizedRootMessages(bytes32)', anchor, digest, 'bool'):
                return None
            return {'proof': None}
        proof = self.proof(report)
        if proof is None:
            return None
        s.require(proof['messageIndex'] < 16384 and len(proof['merkleProof']) == 16
                  and proof['merkleProof'][0].lower() == METADATA, 'invalid_v32_gateway_proof')
        for item in proof['merkleProof']:
            s.raw_hex(item, 32)
        root = relay.call_word(self.zksys, receiver, 'gatewayBatchRoots(uint256)', anchor,
                               s.word(proof['blockOrBatchNumber']), 'bytes32')
        if root == s.ZERO:
            return None
        return {'proof': proof}

    def prepare_claim(self, reports, proofs):
        data = calldata(reports, proofs)
        tx = {'from': self.cfg['signer'], 'to': self.cfg['receiver'], 'data': data, 'value': '0x0'}
        try:
            estimate = s.uint(self.zksys.call('eth_estimateGas', [tx]))
            if estimate == 0 or estimate > MAX_GAS:
                raise machine.BatchRejected('claim_exceeds_consensus_gas_limit')
            gas = min(MAX_GAS, (estimate * 12 + 9) // 10)
            result = self.zksys.call('eth_call', [{**tx, 'gas': hex(gas)}, 'latest'])
            s.require(result == '0x', 'unexpected_claim_simulation')
        except relay.RpcError:
            raise machine.BatchRejected('claim_simulation_reverted') from None
        request = dict(chain_id=self.cfg['zksys_chain_id'], sender=self.cfg['signer'], to=self.cfg['receiver'],
                       data=data, value=0, gas=gas, gas_price=self.gas_price)
        return request, {'request': request, 'proofs': proofs}

    def verify_claim_journal(self, entry, reports):
        meta = entry['metadata']
        request = meta['request']
        s.require(calldata(reports, meta['proofs']) == request['data'] and request['value'] == 0
                  and request['chain_id'] == self.cfg['zksys_chain_id'] and request['sender'] == self.cfg['signer']
                  and request['to'] == self.cfg['receiver'] and request['gas_price'] == self.gas_price
                  and 0 < request['gas'] <= MAX_GAS, 'claim_journal_payload_changed')
        expected = {'from': request['sender'], 'to': request['to'], 'chainId': request['chain_id'],
                    'nonce': entry['nonce'], 'gas': request['gas'], 'gasPrice': request['gas_price'],
                    'value': 0, 'data': request['data']}
        s.require(entry['kind'] == 'delivery' and self.signer.hash_raw(entry['raw']) == entry['txHash']
                  and _same_transaction(expected, self.signer.decode(entry['raw'])), 'claim_journal_transaction_changed')

    def finalized_claims(self, receipt, reports):
        anchor = {'blockHash': receipt['blockHash'], 'requireCanonical': True}
        for report in reports:
            digest = s.raw_hex(s.keccak(receipts.report_message(report)), 32)
            mapping = 'relayedRootMessages(bytes32)' if report['fromRoot'] else 'relayedMessages(bytes32)'
            if not relay.call_word(self.zksys, self.cfg['receiver'], mapping, anchor, digest, 'bool'):
                return False
            if report['kind'] == 'accepted':
                package = s.struct_hash('AcceptedPackageV1', s.PACKAGE, report['accepted'])
                if not relay.call_word(self.zksys, self.cfg['registry'], 'acceptedPackages(bytes32)', anchor,
                                       s.raw_hex(package, 32), 'bool'):
                    return False
        return True


class ClaimRunner(machine.ReceiptRunner):
    def __init__(self, journal, save, driver, zksys_rpc, hooks, *, clock=time.time):
        super().__init__(journal, save, driver, zksys_rpc, zksys_rpc, hooks, clock=clock)
        storage._normalize(journal, 'claim')

    def index_discovery(self, changes):
        for key, value in changes.items():
            report = value['report']
            if not report['fromRoot'] and value.get('rootProof') is None:
                self.journal['rootPending'][key] = True
            if report['kind'] == 'activation':
                self.journal['activations'][key] = report

    def remove_split(self, ids):
        selected = set(ids)
        self.journal['split'] = [remaining for group in self.journal['split']
                                 if (remaining := [key for key in group if key not in selected])]

    def run_once(self):
        if self.hooks.verify_deployment() is not True:
            raise TransactionError('claim deployment identity verification failed')
        if set(self.journal['transactions']) - {'claim'}:
            raise TransactionError('unexpected claim transaction')
        if 'claim' in self.journal['transactions']:
            return self.resume_claim()
        self.discover()
        for key, value in journal.status_items(self.journal['records'], ('pending',)):
            self.hooks.validate_report(value['report'])
            finalized = self.hooks.already_claimed(value['report'])
            if finalized is not None:
                self.journal['records'][key].update(status='complete', observedFinalizedBlockHash=finalized)
        # External or direct-path claims can complete a previously split item.
        for group in self.journal['split'][:]:
            for key in group:
                if self.journal['records'][key]['status'] == 'complete':
                    self.remove_split([key])
        requests = self.hooks.collect_requests(self.journal['records'], self.journal['rootRequests'],
                                               self.journal['rootRequestHashes'], self.journal['rootPending'])
        self.save(self.journal)
        self.hooks.publish_requests(requests)
        records, now = self.journal['records'], int(self.clock())
        ready = {}
        pending = list(journal.status_items(records, ('pending',)))
        for key, value in pending:
            if value['nextRetryAt'] <= now:
                candidate = self.hooks.readiness(value['report'])
                if candidate is not None:
                    ready[key] = candidate
        if not ready:
            return 'awaiting_roots_or_reports'
        pending_bootstrap = any(v['report'].get('bootstrap') for _, v in pending)
        split_members = {key for group in self.journal['split'] for key in group}
        activations = list(self.journal['activations'].values())
        cursor = self.journal['cursor']
        roots = {report['roster_root'] for report in activations if report['fromRoot']
                 and cursor['l1'] > s.uint(report['sourceBlockNumber'])}
        children = {report['roster_root'] for report in activations if not report['fromRoot']
                    and cursor['gateway'] > s.uint(report['sourceBlockNumber'])}
        # Each source gate rejects new bootstrap packages after its activation;
        # the scanned activation blocks are a stable boundary, unlike a moving head.
        activation_ready = not pending_bootstrap and bool(roots & children)
        chosen = [key for key in ready if activation_ready and records[key]['report']['kind'] == 'activation'
                  and not records[key]['report']['fromRoot']
                  and records[key]['report']['roster_root'] in roots][:1]
        if not chosen:
            for group in self.journal['split']:
                available = [key for key in group if key in ready]
                if available and all(records[k]['report']['kind'] == 'accepted' for k in available):
                    chosen = available
                    break
        if not chosen:
            chosen = [k for k in ready if k not in split_members and records[k]['report']['kind'] == 'accepted']
        if not chosen:
            return 'awaiting_bootstrap_or_retry'
        reports = [records[k]['report'] for k in chosen]
        proofs = [ready[k]['proof'] for k in chosen]
        try:
            request, metadata = self.hooks.prepare_claim(reports, proofs)
        except machine.BatchRejected:
            self.requeue(chosen, 'simulation_or_gas_limit')
            return 'claim_split_or_retried'
        metadata['report_ids'] = chosen
        self.driver.begin('claim', self.l2_rpc, kind='delivery', metadata=metadata, **request)
        return 'claim_submitted'

    def resume_claim(self):
        entry = self.journal['transactions']['claim']
        ids = entry.get('metadata', {}).get('report_ids')
        records = self.journal['records']
        if not isinstance(ids, list) or not ids or len(set(ids)) != len(ids) or any(
                key not in records or records[key]['status'] != 'pending' for key in ids):
            raise TransactionError('invalid journaled claim batch')
        reports = [records[key]['report'] for key in ids]
        for report in reports:
            self.hooks.validate_report(report)
        self.hooks.verify_claim_journal(entry, reports)
        try:
            receipt = self.driver.receipt('claim', self.l2_rpc)
        except FinalizedRevert as error:
            self.journal['history'].append({'ids': ids, 'status': 'claim_reverted', 'txHash': entry['txHash'],
                                            'blockHash': error.receipt['blockHash']})
            del self.journal['transactions']['claim']
            self.requeue(ids, 'claim_reverted')
            return 'claim_reverted_requeued'
        if receipt is None:
            self.driver.rebroadcast_if_missing('claim', self.l2_rpc)
            return 'claim_pending'
        if self.hooks.finalized_claims(receipt, reports) is not True:
            raise TransactionError('finalized claim state mismatch')
        for key in ids:
            records[key].update(status='complete', finalizedTransaction=entry['txHash'],
                                finalizedBlockHash=receipt['blockHash'].lower())
        self.journal['history'].append({'ids': ids, 'status': 'complete', 'txHash': entry['txHash']})
        del self.journal['transactions']['claim']
        self.remove_split(ids)
        self.save(self.journal)
        return 'claimed'


def run_loop(worker, poll_seconds, once, *, sleep=time.sleep, emit=print):
    failures = 0
    while True:
        try:
            status = worker.run_once()
            failures = 0
        except s.Error as error:
            if str(error) != 'rpc_transport_failure' or once:
                raise
            failures += 1
            emit(json.dumps({'status': 'rpc_transport_retry', 'failures': failures}))
            sleep(min(60, poll_seconds * 2 ** min(failures - 1, 6)))
            continue
        emit(json.dumps({'status': status}, sort_keys=True))
        if once:
            return
        sleep(poll_seconds)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--once', action='store_true')
    args = parser.parse_args()
    config = io.private_json(args.config)
    s.exact(config, ('receipt', 'zksys_gas_price'))
    cfg = receipts.configuration(config['receipt'])
    gas_price = s.uint(config['zksys_gas_price'])
    s.require(gas_price > 0, 'invalid_zksys_gas_price')
    if not args.execute:
        print(json.dumps({'status': 'configuration_validated', 'broadcast': False}))
        return
    directory = io.directory(args.state_dir)
    lock = os.open(directory / 'service-local-claim.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(lock)
        s.require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                  and not info.st_mode & 0o077, 'unsafe_claim_lock')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for name in ('keystore', 'password_file'):
            bootstrap_enrollment.private_file(cfg[name])
        signer = CastSigner(cfg['zksys_rpc_url'], cfg['keystore'], cfg['password_file'])
        s.require(signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
                  == cfg['signer'], 'claim_signer_changed')
        rpc = lambda key: relay.Rpc(cfg[key], timeout=cfg['rpc_timeout_seconds'])
        l1, gateway, zksys = rpc('l1_rpc_url'), rpc('gateway_rpc_url'), rpc('zksys_rpc_url')
        identity = hashlib.sha256(s.canonical(config)).hexdigest()
        path = directory / 'service-local-claim.json'
        if path.exists() or path.is_symlink():
            journal = storage.load(path, 'claim')
            s.require(journal.get('version') == 1 and journal.get('identity') == identity, 'claim_journal_identity_changed')
        else:
            journal = storage.journal.State({'version': 1, 'identity': identity, 'transactions': {}, 'records': {}, 'split': [], 'history': []})
            storage.save(path, journal, 'claim')
        save = lambda value: storage.save(path, value, 'claim')
        driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
        hooks = Hooks(cfg, l1, gateway, zksys, signer, gas_price, directory / 'service-root-requests.json', path, identity)
        worker = ClaimRunner(journal, save, driver, zksys.call, hooks)
        run_loop(worker, cfg['poll_seconds'], args.once, emit=lambda line: print(line, flush=True))
    finally:
        os.close(lock)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'service_claim_failed', file=sys.stderr)
        sys.exit(1)
