import json
import shutil
import socket
import subprocess
import sys
import time
import unittest
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from zksys_keeper_cast import CastSigner
from zksys_keeper_transactions import TransactionDriver


@unittest.skipUnless(shutil.which('anvil') and shutil.which('cast'), 'Foundry tools are required')
class AnvilRecoveryTest(unittest.TestCase):
    def test_evicted_transaction_rebroadcasts_identical_signed_bytes(self):
        # Anvil's published development key is used only on this disposable chain.
        key = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80'
        sender = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'
        recipient = '0x' + '22' * 20
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        process = subprocess.Popen(['anvil', '--host', '127.0.0.1', '--port', str(port),
                                    '--no-mining', '--silent'], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        self.addCleanup(lambda: (process.terminate(), process.wait(timeout=5)))
        url = f'http://127.0.0.1:{port}'

        def rpc(method, params):
            request = urllib.request.Request(url, json.dumps({'jsonrpc': '2.0', 'id': 1,
                                                              'method': method, 'params': params}).encode(),
                                             {'Content-Type': 'application/json'})
            with urllib.request.urlopen(request, timeout=3) as response:
                result = json.load(response)
            if 'error' in result:
                raise RuntimeError(result['error'])
            return result['result']

        for attempt in range(50):
            try:
                rpc('eth_chainId', [])
                break
            except OSError:
                if process.poll() is not None or attempt == 49:
                    self.fail('Anvil did not start')
                time.sleep(0.1)

        def sign(tx):
            return subprocess.check_output(['cast', 'mktx', tx['to'], tx['data'],
                                            '--chain', str(tx['chainId']), '--nonce', str(tx['nonce']),
                                            '--gas-limit', str(tx['gas']), '--gas-price', str(tx['gasPrice']),
                                            '--value', str(tx['value']), '--legacy', '--private-key', key],
                                           text=True).strip()

        signer = CastSigner(url, '/unused', '/unused')
        journal = {}
        persisted = []
        driver = TransactionDriver(journal, lambda state: persisted.append(json.loads(json.dumps(state))),
                                   sign, signer.decode, signer.hash_raw)
        tx_hash = driver.begin('l1', rpc, 31337, sender, recipient, '0x', 0, 21000,
                               2_000_000_000, kind='bridge')
        self.assertEqual(journal['transactions']['l1']['state'], 'submitted')
        self.assertEqual(rpc('eth_getTransactionByHash', [tx_hash])['hash'], tx_hash)
        raw = journal['transactions']['l1']['raw']
        self.assertEqual(persisted[0]['transactions']['l1']['raw'], raw)
        self.assertEqual(persisted[0]['transactions']['l1']['state'], 'signed')
        rpc('anvil_dropTransaction', [tx_hash])
        self.assertIsNone(rpc('eth_getTransactionByHash', [tx_hash]))
        self.assertEqual(rpc('eth_getTransactionCount', [sender, 'pending']), '0x0')
        self.assertTrue(driver.rebroadcast_if_missing('l1', rpc))
        self.assertEqual(journal['transactions']['l1']['raw'], raw)
        self.assertEqual(signer.hash_raw(raw), tx_hash)
        self.assertEqual(rpc('eth_getTransactionByHash', [tx_hash])['hash'], tx_hash)
        self.assertFalse(driver.rebroadcast_if_missing('l1', rpc))


if __name__ == '__main__':
    unittest.main()
