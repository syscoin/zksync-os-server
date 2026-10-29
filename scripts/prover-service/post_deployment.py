#!/usr/bin/env python3
"""Run individual post-deployment bootstrap steps using the existing durable helpers."""

import argparse
import json
import sys
import time
from pathlib import Path

import bootstrap_control
import bootstrap_enrollment as enrollment
import bootstrap_package
import keeper
import publish_initial_roster as roster
import relay
import service as s
import workflow_io as io
from zksys_keeper_transactions import TransactionError

NOT_ORCHESTRATED = ('proof_generation', 'qualifying_duties', 'prover_role_cutover',
                    'message_delivery', 'service_activation')


def configuration(value):
    s.exact(value, ('schema_version', 'bootstrap_keeper_config', 'enrollment_config', 'roster_config',
                    'enrollment_state_dir', 'roster_state_dir', 'fri_check_dir'))
    s.require(value['schema_version'] == 1, 'unsupported_setup_configuration')
    for key in value.keys() - {'schema_version'}:
        s.require(type(value[key]) is str and Path(value[key]).is_absolute(), 'absolute_setup_path_required')
    s.require(value['enrollment_state_dir'] != value['roster_state_dir'], 'separate_setup_journals_required')
    return value


def load_configs(value):
    config = configuration(value)
    keeper_config = keeper.configuration(io.private_json(config['bootstrap_keeper_config']))
    enroll_config = enrollment.configuration(io.private_json(config['enrollment_config']))
    roster_config = roster.validate_config(io.private_json(config['roster_config']))
    settings = keeper_config['settings']
    s.require(settings == enroll_config['settings'], 'setup_keeper_enrollment_identity_changed')
    s.require(settings['coordinator'] == s.ZERO_ADDRESS
              and keeper_config['policy']['expected_operator'] == settings['sequencer'],
              'setup_requires_bootstrap_keeper')
    s.require(roster_config['registry'] == settings['registry']
              and roster_config['chain_id'] == s.uint(settings['registry_chain_id'])
              and roster_config['genesis_hash'] == enroll_config['genesis_hash']
              and roster_config['registry_code_hash'] == enroll_config['registry_code_hash'],
              'setup_registry_identity_changed')
    return keeper_config, enroll_config, roster_config


def finalized_roster(rpc, config, artifact, result):
    if result['status'] != 'published':
        return result
    block, anchor = roster.read_snapshot(rpc, config, 'finalized')
    state = roster.registry_state(rpc, config, anchor)
    s.require(state['root'] in (s.ZERO, artifact['root']), 'finalized_roster_conflict')
    if state['root'] == artifact['root']:
        s.require(state['count'] == artifact['count'], 'finalized_roster_count_changed')
    roster.check_anchor(rpc, config, block)
    if state['root'] == s.ZERO:
        return {'status': 'awaiting_finalized_roster', 'root': artifact['root'], 'anchor': block['hash']}
    return {'status': 'published', 'root': artifact['root'], 'finalized_block_hash': block['hash']}


def report(action, result):
    return {'schema_version': 1, 'step': action, 'result': result, 'service_ready_verified': False,
            'not_orchestrated': list(NOT_ORCHESTRATED)}


def perform(value, action, inputs, execute=False, now=None):
    s.require(type(execute) is bool, 'invalid_execute_flag')
    s.require(not execute or action in ('enroll', 'publish-roster'), 'execute_not_supported_for_step')
    now = int(time.time()) if now is None else now
    keeper_config, enroll_config, roster_config = load_configs(value)
    if action in ('manifest', 'proof-request', 'proof-verify'):
        registry_rpc, settlement_rpc = keeper.registry_rpc_for(keeper_config), keeper.rpc_for(keeper_config)
        if action == 'manifest':
            s.exact(inputs, ('previous_cursor',))
            return bootstrap_control.prepare(keeper_config, registry_rpc, settlement_rpc, 0,
                                             inputs['previous_cursor'], now)
        evidence = io.private_json(inputs['evidence'])
        payload = io.private_json(inputs['payload'], io.MAX_PAYLOAD)
        if action == 'proof-request':
            s.exact(inputs, ('evidence', 'payload', 'manifest', 'beneficiary'))
            return bootstrap_package.request(keeper_config, registry_rpc, settlement_rpc,
                io.private_json(inputs['manifest']), inputs['beneficiary'], evidence, payload, now)
        s.exact(inputs, ('evidence', 'payload', 'request', 'proof', 'release', 'fri_verifier', 'lease_dir'))
        release = bootstrap_package.wrapper.job.read_file(inputs['release'],
                    bootstrap_package.wrapper.job.MAX_MANIFEST, private=True)
        return bootstrap_package.verify(keeper_config, registry_rpc, settlement_rpc, io.private_json(inputs['request']),
            evidence, payload, io.private_json(inputs['proof']), release, io.private_json(inputs['fri_verifier']),
            io.directory(value['fri_check_dir']), inputs['lease_dir'], now)
    if action == 'enroll':
        s.exact(inputs, ('subscription', 'operator_signature'))
        signed = io.private_json(inputs['subscription'])
        signature = s.read(Path(inputs['operator_signature']), private=True).decode().strip()
        rpc = relay.Rpc(enroll_config['rpc_url'], timeout=enroll_config['rpc_timeout_seconds'])
        if execute:
            return enrollment.run_send(rpc, enroll_config, signed, signature, value['enrollment_state_dir'], now)
        return enrollment.prepare(rpc, enroll_config, signed, signature, now)
    if action == 'publish-roster':
        s.exact(inputs, ('roster',))
        artifact = roster.validated_artifact(io.private_json(inputs['roster']))
        rpc = relay.Rpc(roster_config['rpc_url'], timeout=roster_config['rpc_timeout_seconds'])
        if execute:
            result = roster.run_send(rpc, roster_config, artifact, value['roster_state_dir'], now)
        else:
            result = roster.plan_page(rpc, roster_config, artifact, now)
        return finalized_roster(rpc, roster_config, artifact, result)
    raise s.Error('unknown_setup_step')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, help='private post-deployment setup configuration')
    sub = parser.add_subparsers(dest='action', required=True)
    manifest = sub.add_parser('manifest')
    manifest.add_argument('--previous-cursor', required=True)
    proof_request = sub.add_parser('proof-request')
    for key in ('evidence', 'payload', 'manifest', 'beneficiary'):
        proof_request.add_argument('--' + key, required=True)
    proof_verify = sub.add_parser('proof-verify')
    for key in ('evidence', 'payload', 'request', 'proof', 'release', 'fri-verifier', 'lease-dir'):
        proof_verify.add_argument('--' + key, required=True)
    enroll = sub.add_parser('enroll')
    enroll.add_argument('--subscription', required=True)
    enroll.add_argument('--operator-signature', required=True)
    publish = sub.add_parser('publish-roster')
    publish.add_argument('--roster', required=True)
    for command in (manifest, proof_request, proof_verify):
        command.add_argument('--output', required=True)
    for command in (enroll, publish):
        command.add_argument('--execute', action='store_true')
    args = vars(parser.parse_args())
    value = io.private_json(args.pop('config'))
    action = args.pop('action')
    output = args.pop('output', None)
    execute = args.pop('execute', False)
    result = report(action, perform(value, action, args, execute=execute))
    if output is not None:
        s.write_new(output, result)
        print('wrote private bootstrap artifact; no transaction was sent', file=sys.stderr)
    else:
        print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransactionError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransactionError)) else 'post_deployment_step_failed', file=sys.stderr)
        sys.exit(1)
