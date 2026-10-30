#!/usr/bin/env python3
"""Exercise the release-image source binding and non-publishing CI checks."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / '.github/workflows/docker.yml').read_text()
SBOM = (ROOT / '.github/workflows/sbom-analysis-reusable.yaml').read_text()
SMOKE = (ROOT / '.github/workflows/server-image-build-check.yml').read_text()


def command_block(step):
    block = WORKFLOW.split(f'      - name: {step}\n', 1)[1].split('\n      - name:', 1)[0]
    return textwrap.dedent(block.split('        run: |\n', 1)[1])


class ServerImageWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.repo = self.directory / 'repo'
        self.repo.mkdir()
        def git(*args):
            return subprocess.run(['git', *args], cwd=self.repo, check=True,
                                  text=True, capture_output=True).stdout.strip()
        git('init')
        git('config', 'user.name', 'Test')
        git('config', 'user.email', 'test@example.invalid')
        (self.repo / 'file').write_text('source\n')
        git('add', 'file')
        git('commit', '-m', 'source')
        self.sha = git('rev-parse', 'HEAD')
        git('tag', 'v1.2.3')
        self.output = self.directory / 'output'
        self.output.touch()

    def tearDown(self):
        self.temporary.cleanup()

    def run_block(self, step, **values):
        env = {**os.environ, 'GITHUB_OUTPUT': str(self.output), 'GITHUB_EVENT_NAME': 'push',
               'GITHUB_REF_TYPE': 'branch', 'GITHUB_REF': 'refs/heads/main',
               'GITHUB_REF_NAME': 'main', 'GITHUB_SHA': self.sha, 'REPO_OWNER': 'Syscoin',
               'TAG_NAME': '', 'SOURCE_SHA': '', **values}
        return subprocess.run(['bash', '-euo', 'pipefail', '-c', command_block(step)],
                              env=env, cwd=self.repo, text=True, capture_output=True)

    def test_release_call_binds_exact_tag_and_source(self):
        values = {'TAG_NAME': 'v1.2.3', 'SOURCE_SHA': self.sha}
        request = self.run_block('Validate requested source', **values)
        self.assertEqual(request.returncode, 0, request.stderr)
        self.assertIn('ref=refs/tags/v1.2.3', self.output.read_text())
        bind = self.run_block('Bind source and image identity', **values)
        self.assertEqual(bind.returncode, 0, bind.stderr)
        self.assertIn(f'source_sha={self.sha}', self.output.read_text())
        self.assertIn('image_name=ghcr.io/syscoin/zksync-os-server', self.output.read_text())
        bad = self.run_block('Bind source and image identity', TAG_NAME='v1.2.3', SOURCE_SHA='0'*40)
        self.assertNotEqual(bad.returncode, 0)

    def test_rejects_unsafe_tags_and_missing_sha(self):
        for tag, sha in [('latest', self.sha), ('v1/other', self.sha), ('v1;false', self.sha),
                         ('v1', ''), ('', self.sha)]:
            with self.subTest(tag=tag, sha=sha):
                self.assertNotEqual(self.run_block('Validate requested source', TAG_NAME=tag,
                                                  SOURCE_SHA=sha).returncode, 0)

    def test_main_and_push_tags_bind_checkout(self):
        self.assertEqual(self.run_block('Bind source and image identity').returncode, 0)
        self.assertNotEqual(self.run_block('Bind source and image identity', GITHUB_SHA='0'*40).returncode, 0)
        tag = self.run_block('Bind source and image identity', GITHUB_REF_TYPE='tag',
                             GITHUB_REF='refs/tags/v1.2.3', GITHUB_REF_NAME='v1.2.3')
        self.assertEqual(tag.returncode, 0, tag.stderr)

    def test_build_provenance_and_immutable_sbom(self):
        self.assertIn("group: server-docker-${{ github.repository }}-${{ inputs.tag_name || (github.ref_type == 'tag' && github.ref_name) || github.ref }}", WORKFLOW)
        self.assertNotIn('group: ${{ github.workflow }}-${{ github.ref }}', WORKFLOW)
        self.assertNotIn('matterlabs-ci-', WORKFLOW)
        self.assertNotIn('matterlabs-infra', WORKFLOW)
        self.assertIn("github.ref == 'refs/heads/main' && inputs.tag_name == ''", WORKFLOW)
        self.assertIn('push-to-registry: true', WORKFLOW)
        self.assertIn('image_digest: ${{ steps.build.outputs.digest }}', WORKFLOW)
        self.assertIn('image: ${{ inputs.image_name }}@${{ inputs.image_digest }}', SBOM)
        self.assertIn("io.syscoin.server.image-digest", SBOM)
        self.assertIn("io.syscoin.server.source-sha", SBOM)
        self.assertIn('duplicate server identity', SBOM)

    def test_provenance_binds_source_and_rejects_conflict(self):
        uri = 'git+https://github.com/syscoin/zksync-os-server@refs/tags/v1.2.3'
        base = {'buildDefinition': {'buildType': 'https://actions.github.io/buildtypes/workflow/v1',
                                    'resolvedDependencies': []},
                'runDetails': {'builder': {'id': 'runner'}}}
        values = {'PREDICATE_TYPE': 'https://slsa.dev/provenance/v1', 'PREDICATE': json.dumps(base),
                  'SOURCE_SHA': self.sha, 'SOURCE_REF': 'refs/tags/v1.2.3',
                  'GITHUB_SERVER_URL': 'https://github.com', 'GITHUB_REPOSITORY': 'syscoin/zksync-os-server',
                  'RUNNER_TEMP': str(self.directory)}
        result = self.run_block('Bind checked source to provenance', **values)
        self.assertEqual(result.returncode, 0, result.stderr)
        bound = json.loads((self.directory / 'server-image-provenance.json').read_text())
        self.assertIn({'uri': uri, 'digest': {'gitCommit': self.sha}},
                      bound['buildDefinition']['resolvedDependencies'])
        base['buildDefinition']['resolvedDependencies'] = [{'uri': uri, 'digest': {'gitCommit': '0'*40}}]
        result = self.run_block('Bind checked source to provenance', **{**values, 'PREDICATE': json.dumps(base)})
        self.assertNotEqual(result.returncode, 0)
        result = self.run_block('Bind checked source to provenance', **{**values, 'PREDICATE_TYPE': 'bad'})
        self.assertNotEqual(result.returncode, 0)

    def test_pr_build_never_publishes_or_gets_privileged_permissions(self):
        self.assertIn('pull_request:', SMOKE)
        self.assertIn('push: false', SMOKE)
        self.assertIn('contents: read', SMOKE)
        self.assertNotIn('secrets.', SMOKE)
        self.assertNotIn('id-token: write', SMOKE)
        self.assertNotIn('packages: write', SMOKE)


if __name__ == '__main__':
    unittest.main()
