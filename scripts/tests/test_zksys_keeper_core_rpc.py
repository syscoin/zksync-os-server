import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location('zksys_keeper_cli_rpc_test', SCRIPTS / 'zksys-membership-keeper.py')
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class Response:
    status = 200

    def __init__(self, result):
        self.data = json.dumps({'jsonrpc': '2.0', 'id': 1, 'result': result}).encode()
        self.offset = 0

    def __enter__(self): return self
    def __exit__(self, *args): return False

    def read1(self, size):
        part = self.data[self.offset:self.offset + size]
        self.offset += len(part)
        return part


class CoreRpcLimitTests(unittest.TestCase):
    def test_large_core_list_is_not_subject_to_two_mib_prover_limit(self):
        rpc = cli.CoreRpc('http://127.0.0.1:1')
        body = 'a' * (2 * 1024 * 1024 + 1)
        with patch.object(rpc.opener, 'open', return_value=Response([body])):
            self.assertEqual(rpc.call('protx_list', ['registered', True, 1]), [body])

    def test_core_list_over_configured_limit_fails_closed(self):
        rpc = cli.CoreRpc('http://127.0.0.1:1')
        rpc.MAX_RESPONSE = 128
        with patch.object(rpc.opener, 'open', return_value=Response(['a' * 256])):
            with self.assertRaisesRegex(cli.service.Error, 'rpc_response_limit'):
                rpc.call('protx_list', ['registered', True, 1])

    def test_other_methods_keep_standard_limit(self):
        rpc = cli.CoreRpc('http://127.0.0.1:1')
        body = 'a' * (2 * 1024 * 1024 + 1)
        with patch.object(rpc.opener, 'open', return_value=Response(body)):
            with self.assertRaisesRegex(cli.service.Error, 'rpc_response_limit'):
                rpc.call('getblockheader', ['f' * 64])

class IntegratedDiscoveryTests(unittest.TestCase):
    def test_full_report_uses_rpc_and_recovers_history_after_partial_failure(self):
        import contextlib
        import io
        import tempfile
        from unittest.mock import patch
        a, b, bridge, membership = ('0x' + x * 40 for x in '1234')
        l1_hash, l2_hash = '0x' + 'aa' * 32, '0x' + 'bb' * 32
        core_hash = '01' + '00' * 31
        raw_hash = bytes.fromhex(core_hash)[::-1].hex()
        state = {'fail_reconcile': True, 'phase': 0}
        def w(n): return n.to_bytes(32, 'big').hex()
        def wa(a): return '00' * 12 + a[2:]
        def selector(s): return '0x' + cli.k.selector(s).hex()
        class RPC:
            def __init__(self, chain): self.chain = chain
            def call(self, method, params):
                if self.chain == 'core':
                    if method == 'getblockheader':
                        if params[0] != core_hash: raise AssertionError(params)
                        return {'hash': core_hash, 'height': 100}
                    if method == 'getblockhash': return core_hash
                    if method == 'protx_list':
                        return [{'proTxHash': '01' * 32, 'state': {'nevmAddress': a if state['phase'] == 0 else b}}]
                if method == 'eth_chainId': return '0x39' if self.chain == 'l1' else '0x3a'
                if method == 'eth_getBlockByNumber': return {'hash': l1_hash if self.chain == 'l1' else l2_hash, 'number': '0xa'}
                if method == 'eth_getCode': return '0x6000'
                if method == 'eth_call':
                    tx, _ = params
                    data, to = tx['data'], tx['to']
                    if to.endswith('61'): return '0x' + raw_hash
                    sig = data[:10]
                    arg = '0x' + data[-40:]
                    if sig == selector('l2Registry()'): return '0x' + wa(membership)
                    if sig == selector('zksysChainId()'): return '0x' + w(58)
                    if sig == selector('l1RegistryBridge()'): return '0x' + wa(bridge)
                    if sig == selector('activeSentryNodeCount()'): return '0x' + w(1)
                    if sig == selector('activeSentryNodeAt(uint256)'): return '0x' + wa(b)
                    if sig == selector('nevmCollateralHeight(address)'):
                        if state['fail_reconcile']: raise cli.service.Error('simulated_reconcile_failure')
                        return '0x' + w(100 if arg == a else 0)
                    if sig == selector('nevmSentryNodeWeight(address)'): return '0x' + w(100000 if arg == a else 0)
                    if sig == selector('member(address)'): return '0x' + w(0 if arg == a else 100) + w(0 if arg == a else 100000)
                raise AssertionError((self.chain, method, params))
        clients = {'ZKSYS_CORE': RPC('core'), 'ZKSYS_L1': RPC('l1'), 'ZKSYS_L2': RPC('l2')}
        with tempfile.TemporaryDirectory() as d:
            argv = ['keeper', '--state-dir', d, '--bridge', bridge, '--membership', membership,
                    '--l1-chain-id', '57', '--l2-chain-id', '58']
            with patch.object(cli, 'env_rpc', side_effect=lambda n: clients[n]), patch.object(sys, 'argv', argv), \
                    contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(cli.main(), 1)
                saved = json.loads((Path(d) / 'discovery.json').read_text())
                self.assertEqual(saved['ever_seen'], [a, b])
                state.update(fail_reconcile=False, phase=1)
                self.assertEqual(cli.main(), 0)
                result = json.loads(out.getvalue())
                self.assertEqual(result['known_addresses'], 2)
                self.assertEqual([change['account'] for change in result['changes']], [b, a])


if __name__ == '__main__':
    unittest.main()
