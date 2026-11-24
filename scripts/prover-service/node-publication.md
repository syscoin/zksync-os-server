# Node service publication mode

`service_publication.enabled: true` replaces the node's proof transaction sender
with a protected handoff and canonical receipt observer. Commit and execute
senders retain their existing behavior. A service node does not construct or
require `operator_prove_sk`; the external relayer owns proof transaction signing.
The default is `false`, preserving the existing timelock proof sender.
Record the native verified frontier and the gate's deployment block before
enabling the handoff. Reconcile ordinary in-flight proof submissions and retain
their canonical batch metadata. The node can authenticate independently verified
batches through the gate's native-frontier observation. Ordinary proof or
emergency authority must already be available on the intended settlement layer.

Enable this only on a main node with the batcher and real FRI/SNARK proving. Pin
the reviewed proof gate address/runtime hash, service policy, production V8 VK,
and sequencer. Bootstrap uses zero coordinator address/hash because that
contract is installed after bootstrap. Service activation requires the reviewed
coordinator address and runtime hash.

The node compares `production_vk_hash` with its
[compiled V8 identity](../../lib/types/src/protocol/proving_version.rs), currently
`0x8e6285da659dac3cfff56dbae3bf445e1524b9f08a9270f19d3361a97f8ce990`,
and separately authenticates the deployed production verifier at startup. The
[generated source release](../releases/era-v32/README.md) supplies this nonzero
app-bound candidate identity for the security-patched recursion circuits;
historical service proofs under previous keys do not qualify this identity.
The current minimal-input native proof and normal-profile local EVM acceptance
are recorded separately from live service qualification.
A live service deployment, rental hardware qualification,
and canonical local-chain fixture acceptance remain separate requirements.

Example overlay:

```yaml
service_publication:
  enabled: true
  directory: /secure/zksys/node-service-publication
  gate: '0x...'
  gate_code_hash: '0x...'
  coordinator: '0x0000000000000000000000000000000000000000'
  coordinator_code_hash: '0x0000000000000000000000000000000000000000000000000000000000000000'
  policy_hash: '0x...'
  production_vk_hash: '0x...'
  sequencer: '0x...'
  poll_interval: 2s
  rpc_timeout: 30s
  max_records: 256
```

The directory must be absolute, with no symlink components. Its parent must
exist. The node creates the directory with mode 0700, requires owner-only files,
rejects symlinks and hard-linked files, and holds a process lock. Use a distinct
directory for each child/Gateway execution lane. The settlement provider and
confirmation depth come from the node's actual discovered settlement topology:
`gateway_sender.required_confirmations` for a child settling on Gateway,
`l1_sender.required_confirmations` for a Gateway settling on Syscoin or a direct
child. Depth is inclusive; three means the inclusion block plus two successors.

After the gate is deployed, `publication_config.py` can preview an overlay for
one lane using its private keeper configuration:

```sh
python3 scripts/prover-service/publication_config.py \
  --keeper-config /trusted/child-keeper.json --lane child \
  --execution-chain-id CHILD_CHAIN_ID --settlement-chain-id GATEWAY_CHAIN_ID \
  --directory /secure/zksys/node-service-publication
```

The directory must already exist with owner-only permissions for this preview.
It checks lane and chain identities, reviewed gate and coordinator pins,
the source V8 key, deployed verifier and timelock prover authority at a pinned
RPC head. The output is a JSON object usable as YAML, printed for review; the
command neither edits a node config nor starts a node. Its VK check is against
the source checkout, so independently confirm the node binary uses that build.
Record the provenance of the supplied runtime hashes, inventory all native
prover authorities, and verify both service and ordinary proving on each lane.

After native SNARK acceptance, the node fsyncs one immutable `work.json` per
execution chain/range/native-proof hash. It contains the exact native
`proof_data`, batch output preimages, actual execution/settlement identity,
timelock and reviewed publication pins. No signing key or native lease appears
in this file. The [automatic coordinator and wrapper](keeper-operator-guide.md)
verify canonical batch commitments, the exact native FRI proofs, the SNARK and
the signed dispatch audit before endorsing the package. This flow requires no EN.
The relay uses the identical native `proof_data`.

After `relay.py step` reports confirmed acceptance, export a bound hint:

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --state-dir /trusted/relay-journal \
  --execute export-handoff --operation-id PACKAGE_HASH_WITHOUT_0x \
  --work-dir /secure/zksys/node-service-publication/EXACT_WORK_DIRECTORY
```

Omitting `--execute` previews the hash/path without writing. This command needs
no wallet or RPC connection. It matches node identity, contract/release pins,
range, output preimages and native proof bytes to the immutable accepted relay
artifact, then atomically writes `relay.json` with the transaction hash, sidecar
and Keccak hash of the exact `work.json` bytes. An external acceptance known only
from the gate's parent hash is insufficient: the actual accepting transaction
hash must first be found. This hint does not authorize execution itself.

The node binds the accepted package to its retained batch range, outputs, proof
bytes and duty report. Direct gate transactions must carry zero value and the
exact reconstructed calldata. For a submission forwarded by a smart account or
router, the pinned gate's acceptance event authenticates the package inside the
outer transaction. Other packages can have acceptance events in the same receipt.
The node requires a successful receipt with exactly one matching event, the
reviewed historical gate/coordinator runtimes and configuration, a stable
settlement identity, and canonical inclusion/tip postchecks. It enforces at least
both the stored and current confirmation depths. Pending, reverted, malformed,
foreign, reorged or unavailable receipts keep the native proof waiting. A local
`confirmed.json` never substitutes for fresh chain checks on restart.

For a service acceptance, the node records the mined transaction's calldata hash
and the accepted package hash in `confirmed.json`, notifies the durable
SNARK journal and forwards the original batch envelopes to priority-tree and
execute processing. Restore the service receipt and original `work.json` when
recovering that acceptance.

An independently verified range can advance on its native evidence. The node
checks the execution and settlement identities, reviewed gate code, confirmation
depth and canonical stored hash of each covered local batch. It locates the
gate's first observation of the range and verifies the corresponding
`NativeSettlementAdvanced` event. For a batch already verified before gate
deployment, it checks the predeployment native frontier. RPC access to that
historical state and its logs is required. Preserve the journals if historical
evidence is unavailable or conflicts with local metadata.

When native progress covers part of a pending service aggregate, the original
endorsements become stale. The confirmed prefix can continue to execution.
Rebuild the remaining unverified suffix from its native batches and obtain a
fresh package. A local `superseded.json` records disposition for recovery.
Service credit still requires an authenticated service acceptance.

If native settlement cuts through a wrapper, startup can also retire a following
unproved wrapper to form a legal proof range. For example, when native settlement
proves batches 1–2 and the journal holds 1–3 and 4–5, recovery can rebuild 3–5
as one wrapper when the configured capacity permits three batches. The node saves
`regrouped.json` against the exact old work and confirmed native batch hash before
removing the wrapper. Keep that record across restarts. The replacement package
needs fresh endorsements. Only a confirmed service acceptance can earn credit.

A bootstrap receipt remains valid when coordinator installation follows it in
the same block. The successful exact `submitBootstrap` call proves the bootstrap
phase at execution time. On a later service restart, original zero-coordinator
bootstrap work keeps its original bytes and hash; the node allows only that
coordinator transition while all native/deployment pins remain identical.

Completed publication records remain available for recovery and audit. Before
moving a publication to backup, verify canonical execution through its last batch
and check the coordinator journal. Neither its `active` operation nor any
`regrouped_pending` operation may still reference that publication. Retain the
backup. If an archived publication is needed to deliver an acceptance for the
first time or with a different transaction hash, the coordinator records
`handoff_restore_required` on that operation. Restore the original directory
at its recorded path so the coordinator can deliver the authenticated handoff.
Keep native metadata and the applicable service receipts or
canonical native observation evidence for proved-but-unexecuted ranges. The bounded journal fails closed when `max_records` is
exhausted. Confirmation depth is not a claim of Bitcoin ChainLock finality.
