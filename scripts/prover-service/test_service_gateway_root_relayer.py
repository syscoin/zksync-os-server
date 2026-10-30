import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import service as s
import service_gateway_root_relayer as r


class Rpc:
    def call(self, method, params):
        if method == 'eth_chainId':
            return '0x2'
        raise AssertionError(method)


class Hooks:
    def __init__(self):
        self.l1 = Rpc()
        self.child = Rpc()
        self.ready = {}
        self.received_result = True
    def verify(self): pass
    def verify_journal(self, entry): pass
    def snapshot(self, batch, message_hash): return self.ready.get((batch, message_hash))
    def prepare(self, batch, message_hash, snapshot):
        return ({'chain_id': 1}, {'batch': batch, 'messageHash': message_hash, 'root': snapshot['root']})
    def priority_hash(self, receipt, entry): return '0x' + '44' * 32
    def received(self, *args): return self.received_result


class Driver:
    def __init__(self, journal): self.journal, self.result, self.rebroadcast = journal, None, 0
    def begin(self, name, rpc, kind, metadata, **request):
        self.journal['transactions'][name] = {'metadata': metadata, 'txHash': '0x' + 'aa' * 32}
    def receipt(self, name, rpc): return self.result
    def rebroadcast_if_missing(self, name, rpc): self.rebroadcast += 1


def state(tmp_path):
    path = tmp_path / 'requests.json'
    path.write_text(json.dumps({'version': 1, 'identity': '22' * 32, 'requests': []}))
    path.chmod(0o600)
    cfg = {'requests_file': str(path), 'requests_identity': '0x' + '22' * 32, 'zksys_chain_id': 2}
    journal = {'transactions': {}, 'completed': {}, 'history': [], 'nextRetryAt': 0}
    return path, cfg, journal, Driver(journal), Hooks()


def requests(path, items):
    path.write_text(json.dumps({'version': 1, 'identity': '22' * 32, 'requests': items}))


def test_only_requested_batches_and_bundled_hash(tmp_path):
    path, cfg, journal, driver, hooks = state(tmp_path)
    root, digest = '0x' + '33' * 32, '0x' + '55' * 32
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 1) == 'no_requested_roots'
    requests(path, [{'batch': 100, 'messageHash': digest}])
    hooks.ready[(100, digest)] = {'root': root, 'imported': root, 'authorized': False}
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 1) == 'submitted'
    assert journal['transactions']['root']['metadata']['messageHash'] == digest
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 2) == 'l1_pending'
    assert driver.rebroadcast == 1
    driver.result = {'blockHash': root}
    with mock.patch.object(r, 'finalized_receipt', return_value=None):
        assert r.step(cfg, journal, lambda _: None, driver, hooks, 3) == 'l2_pending'
    with mock.patch.object(r, 'finalized_receipt', return_value={'status': '0x1', 'blockHash': root}):
        assert r.step(cfg, journal, lambda _: None, driver, hooks, 4) == 'delivered'
    assert list(journal['completed'].values()) == [{'batch': 100, 'messageHash': digest}]
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 5) == 'no_requested_roots'


def test_l2_revert_retry_never_completes_unaccepted_hash(tmp_path):
    path, cfg, journal, driver, hooks = state(tmp_path)
    root, digest = '0x' + '33' * 32, '0x' + '55' * 32
    requests(path, [{'batch': 1, 'messageHash': digest}])
    hooks.ready[(1, digest)] = {'root': root, 'imported': root, 'authorized': False}
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 1) == 'submitted'
    driver.result = {'blockHash': root}
    with mock.patch.object(r, 'finalized_receipt', return_value={'status': '0x0', 'blockHash': root}):
        assert r.step(cfg, journal, lambda _: None, driver, hooks, 2) == 'l2_reverted_retry_scheduled'
    assert journal['completed'] == {}
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 3) == 'retry_wait'
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 63) == 'submitted'


def test_event_commitment_includes_message_hash():
    root, digest, msg = ('0x' + x * 64 for x in ('3', '4', '5'))
    sink, diamond, receiver = ('0x' + x * 40 for x in ('1', '2', '3'))
    cfg = {'gateway_chain_id': 42, 'root_sink': sink, 'zksys_diamond': diamond, 'receiver': receiver}
    hooks = r.Hooks(cfg, None, None, None)
    for message_hash in (s.ZERO, msg):
        meta = {'batch': 7, 'root': root, 'messageHash': message_hash, 'childData': '0x1234'}
        commitment = s.keccak(s.word(42) + s.word(7) + s.raw_hex(root, 32) + s.raw_hex(message_hash, 32))
        logs = [{'address': sink, 'topics': [r.ROOT_EVENT, '0x' + s.word(7).hex(), root, digest]},
                {'address': sink, 'topics': [r.RELAY_EVENT, commitment, digest]}]
        if message_hash != s.ZERO:
            logs.append({'address': sink, 'topics': [r.BUNDLE_EVENT, '0x' + s.word(7).hex(), message_hash, digest]})
        payload = {'transactionType': 127, 'from': r.alias(sink), 'to': receiver, 'input': '0x1234'}
        with mock.patch.object(r, 'priority_payload', return_value=payload):
            assert hooks.priority_hash({'logs': logs}, {'metadata': meta}) == digest
            logs[1]['topics'][1] = s.keccak(s.word(42) + s.word(7) + s.raw_hex(root, 32))
            try:
                hooks.priority_hash({'logs': logs}, {'metadata': meta})
                assert False, 'three-word commitment unexpectedly accepted'
            except s.Error:
                pass


def test_inbox_identity_and_unexecuted_batch(tmp_path):
    path, cfg, journal, driver, hooks = state(tmp_path)
    requests(path, [{'batch': 12, 'messageHash': s.ZERO}])
    assert r.step(cfg, journal, lambda _: None, driver, hooks, 1) == 'awaiting_gateway_execution'
    cfg['requests_identity'] = '0x' + '77' * 32
    try:
        r.step(cfg, journal, lambda _: None, driver, hooks, 1)
        assert False, 'foreign inbox unexpectedly accepted'
    except s.Error:
        pass

class RootPublisherTests(unittest.TestCase):
    def test_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            test_only_requested_batches_and_bundled_hash(Path(directory))
    def test_revert(self):
        with tempfile.TemporaryDirectory() as directory:
            test_l2_revert_retry_never_completes_unaccepted_hash(Path(directory))
    def test_commitment(self): test_event_commitment_includes_message_hash()
    def test_inbox(self):
        with tempfile.TemporaryDirectory() as directory:
            test_inbox_identity_and_unexecuted_batch(Path(directory))


if __name__ == '__main__': unittest.main()
