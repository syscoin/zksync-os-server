#!/usr/bin/env python3
"""Drive one enrolled bootstrap operator's native FRI duties through an existing pool."""

import argparse
import json
from pathlib import Path
import sys
import time

import dispatcher
import enrollment
import journal
import service as s
import workflow_io as io

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'prover-rental'))
import job
import pool
import sentry
from runpod import Error as TransportError, Store


def configuration(value):
    s.exact(value, ('schema_version', 'dispatcher_dir', 'pool_dir', 'signature_dir', 'registry_rpc_file',
                    'sequencer_wallet_file', 'operator_wallet_file', 'child_auth_file', 'gateway_auth_file',
                    'account', 'readiness_seconds'))
    s.require(value['schema_version'] == 1, 'unsupported_qualification_configuration')
    for key in ('dispatcher_dir', 'pool_dir', 'signature_dir'):
        io.directory(value[key])
    for key in ('registry_rpc_file', 'sequencer_wallet_file', 'operator_wallet_file', 'child_auth_file'):
        s.require(type(value[key]) is str and Path(value[key]).is_absolute(), 'absolute_qualification_file_required')
    s.require(value['gateway_auth_file'] is None or type(value['gateway_auth_file']) is str
              and Path(value['gateway_auth_file']).is_absolute(), 'absolute_qualification_file_required')
    s.nonzero(value['account'], 20)
    s.require(0 < s.uint(value['readiness_seconds'], 32) <= 86400, 'invalid_readiness_window')
    return value


def current_bootstrap(dispatch, rpc):
    s.require(dispatch.state['period'] == 0 and dispatch.state['enrollment']['phase'] == 'bootstrap',
              'bootstrap_journal_required')
    normalized, anchor = enrollment.enrollment_snapshot(dispatch.settings, dispatch.state['subscriptions'], 0, rpc)
    s.require(anchor['phase'] == 'bootstrap' and normalized == dispatch.state['subscriptions'],
              'bootstrap_enrollment_changed')
    s.require(len(normalized) == 1, 'single_bootstrap_operator_required')
    s.require(not s.round_closed(dispatch.settings, rpc, 0), 'bootstrap_round_closed')


def matching_pool(dispatch, worker_pool, operation):
    op = dispatch.state['operations'][operation]
    lane = dispatch.state['lanes'][op['lane']]
    target = worker_pool.settings['lanes'][op['lane']]
    s.require(target['endpoint'] == lane['endpoint'] and target['stages']['FRI']['acquisition'] == 'external',
              'qualification_pool_lane_changed')
    for key, other in (('execution_chain_id', 'chain_id'), ('chain_address', 'chain_address'),
                       ('settlement_chain_id', 'settlement_chain_id'), ('vk_hash', 'vk_hash')):
        s.require(lane['settings'][key] == target['identity'][other], 'qualification_pool_lane_changed')
    events = [event for event in dispatch.state['audit']['events'] if event['kind'] == 'pick_started'
              and event['payload']['operation'] == operation]
    s.require(len(events) == 1, 'qualification_pick_time_missing')
    deadline = min(events[0]['at'] + target['native_lease_seconds'],
                   op['request']['request']['typed_data']['message']['expiresAt'])
    s.require(deadline > events[0]['at'], 'invalid_qualification_deadline')
    identity = 'qualify-' + dispatch.state['journal_id'][2:] + '-' + operation
    directory = dispatch.directory(operation)
    payload_raw = job.read_file(directory / 'payload.json', job.MAX_PICK['FRI'], private=True)
    evidence = io.private_json(directory / 'evidence.json')
    binding, _ = worker_pool.bind_evidence(op['lane'], job.encode(job.decode(payload_raw)), evidence,
        op['assignment']['batch_number'], op['assignment']['batch_number'])
    matches = [(key, item) for key, item in worker_pool.state['operations'].items() if item['job_id'] == identity]
    s.require(len(matches) <= 1, 'duplicate_qualification_pool_job')
    if matches:
        key, item = matches[0]
        s.require(item['mode'] == 'external' and item['lane'] == op['lane'] and item['stage'] == 'FRI'
                  and item['deadline'] == deadline and item['chain_binding'] == binding,
                  'qualification_pool_job_changed')
        if item['status'] == 'import_pending':
            s.require(worker_pool.clock() < deadline, 'qualification_lease_expired_before_enqueue')
            return worker_pool.enqueue(op['lane'], 'FRI', identity, payload_raw, evidence, deadline)
        worker_pool.bound_authority(key)
        return key
    s.require(worker_pool.clock() < deadline, 'qualification_lease_expired_before_enqueue')
    return worker_pool.enqueue(op['lane'], 'FRI', identity, payload_raw, evidence, deadline)


def step(dispatch, worker_pool, registry_rpc, signer, operator_signer, signatures, account, readiness_seconds, now):
    current_bootstrap(dispatch, registry_rpc)
    s.require(account == dispatch.state['subscriptions'][0]['subscription']['account'], 'bootstrap_account_changed')
    pending = dispatch.state['pending_pick']
    if pending is not None:
        s.require((dispatch.directory(pending) / 'picked-wire.json').exists(), 'native_pick_uncertain_preserve_journal')
        return {'next_action': 'recovered_pick', 'operation': dispatch.recover_pick()}
    active = [(key, op) for key, op in dispatch.state['operations'].items()
              if op['status'] in ('picked', 'offered', 'submission_pending')]
    s.require(not any(op['status'] == 'pick_pending' for op in dispatch.state['operations'].values()),
              'untracked_pending_pick')
    s.require(len(active) <= 1 and all(op['account'] == account for _, op in active),
              'reconcile_existing_dispatch_operations')
    if not active:
        account_state = dispatch.state['accounts'][account]
        if account_state['offered'] >= dispatch.settings['duties_per_round']:
            return {'next_action': 'await_bootstrap_package_or_investigate_unfilled_quota',
                    'dispatch_report': dispatch.report()}
        ready = account_state['ready']
        if ready is None or ready['request']['typed_data']['message']['expiresAt'] <= now:
            expiry = min(now + readiness_seconds, dispatch.state['enrollment']['ends_at'])
            request = dispatch.request(account, expiry)
            signature = io.typed_signature(signatures, request, operator_signer, True)
            dispatch.ready(request, signature)
        result = dispatch.pick()
        return {'next_action': 'native_pick', 'result': result}
    operation, op = active[0]
    if op['status'] == 'submission_pending':
        saved = io.private_json(dispatch.directory(operation) / 'signed-proof.json')
        s.exact(saved, ('proof', 'signature'))
        return {'next_action': 'native_submission', 'operation': operation,
                'result': dispatch.submit(operation, saved['proof'], saved['signature'])}
    request = dispatch.offer_request(operation)
    offer_signature = io.typed_signature(signatures, request, signer, True)
    dispatch.authorize(operation, offer_signature)
    pool_operation = matching_pool(dispatch, worker_pool, operation)
    pool_item = worker_pool.operation(pool_operation)
    if pool_item['status'] != 'returned':
        s.require(pool_item['status'] not in pool.DONE, 'qualification_pool_job_failed')
        return {'next_action': 'await_pool_result', 'operation': operation, 'pool_operation': pool_operation,
                'pool_status': pool_item['status']}
    root = worker_pool.directory(pool_operation)
    controller = (Store(Path(pool_item['warm_controller_dir'])) if pool_item.get('warm_owner')
                  else worker_pool.controller_store(pool_item))
    sentry.verify_input_result(root, controller, pool_item['rental_operation'], root / 'returned-proof.json')
    proof = io.private_json(root / 'returned-proof.json', job.MAX_SUBMIT)
    duty_request = dispatch.proof_request(operation, proof)
    signature = io.typed_signature(signatures, duty_request, operator_signer, True)
    result = dispatch.submit(operation, proof, signature)
    return {'next_action': 'native_submission', 'operation': operation, 'result': result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--execute', action='store_true')
    args = parser.parse_args()
    value = configuration(io.private_json(args.config))
    if not args.execute:
        print(json.dumps({'next_action': 'add_execute_to_run_one_step', 'service_ready_verified': False}))
        return
    registry_rpc = io.wallet_connection(value['registry_rpc_file'], 15)
    signer = io.wallet_connection(value['sequencer_wallet_file'], 15)
    operator = io.wallet_connection(value['operator_wallet_file'], 15)
    native = job.NativeNetwork()
    dispatcher_store, pool_store = Store(value['dispatcher_dir']), Store(value['pool_dir'])
    with dispatcher_store.lock('dispatcher.lock'), pool_store.lock('pool.lock'):
        auth = {'child': sentry.authorization(value['child_auth_file'])}
        state = journal.load(Path(value['dispatcher_dir']) / 'state.json', list_fields=('audit.events',))
        if 'gateway' in state['lanes']:
            s.require(value['gateway_auth_file'] is not None, 'gateway_authorization_required')
            auth['gateway'] = sentry.authorization(value['gateway_auth_file'])
        dispatch = dispatcher.Dispatcher(dispatcher_store, native, auth, registry_rpc=registry_rpc)
        worker_pool = pool.Pool(pool_store)
        result = step(dispatch, worker_pool, registry_rpc, signer, operator, value['signature_dir'],
                      value['account'], value['readiness_seconds'], dispatch.now)
    print(json.dumps({**result, 'service_ready_verified': False, 'onchain_qualification_verified': False}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (s.Error, TransportError, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, (s.Error, TransportError)) else 'qualification_step_failed', file=sys.stderr)
        sys.exit(1)
