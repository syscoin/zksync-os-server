#!/usr/bin/env python3
"""Deploy one service proof gate and priority guard after the native chains exist.

The lane's checkpoint receiver/source and root message sink must already exist.
This stage does not change timelock roles or activate the service.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
import zksys_membership_keeper as membership
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionError, _same_transaction
from service_transaction_driver import ServiceTransactionDriver
import relay
import service

ADDRESS = re.compile(r'0x[0-9a-fA-F]{40}\Z')
HASH = re.compile(r'0x[0-9a-fA-F]{64}\Z')
HEX = re.compile(r'0x(?:[0-9a-fA-F]{2})*\Z')
ZERO = '0x' + '00' * 20
CREATE2 = '0x4e59b44847b379578588920ca78fbf26c0b4956c'


def require(condition, message):
    if not condition:
        raise TransactionError(message)


def command(program, args):
    try:
        result = subprocess.run([program, *args], text=True, capture_output=True, check=True, timeout=180,
                                cwd=ROOT)
    except (OSError, subprocess.SubprocessError):
        raise TransactionError(program + ' command failed') from None
    return result.stdout.strip()


def cast(*args):
    return command('cast', list(args))


def bytecode(contract):
    result = command('forge', ['inspect', '--root', 'contracts', '--remappings', 'contracts/=./',
                               'src/zksys/' + contract + '.sol:' + contract, 'bytecode'])
    require(HEX.fullmatch(result) is not None and len(result) > 2, 'invalid compiler bytecode')
    return result.lower()


def digest(value):
    result = cast('keccak', value)
    require(HASH.fullmatch(result) is not None, 'invalid keccak output')
    return result.lower()


def encode(signature, *args):
    output = cast('abi-encode', signature, *[str(x) for x in args])
    require(HEX.fullmatch(output) is not None, 'invalid ABI output')
    return output[2:].lower()


def config(raw):
    fields = {'chain_id', 'genesis_hash', 'sender', 'chain', 'chain_code_hash', 'timelock',
              'timelock_code_hash', 'checkpoint', 'checkpoint_code_hash', 'message_sink',
              'message_sink_code_hash', 'sequencer', 'policy_hash', 'production_vk_hash',
              'child_chain_id', 'max_snapshot_age', 'max_proof_work_seconds', 'max_priority_per_package',
              'factory_salt', 'create2_code_hash', 'gas_limit', 'gas_price', 'keystore', 'password_file'}
    require(type(raw) is dict and set(raw) == fields, 'invalid configuration fields')
    value = dict(raw)
    for key in ('sender', 'chain', 'timelock', 'checkpoint', 'message_sink', 'sequencer'):
        require(type(value[key]) is str and ADDRESS.fullmatch(value[key]) is not None, 'invalid ' + key)
        value[key] = value[key].lower()
        if key != 'message_sink':
            require(value[key] != ZERO, 'zero ' + key)
    for key in ('genesis_hash', 'chain_code_hash', 'timelock_code_hash', 'checkpoint_code_hash',
                'message_sink_code_hash', 'policy_hash', 'production_vk_hash', 'factory_salt', 'create2_code_hash'):
        require(type(value[key]) is str and HASH.fullmatch(value[key]) is not None, 'invalid ' + key)
        value[key] = value[key].lower()
        if key != 'message_sink_code_hash':
            require(int(value[key], 16) != 0, 'zero ' + key)
    require((value['message_sink'] == ZERO) == (int(value['message_sink_code_hash'], 16) == 0),
            'sink and code hash inconsistent')
    for key in ('chain_id', 'child_chain_id', 'max_snapshot_age', 'max_proof_work_seconds',
                'max_priority_per_package', 'gas_limit', 'gas_price'):
        require(type(value[key]) is int and value[key] > 0, 'invalid ' + key)
    require(value['chain_id'] != value['child_chain_id'], 'settlement and child chain ids must differ')
    require(value['max_priority_per_package'] <= 1024, 'priority package limit exceeds contract maximum')
    for key in ('max_snapshot_age', 'max_proof_work_seconds', 'max_priority_per_package'):
        require(value[key] < 2**64, 'invalid ' + key)
    for key in ('keystore', 'password_file'):
        require(type(value[key]) is str and Path(value[key]).is_absolute(), 'invalid ' + key)
    return value


def build(cfg):
    factory_init = bytecode('ZkSysServiceGateDeploymentFactoryV1') + encode('constructor(address)', cfg['sender'])
    factory = cast('create2', '--deployer', CREATE2, '--salt', cfg['factory_salt'], '--init-code', factory_init).lower()
    require(ADDRESS.fullmatch(factory) is not None, 'invalid factory address')
    guard = cast('compute-address', factory, '--nonce', '1').split()[-1].lower()
    gate = cast('compute-address', factory, '--nonce', '2').split()[-1].lower()
    require(ADDRESS.fullmatch(guard) and ADDRESS.fullmatch(gate), 'invalid predicted addresses')
    guard_init = bytecode('ZkSysPriorityGuardV1') + encode(
        'constructor(address,address,address,bytes32,uint64,uint64,uint64)', gate, cfg['chain'],
        cfg['checkpoint'], cfg['policy_hash'], cfg['max_snapshot_age'], cfg['max_proof_work_seconds'],
        cfg['max_priority_per_package'])
    gate_init = bytecode('ZkSysProofGateV1') + encode(
        'constructor(address,address,address,bytes32,bytes32,address,address)', cfg['chain'], cfg['timelock'],
        cfg['sequencer'], cfg['policy_hash'], cfg['production_vk_hash'], guard, cfg['message_sink'])
    commitment = digest('0x' + encode('f(bytes,bytes)', guard_init, gate_init))
    return {'factory': factory, 'guard': guard, 'gate': gate, 'factory_init': factory_init,
            'guard_init': guard_init, 'gate_init': gate_init, 'commitment': commitment}


def rpc_code(rpc, address, block='latest'):
    code = rpc.call('eth_getCode', [address, block])
    require(type(code) is str and HEX.fullmatch(code) is not None, 'invalid contract code')
    return code.lower()


def call_word(rpc, address, signature):
    result = rpc.call('eth_call', [{'to': address, 'data': '0x' + cast('sig', signature)[2:]}, 'latest'])
    require(type(result) is str and re.fullmatch(r'0x[0-9a-fA-F]{64}', result), 'invalid contract response')
    return result.lower()


def call_address(rpc, address, signature):
    value = call_word(rpc, address, signature)
    require(value[2:26] == '0' * 24, 'invalid address response')
    return '0x' + value[-40:]


def assert_address(rpc, address, signature, expected):
    require(call_address(rpc, address, signature) == expected, signature + ' mismatch')


def verify_inputs(rpc, cfg, plan):
    require(membership.hexint(rpc.call('eth_chainId', [])) == cfg['chain_id'], 'chain id mismatch')
    genesis = rpc.call('eth_getBlockByNumber', ['0x0', False])
    require(type(genesis) is dict and genesis.get('hash', '').lower() == cfg['genesis_hash'], 'genesis mismatch')
    pins = [(CREATE2, cfg['create2_code_hash']), (cfg['chain'], cfg['chain_code_hash']),
            (cfg['timelock'], cfg['timelock_code_hash']), (cfg['checkpoint'], cfg['checkpoint_code_hash'])]
    if cfg['message_sink'] != ZERO:
        pins.append((cfg['message_sink'], cfg['message_sink_code_hash']))
    for address, expected in pins:
        code = rpc_code(rpc, address)
        require(code != '0x' and digest(code) == expected, 'pinned contract code mismatch')
    require(int(call_word(rpc, cfg['chain'], 'getChainId()'), 16) == cfg['child_chain_id'], 'native chain id mismatch')
    require(int(call_word(rpc, cfg['checkpoint'], 'childChainId()'), 16) == cfg['child_chain_id'], 'checkpoint chain mismatch')
    require(call_word(rpc, cfg['checkpoint'], 'policyHash()') == cfg['policy_hash'], 'checkpoint policy mismatch')
    if cfg['message_sink'] != ZERO:
        assert_address(rpc, cfg['message_sink'], 'publisher()', plan['gate'])
        require(call_word(rpc, cfg['message_sink'], 'policyHash()') == cfg['policy_hash'], 'sink policy mismatch')

def verify_queue_drained(rpc, cfg):
    require(int(call_word(rpc, cfg['chain'], 'getTotalBatchesVerified()'), 16)
            == int(call_word(rpc, cfg['chain'], 'getTotalBatchesExecuted()'), 16), 'verified batches must be executed')
    require(int(call_word(rpc, cfg['chain'], 'getFirstUnprocessedPriorityTx()'), 16)
            == int(call_word(rpc, cfg['chain'], 'getTotalPriorityTxs()'), 16), 'priority queue must be drained')
    require(int(call_word(rpc, cfg['chain'], 'isPriorityQueueActive()'), 16) == 0, 'priority queue active')


def attest_factory(rpc, cfg, plan):
    code = rpc_code(rpc, plan['factory'])
    if code == '0x':
        return False
    expected = rpc.call('eth_call', [{'from': cfg['sender'], 'data': plan['factory_init']}, 'latest'])
    require(type(expected) is str and HEX.fullmatch(expected) and expected.lower() == code,
            'factory runtime mismatch')
    assert_address(rpc, plan['factory'], 'owner()', cfg['sender'])
    return True


def attest_gate(rpc, cfg, plan):
    commitment = call_word(rpc, plan['factory'], 'deploymentCommitment()')
    if int(commitment, 16) == 0:
        require(rpc_code(rpc, plan['guard']) == '0x' and rpc_code(rpc, plan['gate']) == '0x',
                'unexpected pre-existing gate or guard')
        return False
    require(commitment == plan['commitment'], 'factory deployment commitment mismatch')
    require(rpc_code(rpc, plan['guard']) != '0x' and rpc_code(rpc, plan['gate']) != '0x', 'missing gate or guard')
    assert_address(rpc, plan['factory'], 'gate()', plan['gate'])
    assert_address(rpc, plan['guard'], 'acceptanceGate()', plan['gate'])
    assert_address(rpc, plan['gate'], 'priorityGuard()', plan['guard'])
    assert_address(rpc, plan['gate'], 'deploymentAuthority()', plan['factory'])
    assert_address(rpc, plan['gate'], 'chain()', cfg['chain'])
    assert_address(rpc, plan['gate'], 'timelock()', cfg['timelock'])
    assert_address(rpc, plan['gate'], 'sequencer()', cfg['sequencer'])
    assert_address(rpc, plan['gate'], 'messageSink()', cfg['message_sink'])
    require(call_word(rpc, plan['gate'], 'policyHash()') == cfg['policy_hash'], 'gate policy mismatch')
    require(call_word(rpc, plan['gate'], 'productionVkHash()') == cfg['production_vk_hash'], 'gate VK mismatch')
    return True


def private_file(path):
    info = Path(path).lstat()
    require(stat.S_ISREG(info.st_mode) and not stat.S_ISLNK(info.st_mode) and info.st_uid == os.getuid()
            and info.st_nlink == 1 and not info.st_mode & 0o077, 'signer file must be private')


def private_directory(path):
    require(path.is_absolute(), 'state directory must be absolute')
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode) and info.st_uid == os.getuid()
            and not info.st_mode & 0o077, 'state directory must be private')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, help='private JSON config for one settlement lane')
    parser.add_argument('--state-dir', help='existing private journal directory, required except for --plan')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--send', action='store_true', help='sign and submit one stage')
    mode.add_argument('--plan', action='store_true', help='compute addresses only, without checking any chain')
    parser.add_argument('--coordinator', help='install this already-deployed coordinator after bootstrap')
    parser.add_argument('--coordinator-code-hash', help='reviewed runtime hash of that coordinator')
    args = parser.parse_args()
    require(bool(args.coordinator) == bool(args.coordinator_code_hash), 'coordinator and code hash must be supplied together')
    if args.coordinator:
        require(ADDRESS.fullmatch(args.coordinator) is not None and args.coordinator.lower() != ZERO
                and HASH.fullmatch(args.coordinator_code_hash) is not None
                and int(args.coordinator_code_hash, 16) != 0, 'invalid coordinator identity')
        args.coordinator = args.coordinator.lower()
        args.coordinator_code_hash = args.coordinator_code_hash.lower()
    cfg = config(service.read_json(Path(args.config), private=True))
    if args.plan:
        require(not args.coordinator, 'coordinator not applicable to planning')
        plan = build(cfg)
        print(json.dumps({'status': 'unverified_plan', 'factory': plan['factory'], 'guard': plan['guard'],
                          'gate': plan['gate'], 'commitment': plan['commitment']}, sort_keys=True))
        return
    require(bool(args.state_dir), 'state directory required')
    state_dir = Path(args.state_dir)
    private_directory(state_dir)
    lock = os.open(state_dir / 'gate-deploy.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    lock_info = os.fstat(lock)
    require(stat.S_ISREG(lock_info.st_mode) and lock_info.st_uid == os.getuid() and not lock_info.st_mode & 0o077,
            'unsafe lock file')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise TransactionError('deployment already running') from None
    url = os.environ.get('ZKSYS_SERVICE_SETTLEMENT_RPC_URL', '')
    require(bool(url), 'missing settlement RPC URL')
    rpc = relay.Rpc(url, authorization=os.environ.get('ZKSYS_SERVICE_SETTLEMENT_RPC_AUTHORIZATION'))
    plan = build(cfg)
    verify_inputs(rpc, cfg, plan)
    fingerprint = hashlib.sha256(service.canonical({'config': cfg, 'plan': plan})).hexdigest()
    path = state_dir / 'gate-deploy.json'
    if path.exists() or path.is_symlink():
        journal = service.read_json(path, private=True)
        require(type(journal) is dict and journal.get('version') == 1 and journal.get('fingerprint') == fingerprint,
                'deployment journal identity mismatch')
    else:
        journal = {'version': 1, 'fingerprint': fingerprint, 'transactions': {}}
    save = lambda obj: relay.atomic_json(path, obj)
    factory_exists = attest_factory(rpc, cfg, plan)
    if not args.send:
        print(json.dumps({'factory': plan['factory'], 'guard': plan['guard'], 'gate': plan['gate'],
                          'factory_exists': factory_exists,
                          'gate_exists': attest_gate(rpc, cfg, plan) if factory_exists else False}, sort_keys=True))
        return
    private_file(cfg['keystore'])
    private_file(cfg['password_file'])
    signer = CastSigner(url, cfg['keystore'], cfg['password_file'])
    address = signer._run(['wallet', 'address', '--keystore', cfg['keystore'], '--password-file', cfg['password_file']]).lower()
    require(address == cfg['sender'], 'signer mismatch')
    driver = ServiceTransactionDriver(journal, save, signer.sign, signer.decode, signer.hash_raw)
    factory_tx = (CREATE2, cfg['factory_salt'] + plan['factory_init'][2:])
    gate_tx = (plan['factory'], cast('calldata', 'deploy(bytes,bytes)', plan['guard_init'], plan['gate_init']).lower())
    coordinator_tx = ((plan['factory'], cast('calldata', 'installCoordinator(address)', args.coordinator).lower())
                      if args.coordinator else None)
    if journal['transactions']:
        require(set(journal['transactions']) == {'deploy'}, 'invalid transaction journal')
        entry = journal['transactions']['deploy']
        require(type(entry) is dict and entry.get('kind') == 'deployment' and type(entry.get('metadata')) is dict,
                'invalid deployment transaction')
        step = entry['metadata'].get('step')
        require(step in ('factory', 'guard_and_gate', 'install_coordinator'), 'unknown deployment step')
        if step == 'install_coordinator':
            require(coordinator_tx is not None and entry['metadata'].get('coordinator') == args.coordinator
                    and entry['metadata'].get('coordinator_code_hash') == args.coordinator_code_hash,
                    'journal coordinator identity mismatch')
            target, data = coordinator_tx
        else:
            target, data = factory_tx if step == 'factory' else gate_tx
        expected = {'from': cfg['sender'], 'to': target, 'chainId': cfg['chain_id'], 'nonce': entry['nonce'],
                    'value': 0, 'gas': cfg['gas_limit'], 'gasPrice': cfg['gas_price'], 'data': data}
        require(_same_transaction(expected, signer.decode(entry['raw'])), 'journal transaction identity mismatch')
        receipt = driver.receipt('deploy', rpc.call)
        if receipt is None:
            driver.rebroadcast_if_missing('deploy', rpc.call)
            print('pending')
            return
        require(attest_factory(rpc, cfg, plan), 'finalized factory missing')
        if step in ('guard_and_gate', 'install_coordinator'):
            require(attest_gate(rpc, cfg, plan), 'finalized gate missing')
        if step == 'install_coordinator':
            assert_address(rpc, plan['gate'], 'coordinator()', args.coordinator)
        driver.complete('deploy')
        factory_exists = True
    if not factory_exists:
        target, data, step = *factory_tx, 'factory'
    elif not attest_gate(rpc, cfg, plan):
        verify_queue_drained(rpc, cfg)
        target, data, step = *gate_tx, 'guard_and_gate'
    elif args.coordinator:
        code = rpc_code(rpc, args.coordinator)
        require(code != '0x' and digest(code) == args.coordinator_code_hash, 'coordinator runtime mismatch')
        current = call_address(rpc, plan['gate'], 'coordinator()')
        if current == args.coordinator:
            print(json.dumps({'status': 'coordinator_installed', 'gate': plan['gate'], 'coordinator': current}, sort_keys=True))
            return
        require(current == ZERO, 'gate has a different coordinator')
        target, data, step = *coordinator_tx, 'install_coordinator'
    else:
        print(json.dumps({'status': 'deployed', 'factory': plan['factory'], 'guard': plan['guard'], 'gate': plan['gate']}, sort_keys=True))
        return
    metadata = {'step': step}
    if step == 'install_coordinator':
        metadata.update(coordinator=args.coordinator, coordinator_code_hash=args.coordinator_code_hash)
    save(journal)
    driver.begin('deploy', rpc.call, cfg['chain_id'], cfg['sender'], target, data, 0, cfg['gas_limit'], cfg['gas_price'],
                 kind='deployment', metadata=metadata)
    print(json.dumps({'status': 'submitted', 'step': step}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (TransactionError, service.Error, OSError, ValueError) as exc:
        print('service gate deployment failed: ' + str(exc), file=sys.stderr)
        sys.exit(1)
