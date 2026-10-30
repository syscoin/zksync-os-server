"""Prover-service transactions share durable byte/nonce checks without modifying the membership keeper."""
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from zksys_keeper_transactions import (
    TransactionDriver, TransactionError, _RAW, _address, _hash, _quantity, _same_transaction,
)


class ServiceTransactionDriver(TransactionDriver):
    """The membership outbox remains separate; retain the same nonce and byte checks."""

    def begin(self, name, rpc, chain_id, sender, to, data, value, gas, gas_price, *, kind, metadata=None):
        self._check()
        if kind not in ('enrollment', 'delivery', 'activation', 'checkpoint', 'roster', 'deployment'):
            raise TransactionError('invalid service transaction kind')
        if name in self.journal['transactions']:
            raise TransactionError('chain already has an unresolved transaction')
        if _quantity(rpc('eth_chainId', [])) != _quantity(chain_id):
            raise TransactionError('unexpected chain id')
        sender, to = _address(sender), _address(to)
        for existing in self.journal['transactions'].values():
            if existing['chainId'] == _quantity(chain_id) and existing['sender'] == sender:
                raise TransactionError('signer already has an unresolved transaction')
        if not isinstance(data, str) or not re.fullmatch(r'0x(?:[0-9a-fA-F]{2})*', data):
            raise TransactionError('invalid calldata')
        latest = _quantity(rpc('eth_getTransactionCount', [sender, 'latest']))
        pending = _quantity(rpc('eth_getTransactionCount', [sender, 'pending']))
        if latest != pending:
            raise TransactionError('signer has pending transactions')
        tx = {'from': sender, 'to': to, 'data': data.lower(), 'chainId': _quantity(chain_id),
              'nonce': latest, 'value': _quantity(value), 'gas': _quantity(gas),
              'gasPrice': _quantity(gas_price), 'type': 0}
        if not tx['gas'] or not tx['gasPrice']:
            raise TransactionError('gas and gas price must be positive')
        raw = self.sign(dict(tx))
        if not isinstance(raw, str) or not _RAW.fullmatch(raw):
            raise TransactionError('invalid signed transaction')
        decoded = self.decode(raw)
        if not _same_transaction(tx, decoded) or _quantity(decoded.get('type', 0)) != 0:
            raise TransactionError('signed transaction does not match request')
        digest = _hash(self.hash_raw(raw))
        self.journal['transactions'][name] = {'kind': kind, 'chainId': tx['chainId'],
            'sender': sender, 'nonce': latest, 'txHash': digest, 'raw': raw.lower(),
            'state': 'signed', 'metadata': metadata or {}}
        self._save()
        return self.broadcast(name, rpc)
