#!/usr/bin/env python3
"""Prepare a bootstrap package containing authenticated qualification duties."""

import argparse
import copy
import sys
import time
from pathlib import Path

import audit
import bootstrap_package
import dispatcher
import journal
import keeper
import proof_check
import service as s
import workflow_io as io
import wrapper
from runpod import Error as TransportError, Store


def snapshot(config, dispatch, evidence, payload):
    settings, lane = config['settings'], config['lane']
    s.require(settings['coordinator'] == s.ZERO_ADDRESS and config['policy']['expected_operator'] == settings['sequencer'],
              'qualification_requires_bootstrap_sequencer')
    s.require(dispatch.state['period'] == 0 and dispatch.state['enrollment']['phase'] == 'bootstrap'
              and dispatch.state['subscriptions'] and not dispatch.enrollment.empty_bootstrap,
              'qualification_requires_nonempty_bootstrap_journal')
    s.require(dispatch.enrollment.block_hash == config['enrollment']['block_hash']
              and lane in dispatch.state['lanes'] and dispatch.state['lanes'][lane]['settings'] == settings,
              'qualification_dispatch_identity_changed')
    statements = s.validate_evidence(settings, evidence)
    numbers = list(statements)
    s.require(len(numbers) >= 2, 'qualification_requires_two_batches')
    s.exact(payload, ('from_batch_number', 'to_batch_number', 'vk_hash', 'fri_proofs'))
    s.require(payload['from_batch_number'] == numbers[0] and payload['to_batch_number'] == numbers[-1]
              and payload['vk_hash'] == settings['vk_hash'] and type(payload['fri_proofs']) is list
              and len(payload['fri_proofs']) == len(numbers), 'qualification_native_payload_changed')
    pending = dispatch.state['pending_pick']
    s.require(pending is None or dispatch.state['operations'][pending]['lane'] != lane,
              'qualification_pending_pick_requires_reconciliation')
    hashes = {number: s.keccak(s.decode_proof(proof)) for number, proof in zip(numbers, payload['fri_proofs'])}
    duties = []
    for operation, entry in journal.range_items(dispatch.state['operations'], lane, numbers[0], numbers[-1]):
        s.require(entry['status'] not in audit.LIVE, 'qualification_duty_requires_reconciliation')
        if entry['status'] != 'accepted':
            continue
        directory = dispatch.directory(operation)
        signed = io.private_json(directory / 'signed-proof.json')
        s.exact(signed, ('proof', 'signature'))
        authority = io.private_json(directory / 'authority.json')
        s.require(authority.get('status') == 'accepted', 'qualification_native_acceptance_missing')
        request = dispatch.proof_request(operation, signed['proof'])
        s.verify_eoa(request, signed['signature'])
        duty = io.private_json(directory / 'duty.json')
        s.require(duty == {**request['typed_data']['message'], 'operatorSignature': signed['signature']}
                  and duty['friProofHash'] == hashes[duty['batchNumber']], 'qualification_duty_changed')
        s.require(dispatch.recover_acceptance(operation, signed['proof']), 'qualification_native_proof_unavailable')
        if duty['slot'] < settings['duties_per_round']:
            duties.append(duty)
    s.require(duties, 'qualification_duties_required')
    return {'manifest_payload': dispatch.range_manifest_payload(lane, numbers[0], numbers[-1]),
            'subscriptions': copy.deepcopy(dispatch.state['subscriptions']),
            'duties': sorted(duties, key=lambda value: value['batchNumber'])}


def validate_audit(config, dispatch, registry_rpc, evidence, payload, request, bundle, trust, event_root):
    retained = snapshot(config, dispatch, evidence, payload)
    s.require(request['manifest']['payload'] == retained['manifest_payload']
              and request['subscriptions'] == retained['subscriptions'] and request['duties'] == retained['duties'],
              'qualification_journal_range_changed')
    s.require(bundle['journal_id'] == dispatch.state['journal_id'], 'qualification_audit_journal_changed')
    s.require(not s.round_closed(config['settings'], registry_rpc, 0), 'bootstrap_round_closed')
    return audit.verify_package(config['settings'], evidence, request['manifest'], request['subscriptions'],
        request['duties'], bundle, trust, registry_rpc, fri_payload=payload, event_root=event_root)


def request(config, dispatch, registry_rpc, settlement_rpc, evidence, payload, beneficiary,
            signer, signature_dir, trust, event_root, now):
    keeper.configuration(config)
    retained = snapshot(config, dispatch, evidence, payload)
    settings = config['settings']
    s.nonzero(beneficiary, 20)
    manifest_request = s.manifest_request(settings, retained['manifest_payload'])
    manifest_signature = io.typed_signature(signature_dir, manifest_request, signer, True)
    manifest = {'payload': retained['manifest_payload'], 'sequencer_signature': manifest_signature}
    unsigned_audit = dispatch.audit_payload(config['lane'], payload['from_batch_number'], payload['to_batch_number'],
                                             event_root=event_root)
    audit_signature = io.typed_signature(signature_dir, audit.checkpoint_request(unsigned_audit), signer, True)
    bundle = {**unsigned_audit, 'sequencer_signature': audit_signature}
    chain = keeper.status(config, settlement_rpc, now)
    s.require(chain['mode'] == 'bootstrap' and not chain['package_open'], 'bootstrap_package_already_open')
    accepted = {'domainVersion': 1, 'protocolVersion': 32, 'chainId': settings['execution_chain_id'],
                'chainAddress': settings['chain_address'], 'parent': chain['parent'],
                'batchFrom': payload['from_batch_number'], 'batchTo': payload['to_batch_number'], 'period': 0,
                'rosterRoot': s.ZERO, 'policyHash': settings['policy_hash'], 'vkHash': settings['vk_hash'],
                'turn': 0, 'sequencer': settings['sequencer'], 'sequencerBeneficiary': beneficiary,
                'wrapper': s.ZERO_ADDRESS, 'wrapperBeneficiary': s.ZERO_ADDRESS}
    value = {'proposal': {'mode': 'bootstrap', 'accepted_package': accepted, 'candidate': None, 'candidate_proof': []},
             'manifest': manifest, 'subscriptions': retained['subscriptions'], 'duties': retained['duties']}
    checked = validate_audit(config, dispatch, registry_rpc, evidence, payload, value, bundle, trust, event_root)
    call = keeper.package_call(config, settlement_rpc, value, evidence, payload, now, 'open',
                               enrollment=dispatch.enrollment)
    return {'schema_version': 1, 'request': value, 'audit': bundle, 'audit_check': checked, 'open_call': call}


def verify(config, dispatch, registry_rpc, settlement_rpc, evidence, payload, proof, request_value,
           bundle, trust, release, verifier, check_dir, lease_dir, event_root, now):
    started = time.monotonic()
    keeper.configuration(config)
    s.require(request_value['proposal']['mode'] == 'bootstrap', 'qualification_requires_bootstrap_package')
    checked = validate_audit(config, dispatch, registry_rpc, evidence, payload, request_value, bundle, trust, event_root)
    computed = keeper.prepare(config['settings'], request_value, evidence, payload, enrollment=dispatch.enrollment)
    keeper.inspect(config, settlement_rpc, computed, evidence, now, 1, enrollment=dispatch.enrollment)
    bootstrap_package.retained_lease(lease_dir, payload, proof, release)
    identity = wrapper.job.release_identity(release)
    s.require(identity['stage'] == 'SNARK' and identity['vk_hash'] == config['settings']['vk_hash'], 'wrong_snark_release')
    s.exact(verifier, ('executable', 'sha256', 'timeout_seconds'))
    s.require(Path(verifier['executable']).is_absolute() and isinstance(verifier['sha256'], str)
              and len(verifier['sha256']) == 64 and all(c in '0123456789abcdef' for c in verifier['sha256'])
              and 0 < s.uint(verifier['timeout_seconds'], 32) <= 1800, 'invalid_fri_verifier_pin')
    fri_check = wrapper.verify_fris(Path(check_dir), verifier, identity, config['settings'], evidence, payload)
    snark_check = proof_check.verify_snark(config, settlement_rpc, evidence, payload, proof,
                                           now + int(time.monotonic() - started))
    keeper.inspect(config, settlement_rpc, computed, evidence, now + int(time.monotonic() - started),
                   1, enrollment=dispatch.enrollment)
    package = s.prepare_package(config['settings'], evidence, request_value['manifest'], request_value['subscriptions'],
        request_value['duties'], request_value['proposal'], proof, payload, enrollment=dispatch.enrollment)
    return {'schema_version': 1, 'audit_check': checked, 'fri_check': fri_check,
            'snark_check': snark_check, 'package': package}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('keeper-config', 'dispatcher-dir', 'native-auth-file', 'trust', 'evidence', 'payload',
                 'audit-events-dir', 'output'):
        parser.add_argument('--' + name, required=True)
    sub = parser.add_subparsers(dest='action', required=True)
    create = sub.add_parser('request')
    create.add_argument('--beneficiary', required=True)
    create.add_argument('--sequencer-wallet-file', required=True)
    create.add_argument('--signature-dir', required=True)
    check = sub.add_parser('verify')
    for name in ('request', 'audit', 'proof', 'release', 'fri-verifier', 'check-dir', 'lease-dir'):
        check.add_argument('--' + name, required=True)
    args = parser.parse_args()
    config = keeper.configuration(io.private_json(args.keeper_config))
    registry_rpc, settlement_rpc = keeper.registry_rpc_for(config), keeper.rpc_for(config)
    evidence, payload = io.private_json(args.evidence), io.private_json(args.payload, io.MAX_PAYLOAD)
    trust = io.private_json(args.trust)
    event_root = io.directory(args.audit_events_dir)
    store = Store(io.directory(args.dispatcher_dir))
    with store.lock('dispatcher.lock'):
        dispatch = dispatcher.Dispatcher(store, registry_rpc=registry_rpc)
        dispatch.network = wrapper.job.NativeNetwork()
        dispatch.auth = {config['lane']: wrapper.sentry.authorization(args.native_auth_file)}
        if args.action == 'request':
            result = request(config, dispatch, registry_rpc, settlement_rpc, evidence, payload, args.beneficiary,
                io.wallet_connection(args.sequencer_wallet_file, config['policy']['rpc_timeout_seconds']),
                io.directory(args.signature_dir), trust, event_root, int(time.time()))
        else:
            release = wrapper.job.read_file(args.release, wrapper.job.MAX_MANIFEST, private=True)
            result = verify(config, dispatch, registry_rpc, settlement_rpc, evidence, payload, io.private_json(args.proof),
                io.private_json(args.request), io.private_json(args.audit), trust, release, io.private_json(args.fri_verifier),
                io.directory(args.check_dir), args.lease_dir, event_root, int(time.time()))
        s.write_new(args.output, result)
    print('wrote qualification artifact; no transaction was sent', file=sys.stderr)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransportError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransportError)) else 'qualification_package_failed', file=sys.stderr)
        sys.exit(1)
