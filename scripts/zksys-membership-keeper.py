#!/usr/bin/env python3
"""zkSYS membership discovery and opt-in transaction keeper."""
import argparse
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import stat
import sys
import time
import urllib.error
import urllib.request

import zksys_membership_keeper as k

sys.path.insert(0, str(Path(__file__).parent / 'prover-service'))
import relay
import service


class CoreRpc(relay.Rpc):
    # Core's detailed deterministic list is unpaginated and can be much larger
    # than the per-response cap used by the proving service.
    MAX_RESPONSE = 64 * 1024 * 1024

    def call(self, method, params):
        if method != 'protx_list':
            return super().call(method, params)
        self.request_id += 1
        request_id = self.request_id
        payload = service.canonical({'jsonrpc': '2.0', 'id': request_id, 'method': method, 'params': params})
        headers = {'Content-Type': 'application/json', 'Accept-Encoding': 'identity'}
        if self.authorization:
            headers['Authorization'] = self.authorization
        request = urllib.request.Request(self.url, payload, headers=headers)
        deadline = time.monotonic() + self.timeout
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                service.require(response.status == 200, 'rpc_http_failure')
                data = bytearray()
                while part := response.read1(min(65536, self.MAX_RESPONSE - len(data) + 1)):
                    service.require(time.monotonic() < deadline and len(data) + len(part) <= self.MAX_RESPONSE,
                                    'rpc_response_limit')
                    data.extend(part)
            result = json.loads(data, object_pairs_hook=service._unique)
        except (urllib.error.URLError, OSError, http.client.HTTPException, ValueError, UnicodeError):
            raise service.Error('rpc_transport_failure') from None
        service.require(type(result) is dict and result.get('jsonrpc') == '2.0'
                        and type(result.get('id')) is int and result['id'] == request_id, 'invalid_rpc_response')
        if 'error' in result:
            error = result['error']
            raise relay.RpcError(error.get('code') if isinstance(error, dict) else None,
                                 error.get('data') if isinstance(error, dict) else None)
        service.require('result' in result, 'missing_rpc_result')
        return result['result']


def env_rpc(name):
    url = os.environ.get(name + '_RPC_URL')
    k.require(bool(url), 'missing_' + name.lower() + '_rpc_url')
    rpc_type = CoreRpc if name == 'ZKSYS_CORE' else relay.Rpc
    return rpc_type(url, authorization=os.environ.get(name + '_RPC_AUTHORIZATION'))


def addr_call(rpc, target, signature, anchor):
    raw = k.eth_call_word(rpc, target, signature, anchor)
    k.require(raw[:12] == bytes(12), 'invalid_address_response')
    return k.address('0x' + raw[12:].hex())


def main():
    parser = argparse.ArgumentParser(description='Report Sentry registry differences; explicitly opt in to transaction submission with --send.')
    parser.add_argument('--state-dir', required=True, help='Existing private directory for durable ever-seen address history')
    parser.add_argument('--bridge', required=True, help='L1 registry bridge proxy address')
    parser.add_argument('--membership', required=True, help='L2 membership registry proxy address')
    parser.add_argument('--l1-chain-id', required=True, type=int)
    parser.add_argument('--l2-chain-id', required=True, type=int)
    parser.add_argument('--watch', action='store_true', help='Poll continuously and print one JSON report per successful pass')
    parser.add_argument('--interval', type=int, default=60, help='Polling interval in seconds')
    parser.add_argument('--send', action='store_true', help='Enable real transactions using the explicitly attested configuration')
    parser.add_argument('--sync-only', action='store_true', help='Relay membership without submitting or retrying weight activations; requires --send')
    parser.add_argument('--config', help='Private JSON file with pinned contract identities, signers, and gas bounds')
    args = parser.parse_args()
    k.require(not args.sync_only or args.send, 'sync_only_requires_send')
    k.require(args.interval > 0 and args.l1_chain_id > 0 and args.l2_chain_id > 0 and args.l1_chain_id != args.l2_chain_id, 'invalid_configuration')
    bridge, membership = k.address(args.bridge), k.address(args.membership)
    root = Path(args.state_dir)
    k.require(root.is_absolute(), 'state_dir_must_be_absolute')
    info = root.lstat()
    k.require(stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode) and info.st_uid == os.getuid()
              and info.st_mode & 0o077 == 0, 'state_dir_must_be_private')
    lock = os.open(root / 'keeper.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    lock_info = os.fstat(lock)
    k.require(stat.S_ISREG(lock_info.st_mode) and lock_info.st_uid == os.getuid()
              and lock_info.st_mode & 0o077 == 0, 'unsafe_lock_file')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(lock)
        raise k.KeeperError('keeper_already_running') from None
    core, l1, l2 = env_rpc('ZKSYS_CORE'), env_rpc('ZKSYS_L1'), env_rpc('ZKSYS_L2')
    state_path = root / 'discovery.json'

    if args.send:
        k.require(bool(args.config), 'send_requires_config')
        from zksys_keeper_cast import CastSigner, hash_bytes
        from zksys_keeper_hooks import Hooks, validate_config
        from zksys_keeper_runner import KeeperRunner
        from zksys_keeper_transactions import TransactionDriver, TransactionError
        config = validate_config(service.read_json(Path(args.config), private=True))
        k.require(config.get('bridge', '').lower() == bridge and config.get('membership', '').lower() == membership
                  and config.get('l1_chain_id') == args.l1_chain_id and config.get('l2_chain_id') == args.l2_chain_id,
                  'cli_config_identity_mismatch')
        config['bridge_event_topic'] = hash_bytes(b'RegistryUpdatesRequested(bytes32,uint256)')
        config['priority_event_topic'] = hash_bytes(b'NewPriorityRequest(uint256,bytes32,uint64,(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256[4],bytes,bytes,uint256[],bytes,bytes),bytes[])')
        fingerprint = hashlib.sha256(json.dumps(config, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        path = root / 'transactions.json'
        if path.exists() or path.is_symlink():
            journal = service.read_json(path, private=True)
            k.require(type(journal) is dict and journal.get('config_sha256') == fingerprint, 'journal_identity_mismatch')
        else:
            journal = {'config_sha256': fingerprint, 'transactions': {}, 'ever_seen': []}
        from importlib import util
        module_path = Path(__file__).parent / 'gateway-launch' / '_checkpoint_state_io.py'
        spec = util.spec_from_file_location('checkpoint_state_io', module_path)
        module = util.module_from_spec(spec)
        spec.loader.exec_module(module)
        save = lambda obj: module.atomic_write_json(path, obj)
        if state_path.exists() or state_path.is_symlink():
            old = service.read_json(state_path, private=True)
            expected_old_identity = {'l1_chain_id': args.l1_chain_id, 'l2_chain_id': args.l2_chain_id,
                                     'l1_genesis': config['l1_genesis'].lower(), 'l2_genesis': config['l2_genesis'].lower(),
                                     'bridge': bridge, 'membership': membership}
            k.require(type(old) is dict and old.get('version') == 1 and old.get('identity') == expected_old_identity
                      and type(old.get('ever_seen')) is list, 'invalid_discovery_state')
            journal['ever_seen'] = sorted(set(journal['ever_seen']) | set(old['ever_seen']))
            save(journal)
        sign1 = CastSigner(os.environ['ZKSYS_L1_RPC_URL'], config['l1_keystore'], config['l1_password_file'])
        sign2 = CastSigner(os.environ['ZKSYS_L2_RPC_URL'], config['l2_keystore'], config['l2_password_file'])
        driver1 = TransactionDriver(journal, save, sign1.sign, sign1.decode, sign1.hash_raw)
        driver2 = TransactionDriver(journal, save, sign2.sign, sign2.decode, sign2.hash_raw)
        hooks = Hooks(core, l1, l2, journal, config, save)
        runner = KeeperRunner(journal, save, driver1, driver2, l1.call, l2.call, hooks, config, sync_only=args.sync_only)

        def once_live():
            print(json.dumps({'mode': 'send', 'result': runner.run_once()}, sort_keys=True), flush=True)
    else:
        k.require(not args.config, 'config_requires_send')

    def once():
        k.require(k.hexint(l1.call('eth_chainId', [])) == args.l1_chain_id and k.hexint(l2.call('eth_chainId', [])) == args.l2_chain_id, 'chain_id_mismatch')
        l1_genesis, l2_genesis = k.block(l1, '0x0'), k.block(l2, '0x0')
        l1_now, l2_now = k.block(l1, 'latest'), k.block(l2, 'latest')
        for rpc, target, anchor in ((l1, bridge, l1_now), (l2, membership, l2_now)):
            code = rpc.call('eth_getCode', [target, {'blockHash': anchor['hash'], 'requireCanonical': True}])
            k.require(isinstance(code, str) and code != '0x', 'contract_code_missing')
        k.require(addr_call(l1, bridge, 'l2Registry()', l1_now['hash']) == membership, 'bridge_registry_mismatch')
        chain = int.from_bytes(k.eth_call_word(l1, bridge, 'zksysChainId()', l1_now['hash']), 'big')
        k.require(chain == args.l2_chain_id, 'bridge_chain_mismatch')
        k.require(addr_call(l2, membership, 'l1RegistryBridge()', l2_now['hash']) == bridge, 'registry_bridge_mismatch')
        identity = {'l1_chain_id': args.l1_chain_id, 'l2_chain_id': args.l2_chain_id, 'l1_genesis': l1_genesis['hash'],
                    'l2_genesis': l2_genesis['hash'], 'bridge': bridge, 'membership': membership}
        if state_path.exists() or state_path.is_symlink():
            state = service.read_json(state_path, private=True)
            k.require(type(state) is dict and state.get('identity') == identity and state.get('version') == 1, 'state_identity_mismatch')
        else:
            state = {'version': 1, 'identity': identity, 'ever_seen': []}
        snap = k.snapshot(core, l1, l2, membership)
        accounts = k.merge_discovery(state, snap['entries'], snap['l2_active'])
        from importlib import util
        path = Path(__file__).parent / 'gateway-launch' / '_checkpoint_state_io.py'
        spec = util.spec_from_file_location('checkpoint_state_io', path)
        module = util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.atomic_write_json(state_path, state)
        # Reconciliation uses current L1 hints, not a historical precompile lookup.
        changes = k.reconcile(l1, l2, bridge, membership, accounts, l1_now, snap['l2_block'])
        print(json.dumps({'mode': 'read_only', 'core_height': snap['core_height'], 'core_hash': snap['core_hash'],
                          'l1_finalized': snap['l1_block'], 'l2_finalized': snap['l2_block'],
                          'known_addresses': len(accounts), 'changes': changes}, sort_keys=True), flush=True)

    try:
        while True:
            try:
                once_live() if args.send else once()
            except (k.KeeperError, service.Error, OSError, ValueError, KeyError, RuntimeError) as exc:
                print('keeper discovery failed: ' + str(exc), file=sys.stderr, flush=True)
                if not args.watch:
                    return 1
            if not args.watch:
                return 0
            time.sleep(args.interval)
    finally:
        os.close(lock)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (k.KeeperError, service.Error, OSError, ValueError, KeyError, RuntimeError) as exc:
        print('keeper configuration failed: ' + str(exc), file=sys.stderr)
        sys.exit(1)
