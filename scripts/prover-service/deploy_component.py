#!/usr/bin/env python3
"""Deploy one cross-chain priority or service-message prerequisite after native deployment."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

import deploy_gate as common
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionError, _same_transaction
from service_transaction_driver import ServiceTransactionDriver
import relay
import service
import zksys_membership_keeper as membership

ZERO = common.ZERO
CREATE2 = common.CREATE2


def require(condition, message):
    common.require(condition, message)


def component_config(raw):
    base = {'kind', 'chain_id', 'genesis_hash', 'sender', 'factory_salt', 'create2_code_hash',
            'gas_limit', 'gas_price', 'keystore', 'password_file', 'policy_hash'}
    shapes = {
        'root_priority_source': {'mailbox', 'mailbox_code_hash', 'bridgehub', 'bridgehub_code_hash',
                                 'child_chain_id', 'gateway_receiver', 'gateway_chain_id', 'inclusion_delay'},
        'gateway_checkpoint_receiver': {'root_source', 'child_chain_id'},
        'root_message_sink': {'publisher', 'gateway_gate', 'bridgehub', 'bridgehub_code_hash', 'gateway_chain_id',
                              'gateway_chain_address', 'gateway_chain_code_hash', 'registry_chain_id',
                              'registry_receiver'},
        'roster_receiver': {'child_chain_id', 'child_registry', 'message_sender', 'mailbox',
                            'mailbox_code_hash', 'mailbox_chain_id', 'start_time', 'period_seconds',
                            'first_service_period'},
        'root_draw_receiver': {'root_source'},
        'root_draw': {'mailbox', 'mailbox_code_hash', 'bridgehub', 'bridgehub_code_hash',
                      'gateway_coordinator', 'gateway_receiver', 'gateway_chain_id', 'future_blocks',
                      'confirmations'},
        'native_root_draw': {'coordinator', 'future_blocks', 'confirmations'},
        'coordinator': {'gate', 'gate_code_hash', 'root_source', 'root_source_code_hash',
                        'roster_source', 'roster_source_code_hash', 'sequencer', 'child_chain_id',
                        'child_chain_address', 'child_chain_code_hash', 'production_vk_hash',
                        'initial_parent', 'first_batch', 'first_service_period', 'turn_seconds',
                        'native_root_draw'},
    }
    require(type(raw) is dict and raw.get('kind') in shapes, 'invalid component kind')
    require(set(raw) == base | shapes[raw['kind']], 'invalid component configuration fields')
    value = dict(raw)
    addresses = ('sender', 'mailbox', 'bridgehub', 'gateway_receiver', 'root_source', 'publisher', 'gateway_gate',
                 'gateway_chain_address', 'registry_receiver', 'child_registry', 'message_sender',
                 'gateway_coordinator', 'coordinator', 'gate', 'roster_source', 'sequencer',
                 'child_chain_address')
    hashes = ('genesis_hash', 'factory_salt', 'create2_code_hash', 'policy_hash', 'mailbox_code_hash',
              'bridgehub_code_hash', 'gateway_chain_code_hash', 'gate_code_hash', 'root_source_code_hash',
              'roster_source_code_hash', 'child_chain_code_hash', 'production_vk_hash', 'initial_parent')
    for key in addresses:
        if key not in value:
            continue
        require(type(value[key]) is str and common.ADDRESS.fullmatch(value[key]), 'invalid ' + key)
        value[key] = value[key].lower()
        if key != 'gateway_receiver':
            require(value[key] != ZERO, 'zero ' + key)
    for key in hashes:
        if key not in value:
            continue
        require(type(value[key]) is str and common.HASH.fullmatch(value[key]), 'invalid ' + key)
        value[key] = value[key].lower()
        require(int(value[key], 16) != 0, 'zero ' + key)
    for key in ('chain_id', 'child_chain_id', 'registry_chain_id', 'inclusion_delay', 'gas_limit', 'gas_price',
                'mailbox_chain_id', 'period_seconds', 'future_blocks', 'confirmations', 'first_batch',
                'turn_seconds'):
        if key in value:
            require(type(value[key]) is int and value[key] > 0, 'invalid ' + key)
    if 'gateway_chain_id' in value:
        require(type(value['gateway_chain_id']) is int and value['gateway_chain_id'] >= 0,
                'invalid gateway_chain_id')
    if value['kind'] == 'root_priority_source':
        require((value['gateway_receiver'] == ZERO) == (value['gateway_chain_id'] == 0), 'receiver and Gateway id mismatch')
        require(value['child_chain_id'] != value['chain_id'], 'invalid child chain')
        require(value['inclusion_delay'] < 2**64, 'invalid inclusion delay')
        if value['gateway_chain_id']:
            require(len({value['gateway_chain_id'], value['chain_id'], value['child_chain_id']}) == 3,
                    'invalid Gateway chain')
    elif value['kind'] == 'gateway_checkpoint_receiver':
        require(value['child_chain_id'] != value['chain_id'], 'invalid child chain')
    elif value['kind'] == 'root_message_sink':
        require(value['gateway_chain_id'] > 0 and len({value['chain_id'], value['gateway_chain_id'],
                                                       value['registry_chain_id']}) == 3, 'invalid lane chain ids')
    elif value['kind'] == 'roster_receiver':
        require(value['child_chain_id'] != value['chain_id'] and value['mailbox_chain_id'] != value['chain_id'],
                'invalid roster lane chain ids')
        require(type(value['start_time']) is int and value['start_time'] > 0, 'invalid start_time')
    elif value['kind'] == 'root_draw':
        require(value['gateway_chain_id'] > 0 and value['gateway_chain_id'] != value['chain_id']
                and value['gateway_receiver'] != ZERO,
                'invalid draw lane chain ids')
    elif value['kind'] == 'coordinator':
        require(value['child_chain_id'] != value['chain_id'] and type(value['native_root_draw']) is bool,
                'invalid coordinator lane')
    if 'first_service_period' in value:
        require(type(value['first_service_period']) is int and 0 <= value['first_service_period'] < 2**64,
                'invalid first service period')
    for key in ('first_batch', 'turn_seconds', 'future_blocks', 'confirmations'):
        if key in value:
            require(value[key] < 2**64, 'invalid ' + key)
    if 'confirmations' in value:
        require(value['confirmations'] <= 255, 'draw confirmations exceed blockhash window')
    for key in ('keystore', 'password_file'):
        require(type(value[key]) is str and Path(value[key]).is_absolute(), 'invalid ' + key)
    return value


def component_bytecode(path, name):
    result = common.command('forge', ['inspect', '--root', 'contracts', '--remappings', 'contracts/=./',
                                       'src/zksys/' + path + '.sol:' + name, 'bytecode'])
    require(common.HEX.fullmatch(result) and len(result) > 2, 'invalid component compiler bytecode')
    return result.lower()


def build(cfg):
    factory_init = common.bytecode('ZkSysServiceComponentDeploymentFactoryV1') + common.encode(
        'constructor(address)', cfg['sender'])
    factory = common.cast('create2', '--deployer', CREATE2, '--salt', cfg['factory_salt'],
                          '--init-code', factory_init).lower()
    require(common.ADDRESS.fullmatch(factory), 'invalid factory address')
    component = common.cast('compute-address', factory, '--nonce', '1').split()[-1].lower()
    require(common.ADDRESS.fullmatch(component), 'invalid predicted component address')
    kind = cfg['kind']
    if kind == 'root_priority_source':
        init = component_bytecode('ZkSysPriorityGuardV1', 'ZkSysRootPrioritySourceV1') + common.encode(
            'constructor(address,address,address,uint256,bytes32,uint64)', cfg['mailbox'], cfg['bridgehub'],
            cfg['gateway_receiver'], cfg['gateway_chain_id'], cfg['policy_hash'], cfg['inclusion_delay'])
    elif kind == 'gateway_checkpoint_receiver':
        init = component_bytecode('ZkSysPriorityGuardV1', 'ZkSysPriorityCheckpointReceiverV1') + common.encode(
            'constructor(address,uint256,bytes32)', cfg['root_source'], cfg['child_chain_id'], cfg['policy_hash'])
    elif kind == 'root_message_sink':
        args = '(' + ','.join(str(cfg[key]) for key in ('publisher', 'gateway_gate', 'bridgehub', 'gateway_chain_id',
                    'gateway_chain_address', 'registry_chain_id', 'registry_receiver', 'policy_hash')) + ')'
        init = component_bytecode('ZkSysRootServiceV1', 'ZkSysRootServiceMessageSinkV1') + common.encode(
            'constructor((address,address,address,uint256,address,uint256,address,bytes32))', args)
    elif kind == 'roster_receiver':
        args = '(' + ','.join(str(cfg[key]) for key in ('child_chain_id', 'child_registry', 'message_sender',
                    'mailbox', 'policy_hash', 'start_time', 'period_seconds', 'first_service_period')) + ')'
        init = component_bytecode('ZkSysServiceMessageReceiversV1', 'ZkSysQualifiedRosterReceiverV1') + common.encode(
            'constructor((uint256,address,address,address,bytes32,uint256,uint256,uint64))', args)
    elif kind == 'root_draw_receiver':
        init = component_bytecode('ZkSysRootDrawV1', 'ZkSysRootDrawReceiverV1') + common.encode(
            'constructor(address)', cfg['root_source'])
    elif kind == 'root_draw':
        init = component_bytecode('ZkSysRootDrawV1', 'ZkSysRootDrawV1') + common.encode(
            'constructor(address,address,address,address,uint256,uint64,uint64)', cfg['mailbox'], cfg['bridgehub'],
            cfg['gateway_coordinator'], cfg['gateway_receiver'], cfg['gateway_chain_id'], cfg['future_blocks'],
            cfg['confirmations'])
    elif kind == 'native_root_draw':
        init = component_bytecode('ZkSysRootServiceV1', 'ZkSysNativeRootDrawV1') + common.encode(
            'constructor(address,uint64,uint64)', cfg['coordinator'], cfg['future_blocks'], cfg['confirmations'])
    else:
        args = '(' + ','.join(str(cfg[key]) for key in ('gate', 'root_source', 'roster_source', 'sequencer',
                    'child_chain_id', 'child_chain_address', 'policy_hash', 'production_vk_hash', 'initial_parent',
                    'first_batch', 'first_service_period', 'turn_seconds')) + ',' + str(cfg['native_root_draw']).lower() + ')'
        init = component_bytecode('ZkSysWrapperCoordinatorV1', 'ZkSysWrapperCoordinatorV1') + common.encode(
            'constructor((address,address,address,address,uint256,address,bytes32,bytes32,bytes32,uint64,uint64,uint64,bool))', args)
    return {'factory': factory, 'component': component, 'factory_init': factory_init,
            'component_init': init, 'commitment': common.digest(init)}


def verify_inputs(rpc, cfg):
    require(membership.hexint(rpc.call('eth_chainId', [])) == cfg['chain_id'], 'chain id mismatch')
    genesis = rpc.call('eth_getBlockByNumber', ['0x0', False])
    require(type(genesis) is dict and genesis.get('hash', '').lower() == cfg['genesis_hash'], 'genesis mismatch')
    pins = [(CREATE2, cfg['create2_code_hash'])]
    for name, hash_name in (('mailbox', 'mailbox_code_hash'), ('bridgehub', 'bridgehub_code_hash'),
                            ('gateway_chain_address', 'gateway_chain_code_hash'), ('gate', 'gate_code_hash'),
                            ('root_source', 'root_source_code_hash'), ('roster_source', 'roster_source_code_hash'),
                            ('child_chain_address', 'child_chain_code_hash')):
        if hash_name in cfg:
            pins.append((cfg[name], cfg[hash_name]))
    for address, expected in pins:
        code = common.rpc_code(rpc, address)
        require(code != '0x' and common.digest(code) == expected, 'pinned contract code mismatch')
    if cfg['kind'] == 'root_priority_source':
        actual = int(common.call_word(rpc, cfg['mailbox'], 'getChainId()'), 16)
        require(actual == cfg['child_chain_id'], 'mailbox child chain mismatch')
    elif cfg['kind'] == 'root_message_sink':
        actual = int(common.call_word(rpc, cfg['gateway_chain_address'], 'getChainId()'), 16)
        require(actual == cfg['gateway_chain_id'], 'gateway chain mismatch')
        data = '0x' + relay.selector('getZKChain(uint256)').hex() + format(cfg['gateway_chain_id'], '064x')
        registered = rpc.call('eth_call', [{'to': cfg['bridgehub'], 'data': data}, 'latest'])
        require(type(registered) is str and registered.lower() == '0x' + '00' * 12 + cfg['gateway_chain_address'][2:],
                'gateway diamond is not registered with bridgehub')
    elif cfg['kind'] in ('roster_receiver', 'root_draw'):
        expected = cfg['mailbox_chain_id'] if cfg['kind'] == 'roster_receiver' else cfg['gateway_chain_id']
        actual = int(common.call_word(rpc, cfg['mailbox'], 'getChainId()'), 16)
        require(actual == expected, 'roster or draw mailbox chain mismatch')
    elif cfg['kind'] == 'coordinator':
        gate = cfg['gate']
        for field, selector in [('child_chain_address', 'chain()'), ('sequencer', 'sequencer()')]:
            common.assert_address(rpc, gate, selector, cfg[field])
        for field, selector in [('child_chain_id', 'childChainId()')]:
            require(int(common.call_word(rpc, gate, selector), 16) == cfg[field], 'gate chain mismatch')
        require(common.call_word(rpc, gate, 'policyHash()') == cfg['policy_hash'], 'gate policy mismatch')
        require(common.call_word(rpc, gate, 'productionVkHash()') == cfg['production_vk_hash'], 'gate VK mismatch')
        require(int(common.call_word(rpc, cfg['child_chain_address'], 'getChainId()'), 16) == cfg['child_chain_id'],
                'child chain mismatch')
        require(common.call_word(rpc, cfg['roster_source'], 'policyHash()') == cfg['policy_hash'],
                'roster policy mismatch')
        require(int(common.call_word(rpc, cfg['roster_source'], 'firstServicePeriod()'), 16)
                == cfg['first_service_period'], 'roster first period mismatch')
        require(int(common.call_word(rpc, cfg['roster_source'], 'periodSeconds()'), 16) > 0,
                'invalid roster period')
        sink = common.call_address(rpc, gate, 'messageSink()')
        require((sink != ZERO) == cfg['native_root_draw'], 'draw lane topology mismatch')


def verify_source_drained(rpc, cfg):
    if cfg['kind'] != 'root_priority_source':
        return
    common.verify_queue_drained(rpc, {'chain': cfg['mailbox']})


def verify_coordinator_parent(rpc, cfg, plan):
    if cfg['kind'] != 'coordinator':
        return
    gate = cfg['gate']
    require(common.call_address(rpc, gate, 'coordinator()') == ZERO, 'gate already has a coordinator')
    require(int(common.call_word(rpc, gate, 'serviceActive()'), 16) == 0, 'gate already active')
    require(int(common.call_word(rpc, gate, 'priorityWorkId()'), 16) == 0, 'bootstrap work is still open')
    require(common.call_word(rpc, gate, 'lastAcceptedPackage()') == cfg['initial_parent'], 'bootstrap parent changed')
    verified = int(common.call_word(rpc, cfg['child_chain_address'], 'getTotalBatchesVerified()'), 16)
    require(verified + 1 == cfg['first_batch'], 'bootstrap next batch changed')
    if cfg['native_root_draw']:
        common.assert_address(rpc, cfg['root_source'], 'coordinator()', plan['component'])


def attest_component(rpc, cfg, plan):
    commitment = common.call_word(rpc, plan['factory'], 'deploymentCommitment()')
    if int(commitment, 16) == 0:
        require(common.rpc_code(rpc, plan['component']) == '0x', 'unexpected pre-existing component')
        return False
    require(commitment == plan['commitment'], 'factory deployment commitment mismatch')
    require(common.rpc_code(rpc, plan['component']) != '0x', 'component code missing')
    common.assert_address(rpc, plan['factory'], 'component()', plan['component'])
    address = plan['component']
    if cfg['kind'] not in ('root_draw_receiver', 'root_draw', 'native_root_draw'):
        require(common.call_word(rpc, address, 'policyHash()') == cfg['policy_hash'], 'component policy mismatch')
    if cfg['kind'] == 'root_priority_source':
        for field, selector in [('mailbox', 'mailbox()'), ('bridgehub', 'bridgehub()'),
                                ('gateway_receiver', 'gatewayReceiver()')]:
            common.assert_address(rpc, address, selector, cfg[field])
        for field, selector in [('child_chain_id', 'childChainId()'), ('gateway_chain_id', 'gatewayChainId()'),
                                ('inclusion_delay', 'inclusionDelay()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
    elif cfg['kind'] == 'gateway_checkpoint_receiver':
        alias = (int(cfg['root_source'], 16) + int('1111000000000000000000000000000000001111', 16)) % 2**160
        common.assert_address(rpc, address, 'aliasedRootSource()', '0x' + format(alias, '040x'))
        require(int(common.call_word(rpc, address, 'childChainId()'), 16) == cfg['child_chain_id'],
                'receiver child chain mismatch')
    elif cfg['kind'] == 'root_message_sink':
        for field, selector in [('publisher', 'publisher()'), ('gateway_gate', 'gatewayGate()'), ('bridgehub', 'bridgehub()'),
                                ('gateway_chain_address', 'gatewayChainAddress()'),
                                ('registry_receiver', 'registryReceiver()')]:
            common.assert_address(rpc, address, selector, cfg[field])
        for field, selector in [('gateway_chain_id', 'gatewayChainId()'), ('registry_chain_id', 'registryChainId()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
    elif cfg['kind'] == 'roster_receiver':
        for field, selector in [('child_registry', 'childRegistry()'), ('message_sender', 'messageSender()'),
                                ('mailbox', 'childMailbox()')]:
            common.assert_address(rpc, address, selector, cfg[field])
        for field, selector in [('child_chain_id', 'childChainId()'), ('start_time', 'startTime()'),
                                ('period_seconds', 'periodSeconds()'), ('first_service_period', 'firstServicePeriod()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
    elif cfg['kind'] == 'root_draw_receiver':
        alias = (int(cfg['root_source'], 16) + int('1111000000000000000000000000000000001111', 16)) % 2**160
        common.assert_address(rpc, address, 'aliasedRootSource()', '0x' + format(alias, '040x'))
    elif cfg['kind'] == 'root_draw':
        for field, selector in [('mailbox', 'gatewayMailbox()'), ('bridgehub', 'bridgehub()'),
                                ('gateway_coordinator', 'gatewayCoordinator()'),
                                ('gateway_receiver', 'gatewayReceiver()')]:
            common.assert_address(rpc, address, selector, cfg[field])
        for field, selector in [('gateway_chain_id', 'gatewayChainId()'), ('future_blocks', 'futureBlocks()'),
                                ('confirmations', 'confirmations()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
    elif cfg['kind'] == 'native_root_draw':
        common.assert_address(rpc, address, 'coordinator()', cfg['coordinator'])
        for field, selector in [('future_blocks', 'futureBlocks()'), ('confirmations', 'confirmations()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
    else:
        for field, selector in [('gate', 'acceptanceGate()'), ('root_source', 'rootSource()'),
                                ('roster_source', 'rosterSource()'), ('sequencer', 'sequencer()'),
                                ('child_chain_address', 'childChainAddress()')]:
            common.assert_address(rpc, address, selector, cfg[field])
        for field, selector in [('child_chain_id', 'childChainId()'), ('first_service_period', 'firstServicePeriod()'),
                                ('turn_seconds', 'turnSeconds()')]:
            require(int(common.call_word(rpc, address, selector), 16) == cfg[field], selector + ' mismatch')
        require(common.call_word(rpc, address, 'productionVkHash()') == cfg['production_vk_hash'], 'coordinator VK mismatch')
        require(int(common.call_word(rpc, address, 'nativeRootDraw()'), 16) == int(cfg['native_root_draw']),
                'coordinator draw lane mismatch')
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, help='private JSON component configuration')
    parser.add_argument('--state-dir', help='private journal directory, required except for --plan')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--plan', action='store_true', help='compute address without checking any chain')
    mode.add_argument('--send', action='store_true', help='sign and submit one stage')
    args = parser.parse_args()
    cfg = component_config(service.read_json(Path(args.config), private=True))
    plan = build(cfg)
    if args.plan:
        print(json.dumps({'status': 'unverified_plan', 'kind': cfg['kind'], 'factory': plan['factory'],
                          'component': plan['component'], 'commitment': plan['commitment']}, sort_keys=True))
        return
    require(bool(args.state_dir), 'state directory required')
    root = Path(args.state_dir)
    common.private_directory(root)
    lock = os.open(root / 'component-deploy.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    info = os.fstat(lock)
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, 'unsafe lock file')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise TransactionError('component deployment already running') from None
    url = os.environ.get('ZKSYS_SERVICE_SETTLEMENT_RPC_URL', '')
    require(bool(url), 'missing settlement RPC URL')
    rpc = relay.Rpc(url, authorization=os.environ.get('ZKSYS_SERVICE_SETTLEMENT_RPC_AUTHORIZATION'))
    verify_inputs(rpc, cfg)
    fingerprint = hashlib.sha256(service.canonical({'config': cfg, 'plan': plan})).hexdigest()
    path = root / 'component-deploy.json'
    if path.exists() or path.is_symlink():
        journal = service.read_json(path, private=True)
        require(type(journal) is dict and journal.get('version') == 1 and journal.get('fingerprint') == fingerprint,
                'component journal identity mismatch')
    else:
        journal = {'version': 1, 'fingerprint': fingerprint, 'transactions': {}}
    save = lambda obj: relay.atomic_json(path, obj)
    factory_exists = common.attest_factory(rpc, cfg, plan)
    if not args.send:
        print(json.dumps({'factory': plan['factory'], 'component': plan['component'], 'kind': cfg['kind'],
                          'factory_exists': factory_exists,
                          'component_exists': attest_component(rpc, cfg, plan) if factory_exists else False}, sort_keys=True))
        return
    common.private_file(cfg['keystore'])
    common.private_file(cfg['password_file'])
    signer = CastSigner(url, cfg['keystore'], cfg['password_file'])
    address = signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
    require(address == cfg['sender'], 'signer mismatch')
    driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
    factory_tx = (CREATE2, cfg['factory_salt'] + plan['factory_init'][2:])
    component_tx = (plan['factory'], common.cast('calldata', 'deploy(bytes)', plan['component_init']).lower())
    if journal['transactions']:
        require(set(journal['transactions']) == {'deploy'}, 'invalid transaction journal')
        entry = journal['transactions']['deploy']
        require(type(entry) is dict and entry.get('kind') == 'deployment' and type(entry.get('metadata')) is dict,
                'invalid deployment transaction')
        step = entry['metadata'].get('step')
        require(step in ('factory', 'component'), 'unknown deployment step')
        target, data = factory_tx if step == 'factory' else component_tx
        expected = {'from': cfg['sender'], 'to': target, 'chainId': cfg['chain_id'], 'nonce': entry['nonce'],
                    'value': 0, 'gas': cfg['gas_limit'], 'gasPrice': cfg['gas_price'], 'data': data}
        require(_same_transaction(expected, signer.decode(entry['raw'])), 'journal transaction identity mismatch')
        receipt = driver.receipt('deploy', rpc.call)
        if receipt is None:
            driver.rebroadcast_if_missing('deploy', rpc.call)
            print('pending')
            return
        require(common.attest_factory(rpc, cfg, plan), 'finalized factory missing')
        if step == 'component':
            require(attest_component(rpc, cfg, plan), 'finalized component missing')
        driver.complete('deploy')
        factory_exists = True
    if not factory_exists:
        target, data, step = *factory_tx, 'factory'
    elif not attest_component(rpc, cfg, plan):
        verify_source_drained(rpc, cfg)
        verify_coordinator_parent(rpc, cfg, plan)
        target, data, step = *component_tx, 'component'
    else:
        print(json.dumps({'status': 'deployed', 'kind': cfg['kind'], 'factory': plan['factory'],
                          'component': plan['component']}, sort_keys=True))
        return
    save(journal)
    driver.begin('deploy', rpc.call, cfg['chain_id'], cfg['sender'], target, data, 0, cfg['gas_limit'],
                 cfg['gas_price'], kind='deployment', metadata={'step': step})
    print(json.dumps({'status': 'submitted', 'step': step, 'kind': cfg['kind']}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (TransactionError, service.Error, OSError, ValueError) as exc:
        print('service component deployment failed: ' + str(exc), file=sys.stderr)
        sys.exit(1)
