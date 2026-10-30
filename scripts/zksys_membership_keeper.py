#!/usr/bin/env python3
"""Membership keeper discovery and ABI primitives.

The caller must persist ever_seen before broadcasting and only activate after
confirmed delivery plus a fresh eligibility check. This module never signs or sends.
"""
import functools
import re
import subprocess

ZERO = '0x' + '00' * 20
HASH = re.compile(r'^[0-9a-fA-F]{64}$')
ADDR = re.compile(r'^0x[0-9a-fA-F]{40}$')


class KeeperError(Exception):
    pass


def require(condition, reason):
    if not condition:
        raise KeeperError(reason)


def address(value):
    require(isinstance(value, str) and ADDR.fullmatch(value) is not None, 'invalid_address')
    value = value.lower()
    require(value != ZERO, 'zero_address')
    return value


def number(value):
    require(type(value) is int and value >= 0, 'invalid_integer')
    return value


def word(value):
    value = number(value)
    require(value < 2**256, 'integer_overflow')
    return value.to_bytes(32, 'big')


def aword(value):
    return bytes(12) + bytes.fromhex(address(value)[2:])


@functools.lru_cache(maxsize=64)
def selector(signature):
    try:
        result = subprocess.run(['cast', 'keccak', signature], capture_output=True, check=True, timeout=10, text=True)
    except (OSError, subprocess.SubprocessError):
        raise KeeperError('cast_keccak_failed') from None
    digest = result.stdout.strip()
    require(re.fullmatch(r'0x[0-9a-fA-F]{64}', digest) is not None, 'invalid_keccak')
    return bytes.fromhex(digest[2:10])


def address_set(entries):
    require(type(entries) is list, 'invalid_core_list')
    addresses = set()
    identities = set()
    for entry in entries:
        require(type(entry) is dict and type(entry.get('state')) is dict, 'invalid_core_entry')
        identity = entry.get('proTxHash')
        require(isinstance(identity, str) and HASH.fullmatch(identity) is not None, 'invalid_protx_hash')
        identity = identity.lower()
        require(identity not in identities, 'duplicate_protx_hash')
        identities.add(identity)
        nevm = entry['state'].get('nevmAddress')
        require(isinstance(nevm, str), 'missing_nevm_address')
        if not nevm:
            continue
        nevm = address(nevm)
        require(nevm not in addresses, 'duplicate_nevm_address')
        addresses.add(nevm)
    return addresses


def merge_discovery(state, entries, l2_active):
    require(type(state) is dict and type(state.get('ever_seen')) is list, 'invalid_state')
    require(type(l2_active) is list, 'invalid_active_set')
    previous = [address(item) for item in state['ever_seen']]
    active = [address(item) for item in l2_active]
    require(len(previous) == len(set(previous)) and len(active) == len(set(active)), 'duplicate_address')
    union = set(previous) | address_set(entries) | set(active)
    state['ever_seen'] = sorted(union)
    return state['ever_seen'][:]


def encode_push(accounts, gas_limit, gas_per_pubdata, refund):
    require(type(accounts) is list and 0 < len(accounts) <= 512, 'invalid_batch_size')
    accounts = [address(a) for a in accounts]
    require(len(set(accounts)) == len(accounts), 'duplicate_address')
    require(number(gas_limit) > 0 and number(gas_per_pubdata) > 0, 'invalid_gas_limit')
    payload = selector('pushSentryNodeUpdates(address[],uint256,uint256,address)')
    payload += word(128) + word(gas_limit) + word(gas_per_pubdata) + aword(refund)
    payload += word(len(accounts)) + b''.join(aword(a) for a in accounts)
    return '0x' + payload.hex()


def decode_updates(data):
    require(isinstance(data, str) and re.fullmatch(r'0x(?:[0-9a-fA-F]{2})+', data), 'invalid_calldata')
    raw = bytes.fromhex(data[2:])
    require(len(raw) >= 4 + 128 and raw[:4] == selector('applyL1SentryNodeUpdates((address,uint32,uint128)[],uint64,uint64)'), 'invalid_update_selector')
    body = raw[4:]
    at = lambda offset: int.from_bytes(body[offset:offset + 32], 'big')
    require(at(0) == 96, 'invalid_update_offset')
    height, timestamp, count = at(32), at(64), at(96)
    require(height > 0 and height < 2**64 and timestamp > 0 and timestamp < 2**64, 'invalid_observation')
    require(0 < count <= 512 and len(body) == 128 + count * 96, 'invalid_update_length')
    updates = []
    seen = set()
    for i in range(count):
        offset = 128 + i * 96
        addr = body[offset:offset + 32]
        require(addr[:12] == bytes(12), 'invalid_update_address')
        account = address('0x' + addr[12:].hex())
        collateral, weight = at(offset + 32), at(offset + 64)
        require(account not in seen and collateral < 2**32 and weight < 2**128 and collateral <= height
                and ((collateral == 0) == (weight == 0)), 'invalid_update')
        seen.add(account)
        updates.append((account, collateral, weight))
    return updates, height, timestamp


def mature_pending(pending, current_period):
    require(isinstance(pending, (list, tuple)) and len(pending) == 4, 'invalid_pending_weight')
    values = [number(v) for v in pending]
    return values[2] > 0 and number(current_period) >= values[3]


def hexint(value):
    require(isinstance(value, str) and re.fullmatch(r'0x[0-9a-fA-F]+', value), 'invalid_rpc_quantity')
    return int(value, 16)


def block(rpc, tag):
    result = rpc.call('eth_getBlockByNumber', [tag, False])
    require(type(result) is dict and isinstance(result.get('hash'), str)
            and re.fullmatch(r'0x[0-9a-fA-F]{64}', result['hash']) is not None, 'missing_block')
    height = hexint(result.get('number'))
    return {'number': height, 'hash': result['hash'].lower()}


def eth_call_word(rpc, target, signature, block_hash, argument=b''):
    target = address(target)
    result = rpc.call('eth_call', [{'to': target, 'data': '0x' + (selector(signature) + argument).hex()},
                                  {'blockHash': block_hash, 'requireCanonical': True}])
    require(isinstance(result, str) and re.fullmatch(r'0x[0-9a-fA-F]{64}', result), 'invalid_call_result')
    return bytes.fromhex(result[2:])


def member_at(l2, membership, account, block_hash):
    data = selector('member(address)') + aword(account)
    result = l2.call('eth_call', [{'to': address(membership), 'data': '0x' + data.hex()},
                                  {'blockHash': block_hash, 'requireCanonical': True}])
    require(isinstance(result, str) and re.fullmatch(r'0x[0-9a-fA-F]{128}', result), 'invalid_member_result')
    raw = bytes.fromhex(result[2:])
    height, weight = int.from_bytes(raw[:32], 'big'), int.from_bytes(raw[32:], 'big')
    require(height < 2**32 and weight < 2**128 and ((height == 0) == (weight == 0)), 'invalid_member')
    return height, weight


def reconcile(l1, l2, bridge, membership, accounts, l1_block, l2_block):
    """Compare current NEVM lookup hints against a pinned L2 membership snapshot.

    The NEVM address precompile reads current auxiliary state, even at an older
    eth_call block; callers must not treat this as a historical L1 snapshot.
    """
    require(type(accounts) is list, 'invalid_accounts')
    normalized = [address(a) for a in accounts]
    require(len(normalized) == len(set(normalized)), 'duplicate_address')
    removals, other = [], []
    for account in normalized:
        arg = aword(account)
        height = int.from_bytes(eth_call_word(l1, bridge, 'nevmCollateralHeight(address)', l1_block['hash'], arg), 'big')
        weight = int.from_bytes(eth_call_word(l1, bridge, 'nevmSentryNodeWeight(address)', l1_block['hash'], arg), 'big')
        require(height < 2**32 and weight < 2**128 and ((height == 0) == (weight == 0)), 'invalid_nevm_fact')
        previous = member_at(l2, membership, account, l2_block['hash'])
        if previous == (height, weight):
            continue
        item = {'account': account, 'l1': (height, weight), 'l2': previous}
        (removals if weight < previous[1] else other).append(item)
    require(block(l1, hex(l1_block['number']))['hash'] == l1_block['hash'], 'l1_anchor_changed')
    require(block(l2, hex(l2_block['number']))['hash'] == l2_block['hash'], 'l2_anchor_changed')
    return removals + other


def activation_ready(l1, l2, bridge, membership, weights, issuer, account, l1_block, l2_block):
    """Check a pending Sentry increase against current membership hints.

    This does not protect against a membership change after the check; the
    contracts currently expose no atomic L1 eligibility check on activation.
    """
    account = address(account)
    arg = aword(account)
    source_height = int.from_bytes(eth_call_word(l1, bridge, 'nevmCollateralHeight(address)', l1_block['hash'], arg), 'big')
    source_weight = int.from_bytes(eth_call_word(l1, bridge, 'nevmSentryNodeWeight(address)', l1_block['hash'], arg), 'big')
    member = member_at(l2, membership, account, l2_block['hash'])
    if source_height == 0 or source_weight == 0 or member != (source_height, source_weight):
        return False
    calldata = selector('pendingWeightComponents(address)') + arg
    result = l2.call('eth_call', [{'to': address(weights), 'data': '0x' + calldata.hex()},
                                 {'blockHash': l2_block['hash'], 'requireCanonical': True}])
    require(isinstance(result, str) and re.fullmatch(r'0x[0-9a-fA-F]{256}', result), 'invalid_pending_result')
    raw = bytes.fromhex(result[2:])
    pending = tuple(int.from_bytes(raw[i:i + 32], 'big') for i in range(0, 128, 32))
    period = int.from_bytes(eth_call_word(l2, issuer, 'currentPeriod()', l2_block['hash']), 'big')
    require(block(l1, hex(l1_block['number']))['hash'] == l1_block['hash'], 'l1_anchor_changed')
    require(block(l2, hex(l2_block['number']))['hash'] == l2_block['hash'], 'l2_anchor_changed')
    return mature_pending(pending, period) and pending[2] == member[1]


def active_set(l2, membership_address, pinned_block, limit=100000):
    data = eth_call_word(l2, membership_address, 'activeSentryNodeCount()', pinned_block['hash'])
    count = int.from_bytes(data, 'big')
    require(count <= limit, 'active_set_limit')
    result = []
    for i in range(count):
        data = eth_call_word(l2, membership_address, 'activeSentryNodeAt(uint256)', pinned_block['hash'], word(i))
        require(data[:12] == bytes(12), 'invalid_active_address')
        result.append(address('0x' + data[12:].hex()))
    require(len(result) == len(set(result)), 'duplicate_active_address')
    return result


def snapshot(core, l1, l2, membership_address):
    """Read a canonical Core list paired to the finalized NEVM prefix and one L2 block.

    0x61 excludes its own height; read the previous paired block at the finalized
    execution block. The precompile returns serialized little-endian Syscoin
    uint256 bytes, while Core's RPC uses reverse-order display hex.
    """
    l1_head = block(l1, 'finalized')
    require(l1_head['number'] > 0, 'finalized_head_too_early')
    paired = l1_head['number'] - 1
    result = l1.call('eth_call', [{'to': '0x' + '00' * 19 + '61', 'data': '0x' + paired.to_bytes(8, 'big').hex()},
                                  {'blockHash': l1_head['hash'], 'requireCanonical': True}])
    require(isinstance(result, str) and re.fullmatch(r'0x[0-9a-fA-F]{64}', result), 'invalid_pair_hash')
    core_hash = bytes.fromhex(result[2:])[::-1].hex()
    require(core_hash != '00' * 32, 'zero_pair_hash')
    header = core.call('getblockheader', [core_hash])
    require(type(header) is dict and header.get('hash', '').lower() == core_hash, 'pair_header_mismatch')
    core_height = number(header.get('height'))
    require(core_height > 0 and core.call('getblockhash', [core_height]).lower() == core_hash, 'core_pair_not_canonical')
    entries = core.call('protx_list', ['registered', True, core_height])
    address_set(entries)
    require(core.call('getblockhash', [core_height]).lower() == core_hash, 'core_pair_changed')
    l2_head = block(l2, 'finalized')
    active = active_set(l2, membership_address, l2_head)
    require(block(l1, hex(l1_head['number']))['hash'] == l1_head['hash'], 'l1_anchor_changed')
    require(block(l2, hex(l2_head['number']))['hash'] == l2_head['hash'], 'l2_anchor_changed')
    return {'core_height': core_height, 'core_hash': core_hash, 'l1_block': l1_head,
            'l2_block': l2_head, 'entries': entries, 'l2_active': active}
