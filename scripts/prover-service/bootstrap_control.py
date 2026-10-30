#!/usr/bin/env python3
"""Prepare a sequencer signing request for uncredited pre-enrollment bootstrap work."""

import argparse
import json
import sys
import time

import keeper
import service as s
import workflow_io as io


def prepare(config, registry_rpc, settlement_rpc, period, previous_cursor, now):
    keeper.configuration(config)
    settings = config['settings']
    s.require(settings['coordinator'] == s.ZERO_ADDRESS, 'bootstrap_requires_zero_coordinator')
    s.require(config['policy']['expected_operator'] == settings['sequencer'], 'bootstrap_requires_sequencer_operator')
    s.nonzero(previous_cursor)
    authority = s.EnrollmentAuthority(settings, [], period, registry_rpc, config['enrollment']['block_hash'])
    s.require(authority.empty_bootstrap, 'empty_bootstrap_authority_required')
    status = keeper.status(config, settlement_rpc, now)
    s.require(status['mode'] == 'bootstrap', 'bootstrap_phase_closed')
    payload = {'schema_version': 1, 'chain_id': settings['execution_chain_id'],
               'chain_address': settings['chain_address'], 'sequencer': settings['sequencer'], 'period': period,
               'previous_cursor': previous_cursor, 'subscription_snapshot_hash': s.keccak(s.canonical([])),
               'assignments': [], 'retries': []}
    return {'schema_version': 1, 'enrollment_block_hash': authority.block_hash, 'payload': payload,
            'request': s.manifest_request(settings, payload), 'subscriptions': [], 'duties': []}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--keeper-config', required=True)
    parser.add_argument('--period', type=int, required=True)
    parser.add_argument('--previous-cursor', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    config = io.private_json(args.keeper_config)
    result = prepare(config, keeper.registry_rpc_for(config), keeper.rpc_for(config), args.period,
                     args.previous_cursor, int(time.time()))
    s.write_new(args.output, result)
    print('prepared empty bootstrap control manifest signing request; no transaction was sent', file=sys.stderr)


if __name__ == '__main__':
    try:
        main()
    except (s.Error, OSError, ValueError, TypeError, KeyError) as error:
        print(str(error) if isinstance(error, s.Error) else 'bootstrap_control_failed', file=sys.stderr)
        sys.exit(1)
