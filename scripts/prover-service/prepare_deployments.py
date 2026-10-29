#!/usr/bin/env python3
"""Generate private per-stage service deployment configs from an explicit deployment manifest.

Only computes deterministic addresses and constructor commitments. It does not contact a chain,
sign transactions, or establish that the service can start.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys

import deploy_component
import deploy_gate
import service

NAME = re.compile(r'[a-z][a-z0-9-]{0,63}\Z')


def require(value, reason):
    deploy_gate.require(value, reason)


def predicted_addresses(kind, cfg):
    sender, salt = cfg.get('sender'), cfg.get('factory_salt')
    require(isinstance(sender, str) and deploy_gate.ADDRESS.fullmatch(sender) and sender.lower() != deploy_gate.ZERO,
            'invalid stage sender')
    require(isinstance(salt, str) and deploy_gate.HASH.fullmatch(salt) and int(salt, 16), 'invalid stage salt')
    factory_contract = ('ZkSysServiceGateDeploymentFactoryV1' if kind == 'gate'
                        else 'ZkSysServiceComponentDeploymentFactoryV1')
    init = deploy_gate.bytecode(factory_contract) + deploy_gate.encode('constructor(address)', sender)
    factory = deploy_gate.cast('create2', '--deployer', deploy_gate.CREATE2, '--salt', salt, '--init-code', init).lower()
    require(deploy_gate.ADDRESS.fullmatch(factory), 'invalid predicted factory')
    def child(nonce):
        value = deploy_gate.cast('compute-address', factory, '--nonce', str(nonce)).split()[-1].lower()
        require(deploy_gate.ADDRESS.fullmatch(value), 'invalid predicted address')
        return value
    if kind == 'gate':
        return {'factory': factory, 'guard': child(1), 'gate': child(2)}
    return {'factory': factory, 'component': child(1)}


def replace_refs(value, predictions):
    if isinstance(value, dict):
        if set(value) == {'ref'}:
            ref = value['ref']
            require(isinstance(ref, str) and ref.count('.') == 1, 'invalid stage reference')
            name, field = ref.split('.')
            require(name in predictions and field in predictions[name], 'unknown stage reference')
            return predictions[name][field]
        return {key: replace_refs(item, predictions) for key, item in value.items()}
    if isinstance(value, list):
        return [replace_refs(item, predictions) for item in value]
    return value


def prepare(manifest):
    require(type(manifest) is dict and set(manifest) == {'schema_version', 'stages'}
            and manifest['schema_version'] == 1 and type(manifest['schema_version']) is int,
            'invalid deployment manifest')
    stages = manifest['stages']
    require(type(stages) is list and 0 < len(stages) <= 64, 'invalid stages')
    predictions = {}
    for stage in stages:
        require(type(stage) is dict and set(stage) == {'id', 'type', 'config'}, 'invalid stage')
        name, kind, cfg = stage['id'], stage['type'], stage['config']
        require(type(name) is str and NAME.fullmatch(name) and name not in predictions, 'invalid or duplicate stage id')
        require(kind in ('gate', 'component') and type(cfg) is dict, 'invalid stage type or config')
        predictions[name] = predicted_addresses(kind, cfg)
    output = []
    used = set()
    for stage in stages:
        name, kind = stage['id'], stage['type']
        config = replace_refs(stage['config'], predictions)
        if kind == 'gate':
            config = deploy_gate.config(config)
            plan = deploy_gate.build(config)
            expected_fields = ('factory', 'guard', 'gate')
        else:
            config = deploy_component.component_config(config)
            plan = deploy_component.build(config)
            expected_fields = ('factory', 'component')
        require(all(plan[field] == predictions[name][field] for field in expected_fields), 'prediction changed')
        identity = (config['chain_id'], plan['factory'])
        require(identity not in used, 'two stages share a factory on one chain')
        used.add(identity)
        output.append({'id': name, 'type': kind, 'config': config,
                       'plan': {key: plan[key] for key in (*expected_fields, 'commitment')}})
    return output


def write_output(outdir, entries):
    require(outdir.is_absolute(), 'output directory must be absolute')
    deploy_gate.private_directory(outdir.parent)
    os.mkdir(outdir, 0o700)
    for entry in entries:
        service.write_new(outdir / (entry['id'] + '.json'), entry['config'])
        os.mkdir(outdir / (entry['id'] + '-state'), 0o700)
    service.write_new(outdir / 'plans.json', {'status': 'unverified_plan', 'stages': [
        {'id': item['id'], 'type': item['type'], **item['plan']} for item in entries]})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, help='private JSON manifest with explicit lane settings')
    parser.add_argument('--output-dir', required=True, help='new private absolute directory')
    args = parser.parse_args()
    entries = prepare(service.read_json(Path(args.manifest), private=True))
    write_output(Path(args.output_dir), entries)
    print(json.dumps({'status': 'unverified_plan', 'output_dir': args.output_dir,
                      'stage_count': len(entries)}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (deploy_gate.TransactionError, service.Error, OSError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
