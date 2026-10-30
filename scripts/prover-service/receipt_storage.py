"""Indexed receipt-worker journals and the shared request descriptor."""

from collections.abc import Mapping
from contextlib import contextmanager
from pathlib import Path

import journal
import relay
import service as s
import workflow_io as io

CLAIM_MAPS = ('records', 'rootRequests', 'rootRequestHashes', 'rootPending', 'activations')
NATIVE_MAPS = ('records',)
ROOT_MAPS = ('completed',)
HISTORY = ('history',)


def request_key(item):
    s.exact(item, ('batch', 'messageHash'))
    s.require(type(item['batch']) is int and item['batch'] > 0, 'invalid_root_request_batch')
    s.raw_hex(item['messageHash'], 32)
    return str(item['batch'])


def completed_key(item):
    return request_key(item) + ':' + item['messageHash']


def _normalize(state, kind):
    state.setdefault('history', [])
    if kind in ('claim', 'native'):
        state.setdefault('records', {})
    if kind == 'claim':
        requests = state.setdefault('rootRequests', {})
        if isinstance(requests, list):
            converted = {}
            for item in requests:
                key = request_key(item)
                s.require(key not in converted or converted[key] == item, 'conflicting_root_requests')
                converted[key] = item
            state['rootRequests'] = requests = converted
        s.require(isinstance(requests, Mapping), 'invalid_root_requests')
        if state.get('receiptIndexVersion') != 1:
            state['rootRequestHashes'] = {item['messageHash']: True for item in requests.values()
                                          if item['messageHash'] != s.ZERO}
            pending, activations = {}, {}
            for key, value in state['records'].items():
                report = value['report']
                if not report['fromRoot']:
                    proof = value.get('rootProof')
                    if proof is None or str(s.uint(proof['blockOrBatchNumber'])) not in requests:
                        pending[key] = True
                if report['kind'] == 'activation':
                    activations[key] = report
            state['rootPending'], state['activations'] = pending, activations
            state['receiptIndexVersion'] = 1
    if kind == 'root':
        completed = state.setdefault('completed', {})
        if isinstance(completed, list):
            state['completed'] = {completed_key(item): item for item in completed}
    return state


def fields(kind):
    return {'claim': CLAIM_MAPS, 'native': NATIVE_MAPS, 'root': ROOT_MAPS}[kind]


def load(path, kind):
    return _normalize(journal.load(path, map_fields=fields(kind), list_fields=HISTORY), kind)


def save(path, state, kind):
    _normalize(state, kind)
    journal.save(path, state, map_fields=fields(kind), list_fields=HISTORY)


def publish_descriptor(path, identity, claim_path, claim_identity):
    relay.atomic_json(path, {'version': 2, 'identity': identity, 'claimJournal': str(claim_path),
                             'claimIdentity': claim_identity})


@contextmanager
def request_snapshot(path, identity):
    inbox = io.private_json(path)
    s.require(type(inbox) is dict and inbox.get('identity') == identity, 'invalid_root_requests')
    if inbox.get('version') == 1:
        s.exact(inbox, ('version', 'identity', 'requests'))
        s.require(type(inbox['requests']) is list, 'invalid_root_requests')
        yield inbox['requests'], None
    else:
        s.exact(inbox, ('version', 'identity', 'claimJournal', 'claimIdentity'))
        s.require(inbox['version'] == 2 and type(inbox['claimJournal']) is str
                  and Path(inbox['claimJournal']).is_absolute() and type(inbox['claimIdentity']) is str,
                  'invalid_root_requests')
        with journal.snapshot(inbox['claimJournal'], map_fields=CLAIM_MAPS, list_fields=HISTORY) as state:
            s.require(state.get('version') == 1 and state.get('identity') == inbox['claimIdentity'],
                      'claim_journal_identity_changed')
            _normalize(state, 'claim')
            yield state['rootRequests'].values(), inbox
