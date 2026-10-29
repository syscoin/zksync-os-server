#!/usr/bin/env python3
"""Build and verify an uncredited, pre-enrollment bootstrap proof package."""

import argparse
import sys
import time
from pathlib import Path

import bootstrap_control
import keeper
import proof_check
import service as s
import workflow_io as io
import wrapper


def request(config, registry_rpc, settlement_rpc, manifest, beneficiary, evidence, payload, now):
    settings = config['settings']
    s.exact(manifest, ('payload', 'sequencer_signature'))
    period = s.uint(manifest['payload']['period'], 64)
    prepared = bootstrap_control.prepare(config, registry_rpc, settlement_rpc, period,
                                         manifest['payload']['previous_cursor'], now)
    s.require(manifest['payload'] == prepared['payload'], 'bootstrap_manifest_changed')
    s.verify_eoa(prepared['request'], manifest['sequencer_signature'])
    s.nonzero(beneficiary, 20)
    chain = keeper.status(config, settlement_rpc, now)
    s.require(chain['mode'] == 'bootstrap' and not chain['package_open'], 'bootstrap_package_already_open')
    accepted = {'domainVersion': 1, 'protocolVersion': 32, 'chainId': settings['execution_chain_id'],
                'chainAddress': settings['chain_address'], 'parent': chain['parent'],
                'batchFrom': payload['from_batch_number'], 'batchTo': payload['to_batch_number'],
                'period': period, 'rosterRoot': s.ZERO, 'policyHash': settings['policy_hash'],
                'vkHash': settings['vk_hash'], 'turn': 0, 'sequencer': settings['sequencer'],
                'sequencerBeneficiary': beneficiary, 'wrapper': s.ZERO_ADDRESS,
                'wrapperBeneficiary': s.ZERO_ADDRESS}
    result = {'proposal': {'mode': 'bootstrap', 'accepted_package': accepted, 'candidate': None,
                           'candidate_proof': []}, 'manifest': manifest, 'subscriptions': [], 'duties': []}
    authority = s.EnrollmentAuthority(settings, [], period, registry_rpc, config['enrollment']['block_hash'])
    call = keeper.package_call(config, settlement_rpc, result, evidence, payload, now, 'open', enrollment=authority)
    return {'schema_version': 1, 'request': result, 'open_call': call}


def retained_lease(path, payload, proof, release):
    path = wrapper.sentry.private_directory(path)
    authority = wrapper.read_private_json(path / 'authority.json')
    s.require(authority.get('stage') == 'SNARK' and authority.get('status') == 'accepted',
              'accepted_native_lease_required')
    s.require(authority.get('release_sha256') == wrapper.job.hash_bytes(release)
              and wrapper.job.read_file(path / 'release.json', wrapper.job.MAX_MANIFEST, private=True) == release,
              'retained_release_changed')
    payload_raw = wrapper.job.read_file(path / 'payload.json', wrapper.job.MAX_PICK['SNARK'], private=True)
    s.require(wrapper.job.decode(payload_raw) == payload and authority.get('bounds') == {
        'from_batch_number': payload['from_batch_number'], 'to_batch_number': payload['to_batch_number']},
        'retained_payload_changed')
    submission = wrapper.job.read_file(path / 'submission.json', wrapper.job.MAX_SUBMIT, private=True)
    s.require(wrapper.job.hash_bytes(submission) == authority.get('submission_sha256')
              and wrapper.job.decode(submission) == {**proof, 'lease_token': authority.get('lease_token')},
              'retained_submission_changed')


def verify(config, registry_rpc, settlement_rpc, request_value, evidence, payload, proof,
           release, verifier, check_dir, lease_dir, now):
    started = time.monotonic()
    settings = config['settings']
    keeper.configuration(config)
    s.require(settings['coordinator'] == s.ZERO_ADDRESS and config['policy']['expected_operator'] == settings['sequencer'],
              'bootstrap_requires_sequencer_operator')
    s.exact(request_value, ('proposal', 'manifest', 'subscriptions', 'duties'))
    s.require(request_value['proposal']['mode'] == 'bootstrap' and request_value['subscriptions'] == []
              and request_value['duties'] == [], 'empty_bootstrap_control_only')
    period = s.uint(request_value['manifest']['payload']['period'], 64)
    authority = s.EnrollmentAuthority(settings, [], period, registry_rpc, config['enrollment']['block_hash'])
    s.require(authority.empty_bootstrap, 'empty_bootstrap_authority_required')
    computed = keeper.prepare(settings, request_value, evidence, payload, enrollment=authority)
    keeper.inspect(config, settlement_rpc, computed, evidence, now, 1, enrollment=authority)
    retained_lease(lease_dir, payload, proof, release)
    identity = wrapper.job.release_identity(release)
    s.require(identity['stage'] == 'SNARK' and identity['vk_hash'] == settings['vk_hash'], 'wrong_snark_release')
    s.exact(verifier, ('executable', 'sha256', 'timeout_seconds'))
    s.require(Path(verifier['executable']).is_absolute() and isinstance(verifier['sha256'], str)
              and len(verifier['sha256']) == 64 and all(c in '0123456789abcdef' for c in verifier['sha256'])
              and 0 < s.uint(verifier['timeout_seconds'], 32) <= 1800, 'invalid_fri_verifier_pin')
    fri_check = wrapper.verify_fris(Path(check_dir), verifier, identity, settings, evidence, payload)
    current = now + int(time.monotonic() - started)
    snark_check = proof_check.verify_snark(config, settlement_rpc, evidence, payload, proof, current)
    current = now + int(time.monotonic() - started)
    keeper.inspect(config, settlement_rpc, computed, evidence, current, 1, enrollment=authority)
    package = s.prepare_package(settings, evidence, request_value['manifest'], [], [], request_value['proposal'],
                                proof, payload, enrollment=authority)
    return {'schema_version': 1, 'fri_check': fri_check, 'snark_check': snark_check, 'package': package}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--keeper-config', required=True)
    parser.add_argument('--evidence', required=True)
    parser.add_argument('--payload', required=True)
    parser.add_argument('--output', required=True)
    sub = parser.add_subparsers(dest='action', required=True)
    build = sub.add_parser('request')
    build.add_argument('--manifest', required=True)
    build.add_argument('--beneficiary', required=True)
    check = sub.add_parser('verify')
    check.add_argument('--request', required=True)
    check.add_argument('--proof', required=True)
    check.add_argument('--release', required=True)
    check.add_argument('--fri-verifier', required=True, help='private JSON containing executable, sha256, timeout_seconds')
    check.add_argument('--check-dir', required=True)
    check.add_argument('--lease-dir', required=True)
    args = parser.parse_args()
    config = io.private_json(args.keeper_config)
    evidence = io.private_json(args.evidence)
    payload = io.private_json(args.payload, io.MAX_PAYLOAD)
    registry_rpc, settlement_rpc = keeper.registry_rpc_for(config), keeper.rpc_for(config)
    if args.action == 'request':
        result = request(config, registry_rpc, settlement_rpc, io.private_json(args.manifest), args.beneficiary,
                         evidence, payload, int(time.time()))
    else:
        release = wrapper.job.read_file(args.release, wrapper.job.MAX_MANIFEST, private=True)
        result = verify(config, registry_rpc, settlement_rpc, io.private_json(args.request), evidence, payload,
                        io.private_json(args.proof), release, io.private_json(args.fri_verifier),
                        io.directory(args.check_dir), args.lease_dir, int(time.time()))
    s.write_new(args.output, result)
    print('prepared private bootstrap artifact; no transaction was sent', file=sys.stderr)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, s.Error) else 'bootstrap_package_failed', file=sys.stderr)
        sys.exit(1)
