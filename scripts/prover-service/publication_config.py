#!/usr/bin/env python3
"""Preview one node's service publication overlay using reviewed deployment pins."""

import argparse
import json
from pathlib import Path
import re
import sys
import time

import keeper as k
import relay as r
import service as s
import workflow_io as io


def local_vk_hash():
    source = Path(__file__).resolve().parents[2] / 'lib/types/src/protocol/proving_version.rs'
    text = source.read_text(encoding='utf-8')
    matches = re.findall(r'const\s+V8_VK_HASH\s*:\s*&\s*\x27static\s+str\s*=\s*"(0x[0-9a-f]{64})"', text)
    s.require(len(matches) == 1, 'cannot_identify_source_v8_vk')
    return matches[0]


def preview(config, rpc, now, lane, execution_id, settlement_id, directory, vk_hash):
    k.configuration(config)
    settings, pins = config['settings'], config['policy']
    s.require(config['lane'] == lane, 'wrong_keeper_lane')
    execution = s.uint(settings['execution_chain_id'])
    settlement = s.uint(settings['settlement_chain_id'])
    registry = s.uint(settings['registry_chain_id'])
    s.require(execution == execution_id and settlement == settlement_id and execution != settlement,
              'wrong_execution_or_settlement_identity')
    s.require((lane == 'child' and execution == registry) or
              (lane == 'gateway' and execution != registry), 'wrong_lane_topology')
    s.require(settings['vk_hash'] == vk_hash, 'node_source_vk_mismatch')
    io.directory(directory)
    status = k.status(config, rpc, now)
    coordinator = settings['coordinator']
    s.require((status['mode'] == 'bootstrap') == (coordinator == s.ZERO_ADDRESS), 'phase_pin_mismatch')

    # Reuse the canonical head from keeper.status, then recheck that head after
    # the verifier, priority guard and prover authority reads.
    head = status['head']
    anchor = {'blockHash': head['hash'], 'requireCanonical': True}
    gate, chain = settings['proof_gate'], settings['chain_address']
    call = lambda target, signature, encoded=b'', kind='uint': r.call_word(rpc, target, signature, anchor, encoded, kind)
    guard = call(gate, 'priorityGuard()', kind='address')
    k.check_code(rpc, guard, pins['priority_guard_code_hash'], anchor, 'priority_guard_code_changed')
    s.require(call(guard, 'acceptanceGate()', kind='address') == gate
              and call(guard, 'chain()', kind='address') == chain
              and call(guard, 'policyHash()', kind='bytes32') == settings['policy_hash'], 'priority_guard_changed')
    verifier = call(gate, 'productionVerifier()', kind='address')
    plonk = call(gate, 'plonkVerifier()', kind='address')
    s.require(call(chain, 'getVerifier()', kind='address') == verifier
              and call(verifier, 'plonkVerifiers(uint32)', s.word(8), 'address') == plonk
              and not call(verifier, 'IS_TESTNET_VERIFIER()', kind='bool')
              and call(verifier, 'verificationKeyHash(uint256)', s.word(0x802), 'bytes32') == vk_hash,
              'native_verifier_changed')
    k.check_code(rpc, verifier, call(gate, 'verifierCodeHash()', kind='bytes32'), anchor, 'verifier_code_changed')
    k.check_code(rpc, plonk, call(gate, 'plonkVerifierCodeHash()', kind='bytes32'), anchor, 'plonk_code_changed')
    timelock = call(gate, 'timelock()', kind='address')
    role = s.raw_hex(s.keccak(b'PROVER_ROLE'))
    chain_word = bytes(12) + s.raw_hex(chain, 20)
    s.require(call(chain, 'isValidator(address)', bytes(12) + s.raw_hex(timelock, 20), 'bool')
              and call(timelock, 'getRoleMemberCount(address,bytes32)', chain_word + role) == 1
              and call(timelock, 'getRoleMember(address,bytes32,uint256)', chain_word + role + s.word(0), 'address') == gate,
              'native_prover_authority_changed')
    r.check_anchor(rpc, settings, head)
    return {'service_publication': {'enabled': True, 'directory': str(Path(directory)),
            'gate': gate, 'gate_code_hash': pins['gate_code_hash'], 'coordinator': coordinator,
            'coordinator_code_hash': pins['coordinator_code_hash'], 'policy_hash': settings['policy_hash'],
            'production_vk_hash': vk_hash, 'sequencer': settings['sequencer'], 'poll_interval': '2s',
            'rpc_timeout': '30s', 'max_records': 256}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--keeper-config', required=True)
    parser.add_argument('--lane', choices=('child', 'gateway'), required=True)
    parser.add_argument('--execution-chain-id', type=int, required=True)
    parser.add_argument('--settlement-chain-id', type=int, required=True)
    parser.add_argument('--directory', required=True)
    args = parser.parse_args()
    config = io.private_json(args.keeper_config)
    overlay = preview(config, k.rpc_for(config), int(time.time()), args.lane,
                      args.execution_chain_id, args.settlement_chain_id, args.directory, local_vk_hash())
    print(json.dumps(overlay, indent=2))
    print('Preview only: bootstrap, enrollment, proofs, transport and node rollout remain separate.', file=sys.stderr)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, OSError, ValueError, KeyError, TypeError) as error:
        print(str(error) if isinstance(error, s.Error) else 'publication_config_failed', file=sys.stderr)
        sys.exit(1)
