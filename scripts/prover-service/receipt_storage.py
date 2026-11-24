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


def new_claim_state(identity):
    return journal.State({'version': 1, 'identity': identity, 'transactions': {}, 'split': [], 'history': [],
                          **{field: {} for field in CLAIM_MAPS}})


def fields(kind):
    return {'claim': CLAIM_MAPS, 'native': NATIVE_MAPS, 'root': ROOT_MAPS}[kind]


def require_maps(state, map_fields):
    s.require(all(isinstance(state.get(field), Mapping) for field in map_fields), 'invalid_receipt_journal_maps')


def load(path, kind):
    state = journal.load(path, map_fields=fields(kind), list_fields=HISTORY)
    require_maps(state, fields(kind))
    return state


def save(path, state, kind):
    journal.save(path, state, map_fields=fields(kind), list_fields=HISTORY)


def publish_descriptor(path, identity, claim_path, claim_identity):
    relay.atomic_json(path, {'version': 2, 'identity': identity, 'claimJournal': str(claim_path),
                             'claimIdentity': claim_identity})


def read_descriptor(path, identity):
    inbox = io.private_json(path)
    s.exact(inbox, ('version', 'identity', 'claimJournal', 'claimIdentity'))
    s.require(inbox['version'] == 2 and inbox['identity'] == identity and type(inbox['claimJournal']) is str
              and Path(inbox['claimJournal']).is_absolute() and type(inbox['claimIdentity']) is str,
              'invalid_root_requests')
    return inbox


@contextmanager
def request_snapshot(path, identity):
    inbox = read_descriptor(path, identity)
    with journal.snapshot(inbox['claimJournal'], map_fields=CLAIM_MAPS, list_fields=HISTORY) as state:
        s.require(state.get('version') == 1 and state.get('identity') == inbox['claimIdentity'],
                  'claim_journal_identity_changed')
        require_maps(state, CLAIM_MAPS)
        yield state['rootRequests'].values(), inbox
