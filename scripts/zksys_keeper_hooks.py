"""RPC hooks for the bounded zkSYS membership transaction runner.

Configuration must be independently attested. Proxy checks pin both runtime and
EIP-1967 implementation code. This module does not install or start a service.
"""
import zksys_membership_keeper as k
import re
from zksys_keeper_cast import hash_bytes
from zksys_keeper_transactions import TransactionError

SLOT = '0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc'


def validate_config(cfg):
    if type(cfg) is not dict:
        raise TransactionError('invalid config')
    cfg.setdefault('max_batch_size', 512)
    for field in ('l1_chain_id', 'l2_chain_id', 'max_batch_size', 'l1_gas_price', 'l1_gas_limit',
                  'priority_gas_limit', 'gas_per_pubdata', 'max_priority_fee', 'max_l1_transaction_cost',
                  'l2_gas_price', 'l2_gas_limit', 'max_l2_transaction_cost'):
        if type(cfg.get(field)) is not int or cfg[field] <= 0:
            raise TransactionError('invalid positive configuration field: ' + field)
    if cfg['max_batch_size'] > 512 or cfg['l1_chain_id'] == cfg['l2_chain_id']:
        raise TransactionError('invalid batch size or chain ids')
    for field in ('bridge', 'bridgehub', 'diamond', 'membership', 'weights', 'issuer', 'l1_alias',
                  'l1_signer', 'l2_signer', 'refund'):
        k.address(cfg.get(field))
    for field in ('l1_genesis', 'l2_genesis'):
        if not isinstance(cfg.get(field), str) or not re.fullmatch(r'0x[0-9a-fA-F]{64}', cfg[field]):
            raise TransactionError('invalid genesis hash')
    for field in ('l1_keystore', 'l2_keystore', 'l1_password_file', 'l2_password_file'):
        if not isinstance(cfg.get(field), str) or not cfg[field].startswith('/'):
            raise TransactionError('signer paths must be absolute')
    checks = cfg.get('code_checks')
    if not isinstance(checks, list):
        raise TransactionError('missing code checks')
    for check in checks:
        if type(check) is not dict or check.get('chain') not in ('l1', 'l2'):
            raise TransactionError('invalid code check')
        k.address(check.get('address'))
        for field in ('code_hash',):
            if not isinstance(check.get(field), str) or not re.fullmatch(r'0x[0-9a-fA-F]{64}', check[field]):
                raise TransactionError('invalid code hash')
        if check['chain'] == 'l2' or check['address'].lower() in (cfg['bridge'].lower(), cfg['bridgehub'].lower()):
            k.address(check.get('implementation'))
            if not isinstance(check.get('implementation_code_hash'), str) or not re.fullmatch(r'0x[0-9a-fA-F]{64}', check['implementation_code_hash']):
                raise TransactionError('invalid implementation hash')
    facets = cfg.get('diamond_facets')
    if not isinstance(facets, list) or not facets:
        raise TransactionError('missing diamond facet checks')
    selectors = set()
    for facet in facets:
        if type(facet) is not dict or not isinstance(facet.get('selector'), str) or not re.fullmatch(r'0x[0-9a-fA-F]{8}', facet['selector']):
            raise TransactionError('invalid diamond selector')
        selectors.add(facet['selector'].lower())
        k.address(facet.get('address'))
        if not isinstance(facet.get('code_hash'), str) or not re.fullmatch(r'0x[0-9a-fA-F]{64}', facet['code_hash']):
            raise TransactionError('invalid facet code hash')
    required = {'0x' + k.selector(sig).hex() for sig in (
        'facetAddress(bytes4)',
        'bridgehubRequestL2Transaction((address,address,uint256,uint256,bytes,uint256,uint256,bytes[],address))',
        'l2TransactionBaseCost(uint256,uint256,uint256)',
    )}
    if len(selectors) != len(facets) or not required.issubset(selectors):
        raise TransactionError('missing required diamond selectors')
    return cfg


def _address_word(raw):
    if len(raw) != 32 or raw[:12] != bytes(12):
        raise TransactionError('invalid address result')
    return k.address('0x' + raw[12:].hex())


def _call(rpc, target, signature, block_hash, arg=b''):
    return k.eth_call_word(rpc, target, signature, block_hash, arg)


class Hooks:
    def __init__(self, core, l1, l2, journal, config, save):
        self.core, self.l1, self.l2, self.journal, self.config = core, l1, l2, journal, config
        self.save = save
        self.last_snapshot = None

    hash_bytes = staticmethod(hash_bytes)

    def verify_deployment(self, cfg):
        if k.hexint(self.l1.call('eth_chainId', [])) != cfg['l1_chain_id'] or k.hexint(self.l2.call('eth_chainId', [])) != cfg['l2_chain_id']:
            raise TransactionError('chain identity mismatch')
        if k.block(self.l1, '0x0')['hash'] != cfg['l1_genesis'].lower() or k.block(self.l2, '0x0')['hash'] != cfg['l2_genesis'].lower():
            raise TransactionError('genesis identity mismatch')
        anchors = {'l1': k.block(self.l1, 'latest'), 'l2': k.block(self.l2, 'latest')}
        rpcs = {'l1': self.l1, 'l2': self.l2}
        required = {('l1', k.address(cfg[x])) for x in ('bridge', 'bridgehub', 'diamond')}
        required |= {('l2', k.address(cfg[x])) for x in ('membership', 'weights', 'issuer')}
        checks = cfg['code_checks']
        if not isinstance(checks, list) or {(c['chain'], k.address(c['address'])) for c in checks} != required:
            raise TransactionError('incomplete deployment identity set')
        for item in checks:
            rpc, anchor = rpcs[item['chain']], anchors[item['chain']]['hash']
            addr = k.address(item['address'])
            block = {'blockHash': anchor, 'requireCanonical': True}
            code = rpc.call('eth_getCode', [addr, block])
            if not isinstance(code, str) or code == '0x' or hash_bytes(bytes.fromhex(code[2:])).lower() != item['code_hash'].lower():
                raise TransactionError('runtime code mismatch')
            # The known zkSYS contracts are proxies; no implicit implementation
            # upgrades are accepted when the proxy runtime remains unchanged.
            if item['chain'] == 'l2' or addr in (k.address(cfg['bridge']), k.address(cfg['bridgehub'])):
                raw = rpc.call('eth_getStorageAt', [addr, SLOT, block])
                if not isinstance(raw, str) or len(raw) != 66:
                    raise TransactionError('invalid proxy implementation slot')
                impl = _address_word(bytes.fromhex(raw[2:]))
                if impl != k.address(item['implementation']):
                    raise TransactionError('proxy implementation mismatch')
                impl_code = rpc.call('eth_getCode', [impl, block])
                if not isinstance(impl_code, str) or impl_code == '0x' or hash_bytes(bytes.fromhex(impl_code[2:])).lower() != item['implementation_code_hash'].lower():
                    raise TransactionError('proxy implementation code mismatch')
        a, b = anchors['l1']['hash'], anchors['l2']['hash']
        for facet in cfg['diamond_facets']:
            selector = bytes.fromhex(facet['selector'][2:])
            addr = _address_word(_call(self.l1, cfg['diamond'], 'facetAddress(bytes4)', a, selector + bytes(28)))
            if addr != k.address(facet['address']):
                raise TransactionError('diamond facet mismatch')
            code = self.l1.call('eth_getCode', [addr, {'blockHash': a, 'requireCanonical': True}])
            if not isinstance(code, str) or code == '0x' or hash_bytes(bytes.fromhex(code[2:])).lower() != facet['code_hash'].lower():
                raise TransactionError('diamond facet code mismatch')
        if _address_word(_call(self.l1, cfg['bridge'], 'bridgehub()', a)) != k.address(cfg['bridgehub']):
            raise TransactionError('bridgehub mismatch')
        if _address_word(_call(self.l1, cfg['bridge'], 'l2Registry()', a)) != k.address(cfg['membership']):
            raise TransactionError('bridge target mismatch')
        if int.from_bytes(_call(self.l1, cfg['bridge'], 'zksysChainId()', a), 'big') != cfg['l2_chain_id']:
            raise TransactionError('bridge chain mismatch')
        if _address_word(_call(self.l2, cfg['membership'], 'l1RegistryBridge()', b)) != k.address(cfg['bridge']):
            raise TransactionError('registry bridge mismatch')
        if _address_word(_call(self.l2, cfg['membership'], 'sentryNodeReceiver()', b)) != k.address(cfg['weights']):
            raise TransactionError('membership receiver mismatch')
        if _address_word(_call(self.l2, cfg['weights'], 'weightReceiver()', b)) != k.address(cfg['issuer']):
            raise TransactionError('issuer receiver mismatch')
        if _address_word(_call(self.l1, cfg['bridgehub'], 'getZKChain(uint256)', a, k.word(cfg['l2_chain_id']))) != k.address(cfg['diamond']):
            raise TransactionError('diamond mismatch')
        alias = (int(k.address(cfg['bridge']), 16) + int('1111000000000000000000000000000000001111', 16)) % (1 << 160)
        if k.address(cfg['l1_alias']) != '0x' + format(alias, '040x'):
            raise TransactionError('bridge alias mismatch')
        return True

    def discover(self):
        cfg = self.config
        snap = k.snapshot(self.core, self.l1, self.l2, cfg['membership'])
        state = {'ever_seen': self.journal.get('ever_seen', [])}
        accounts = k.merge_discovery(state, snap['entries'], snap['l2_active'])
        self.journal['ever_seen'] = accounts
        self.save(self.journal)
        current = k.block(self.l1, 'latest')
        changes = k.reconcile(self.l1, self.l2, cfg['bridge'], cfg['membership'], accounts, current, snap['l2_block'])
        self.last_snapshot = snap
        return {'accounts': accounts, 'changes': changes}

    def bridge_request(self, batch):
        cfg = self.config
        gas_price, gas = cfg['l1_gas_price'], cfg['l1_gas_limit']
        args = b''.join(k.word(x) for x in (cfg['l2_chain_id'], gas_price, cfg['priority_gas_limit'], cfg['gas_per_pubdata']))
        latest = k.block(self.l1, 'latest')
        fee = int.from_bytes(_call(self.l1, cfg['bridgehub'], 'l2TransactionBaseCost(uint256,uint256,uint256,uint256)', latest['hash'], args), 'big')
        if fee <= 0 or fee > cfg['max_priority_fee'] or gas_price * gas + fee > cfg['max_l1_transaction_cost']:
            raise TransactionError('priority fee exceeds configured bound')
        return dict(chain_id=cfg['l1_chain_id'], sender=cfg['l1_signer'], to=cfg['bridge'],
                    data=k.encode_push(batch, cfg['priority_gas_limit'], cfg['gas_per_pubdata'], cfg['refund']),
                    value=fee, gas=gas, gas_price=gas_price)

    def activation_ready(self, account):
        cfg = self.config
        return k.activation_ready(self.l1, self.l2, cfg['bridge'], cfg['membership'], cfg['weights'], cfg['issuer'], account,
                                  k.block(self.l1, 'latest'), self.last_snapshot['l2_block'])

    def activation_request(self, account):
        cfg = self.config
        if cfg['l2_gas_price'] * cfg['l2_gas_limit'] > cfg['max_l2_transaction_cost']:
            raise TransactionError('activation fee exceeds configured bound')
        data = '0x' + (k.selector('activatePendingWeightFor(address)') + k.aword(account)).hex()
        return dict(chain_id=cfg['l2_chain_id'], sender=cfg['l2_signer'], to=cfg['weights'],
                    data=data, value=0, gas=cfg['l2_gas_limit'], gas_price=cfg['l2_gas_price'])

    def activation_target(self, account):
        data = k.selector('pendingWeightComponents(address)') + k.aword(account)
        result = self.l2.call('eth_call', [{'to': self.config['weights'], 'data': '0x' + data.hex()},
                                          {'blockHash': self.last_snapshot['l2_block']['hash'], 'requireCanonical': True}])
        if not isinstance(result, str) or len(result) != 258:
            raise TransactionError('invalid pending weight response')
        return int(result[130:194], 16)

    def verify_delivery(self, receipt, entry, updates, observed_height, observed_at):
        anchor = receipt['blockHash']
        for account, height, weight in updates:
            if k.member_at(self.l2, self.config['membership'], account, anchor) != (height, weight):
                return False
            data = k.selector('membershipObservation(address)') + k.aword(account)
            raw = self.l2.call('eth_call', [{'to': self.config['membership'], 'data': '0x' + data.hex()}, {'blockHash': anchor, 'requireCanonical': True}])
            if not isinstance(raw, str) or len(raw) != 130:
                return False
            value = bytes.fromhex(raw[2:])
            if (int.from_bytes(value[:32], 'big'), int.from_bytes(value[32:], 'big')) != (observed_height, observed_at):
                return False
        return True

    def verify_activation(self, receipt, entry):
        account = entry['metadata']['account']
        data = k.selector('weightComponents(address)') + k.aword(account)
        result = self.l2.call('eth_call', [{'to': self.config['weights'], 'data': '0x' + data.hex()}, {'blockHash': receipt['blockHash'], 'requireCanonical': True}])
        if not isinstance(result, str) or len(result) != 130:
            return False
        return int(result[66:], 16) == entry['metadata']['target']

    def verify_activation_superseded(self, receipt, entry):
        account = k.address(entry['metadata']['account'])
        target = entry['metadata']['target']
        if type(target) is not int or target <= 0:
            return False
        snapshot = k.block(self.l2, 'finalized')
        if snapshot['number'] < k.hexint(receipt['blockNumber']):
            return False
        anchor = snapshot['hash']
        height, member_weight = k.member_at(self.l2, self.config['membership'], account, anchor)
        if height == 0 or member_weight != target:
            return False
        block = {'blockHash': anchor, 'requireCanonical': True}
        result = self.l2.call('eth_call', [{'to': self.config['weights'], 'data': '0x' +
            (k.selector('weightComponents(address)') + k.aword(account)).hex()}, block])
        if not isinstance(result, str) or not re.fullmatch(r'0x[0-9a-fA-F]{128}', result):
            raise TransactionError('invalid weight response')
        if int(result[66:], 16) != target:
            return False
        pending = self.l2.call('eth_call', [{'to': self.config['weights'], 'data': '0x' +
            (k.selector('pendingWeightComponents(address)') + k.aword(account)).hex()}, block])
        if not isinstance(pending, str) or not re.fullmatch(r'0x[0-9a-fA-F]{256}', pending):
            raise TransactionError('invalid pending weight response')
        if int(pending[130:194], 16) != 0 or int(pending[194:258], 16) != 0:
            return False
        canonical = k.block(self.l2, hex(snapshot['number']))
        finalized = k.block(self.l2, 'finalized')
        if (canonical['hash'] != snapshot['hash'] or finalized['number'] < snapshot['number']
                or (finalized['number'] == snapshot['number'] and finalized['hash'] != snapshot['hash'])):
            raise TransactionError('finalized activation snapshot changed')
        return True
