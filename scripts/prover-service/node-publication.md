# Node service publication mode

`service_publication.enabled: true` replaces the node's proof transaction sender
with a protected handoff and canonical receipt observer. Commit and execute
senders retain their existing behavior. A service node does not construct or
require `operator_prove_sk`; the external relayer owns proof transaction signing.
The default is `false`, preserving the existing timelock proof sender.
Before enabling it on an existing node, finish canonical execution of every
ordinary proof already in flight. Service recovery requires a gate receipt and
retained work for every proved-but-unexecuted range; it cannot adopt an earlier
ordinary timelock proof without those records.

Enable this only on a main node with the batcher and real FRI/SNARK proving. Pin
the reviewed proof gate address/runtime hash, service policy, production V8 VK,
and sequencer. Bootstrap uses zero coordinator address/hash because that
contract is installed after bootstrap. Service activation requires the reviewed
coordinator address and runtime hash.

The node compares `production_vk_hash` with its
[compiled V8 identity](../../lib/types/src/protocol/proving_version.rs), currently
`0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe`,
and separately authenticates the deployed production verifier at startup. The
[validated source release](../releases/era-v32/README.md) supplies this nonzero
app-bound identity; a live service deployment, rental hardware qualification,
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

After native SNARK acceptance, the node fsyncs one immutable `work.json` per
execution chain/range/native-proof hash. It contains the exact native
`proof_data`, batch output preimages, actual execution/settlement identity,
timelock and reviewed publication pins. No signing key or native lease appears
in this file. Prepare the signed service package through the normal independent
EN and operator workflow, and relay its identical native `proof_data`.

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

The node reconstructs `service_submission` from its retained native command and
checks the actual mined transaction's destination, zero value and exact calldata.
It requires a successful receipt and exactly one matching gate acceptance event,
the reviewed historical gate/coordinator runtimes and configuration, a stable
settlement identity, and canonical inclusion/tip postchecks. It enforces at least
both the stored and current confirmation depths. Pending, reverted, malformed,
foreign, reorged or unavailable receipts keep the native proof waiting. A local
`confirmed.json` never substitutes for fresh chain checks on restart.

Only after these checks does the node fsync `confirmed.json`, notify its durable
SNARK journal, and forward the original batch envelopes to priority-tree and
execute processing. At startup, covered native journal commands pass the same
receipt observer before ordinary recovery can retire them. Already-proved
passthrough batches also require matching retained service work and a canonical
receipt. There is no second proof computation, resubmission, or local signature
step. Missing handoff data keeps recovery closed; restore the original protected
journals rather than deleting records to bypass it.

A bootstrap receipt remains valid when coordinator installation follows it in
the same block. The successful exact `submitBootstrap` call proves the bootstrap
phase at execution time. On a later service restart, original zero-coordinator
bootstrap work keeps its original bytes and hash; the node allows only that
coordinator transition while all native/deployment pins remain identical.

Completed publication records remain available for recovery and audit. Archive
only ranges already executed on the canonical settlement chain, retaining an
operator backup; proved-but-unexecuted ranges still need their work/receipt
handoff after a restart. The bounded journal fails closed when `max_records` is
exhausted. Confirmation depth is not a claim of Bitcoin ChainLock finality.
