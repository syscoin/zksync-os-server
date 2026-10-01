# Durable trusted-host service relayer

`relay.py` consumes the fully signed `ProverServiceSidecarV1` produced by
`service.py complete` and the exact native `proof_data` from the reviewed package.
It stages immutable artifacts, checks the deployed gate/coordinator context,
requests a transaction signature from a trusted wallet RPC, persists the exact
signed bytes, and only then broadcasts. Every mutation, wallet signing request
and transaction broadcast requires `--execute`. The default path is read-only.
No live transaction has been sent as part of the local tests.

The relayer adds no verifier contract. Its `eth_call` preflight simulates the
existing gate transaction without changing state or spending gas; the actual
transaction invokes the existing native settlement verification once. It never
opens or repairs a package, changes a duty report, renews an endorsement, alters
a fee beneficiary, or selects a replacement wrapper.

## Deployment and wallet inputs

Use the same reviewed `config.json` as the signing workflow. Populate
`relay-policy.example.json` with the relay account, deployed gate/coordinator
and priority-guard runtime Keccak code hashes, fixed gas and fee caps, required confirmations,
minimum remaining turn time, and maximum permitted RPC head age. The template's
zero values deliberately fail validation, except that bootstrap uses a zero
coordinator address and may use a zero coordinator runtime hash. Service mode
requires both reviewed coordinator pins. Code hashes must come from the
reviewed deployed release; copying hashes from an unknown RPC does not authenticate
the deployment. The relay account can be separate from the sequencer and wrapper.

Fees are in wei. The maximum native-token exposure for one transaction is
`gas_limit * max_fee_per_gas`, which must fit `max_total_fee_wei`. The relayer
does not bump fees or replace a transaction automatically. A base-fee rise may
leave a transaction pending; it retains that nonce and authorization. Configure
fees using the settlement chain's actual gas policy and measured native-proof
gas use. Remaining turn time is a submission margin, not an inclusion guarantee.

Use one private journal directory and one dedicated relay wallet across all
packages. Do not run an unrelated transaction sender against the same account,
or create multiple journals for it. The file lock serializes local operations;
confirmed and pending nonce checks detect other activity, but cannot coordinate
an external wallet racing the same nonce. A competing consumed nonce stops the
relayer for investigation instead of inventing a replacement authorization.

Copy `relay-rpc.example.json` into a mode-0600 file owned by the operator. Use an
independently trusted settlement-node endpoint supporting EIP-1898 canonical
block-hash reads, receipts and logs. The wallet endpoint must support the
**signing-only** `eth_signTransaction` method and return a signed EIP-1559 raw
transaction. An endpoint that broadcasts as part of signing is unsupported:
it breaks the durable-before-send boundary. Neither a plain browser
`eth_sendTransaction` wallet nor an RPC without raw signing is interchangeable.

Optional authorization headers stay in this private file. HTTPS is required
except for loopback HTTP; redirects and environment proxies are disabled.
Endpoints, credentials, private keys and raw transaction bytes are omitted from
public status. `eth_signTransaction` receives only the bounded, explicit zero-value
gate transaction. The returned raw bytes are independently RLP-decoded and checked
for the intended chain, nonce, fees, gas, destination, value, calldata, empty
access list and recovered sender. A wallet cannot silently alter those fields.

Account signing, wallet setup and funded production deployment remain operator
actions. This program accepts no private-key argument and never sends signing
material or RPC credentials to the rental image.

## Prepare, inspect and stage

Keep the signed sidecar and original package review artifact on the trusted
host. Extract its exact `proof_data` to a private text file, for example:

```sh
umask 077
jq -r .proof_data /trusted/package-request.json > /trusted/proof-data.hex
```

The relayer revalidates the signatures, report hash, native calldata encoding,
batch-output preimages and full duty statements through the same portable
checker before accepting the artifact. The independent EN and native proof
verification requirements in `README.md` still apply; this does not turn copied
producer metadata into independent evidence.

Inspect the current state without staging, signing or broadcasting:

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --rpc-file /trusted/relay-rpc.json \
  inspect --sidecar /trusted/service-sidecar.json \
  --proof-data /trusted/proof-data.hex
```

The output reports the exact current wrapper index, turn, deadline, candidate
operator, range, accepted parent and next action. Reads bind to one canonical
block hash, and the block/chain are rechecked after dependent calls. The
deployment code hashes, chain ID, child-chain address, sequencer, policy, VK,
signing domain, frozen package commitment, roster root and current candidate
must match. The full gate call is simulated with the configured gas limit.
The status also shows the priority guard's frozen work, checkpoint hash, cursor,
required/capped prefix end and expiry. Missing freeze, expired work, mismatched
prefix counts or a missing prefix witness prevent signing.

For service mode, the sequencer must already have opened the exact normalized
package. For bootstrap, it must have frozen bootstrap work through
`openBootstrapPackage` before submission. The mandatory priority guard must
have the required current checkpoint and prefix witness. Those operations are
separate authenticated workflows; missing or expired guard state makes the
exact gate simulation refuse signing. Repairs and checkpoint refreshes must
preserve the coordinator's schedule as the contracts require.

Stage the immutable artifact under its package hash:

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --rpc-file /trusted/relay-rpc.json \
  --state-dir /trusted/relay-journal --execute \
  stage --sidecar /trusted/service-sidecar.json --proof-data /trusted/proof-data.hex
```

The journal must be an absolute, operator-owned mode-0700 directory. The first
stage creates it; artifacts and atomic state files are mode 0600 and fsynced.
Reuse of a package hash requires identical artifacts. The frozen config and fee
policy cannot silently change on restart. Record the returned `operation_id`.

## Sign, send and reconcile

Run a dry step first to see the next action:

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --rpc-file /trusted/relay-rpc.json \
  --state-dir /trusted/relay-journal step --operation-id PACKAGE_HASH_WITHOUT_0x
```

Add `--execute` before `step` to permit that operation's next durable action:

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --rpc-file /trusted/relay-rpc.json \
  --state-dir /trusted/relay-journal --execute \
  step --operation-id PACKAGE_HASH_WITHOUT_0x
```

Each invocation performs bounded work. A supervised keeper may invoke this same
explicit command periodically. It never chooses a new package, changes an
endorsement or creates a replacement transaction. Plain `status` reads only the
durable journal and needs no RPC file; `step` reconciles current chain state.

The important recovery cases are:

| Durable state | Next action |
| --- | --- |
| Staged and current | Reserve the exact unsigned transaction, then request wallet signing. |
| Interrupted signing | Retry the identical unsigned request only while its nonce and authorization still match. Signing must not broadcast. |
| Signed / send uncertain | Recover the deterministic hash; only identical raw bytes may be sent again. |
| Pending and current | Reconcile receipt; bounded rebroadcasts use the same nonce, fees and bytes. |
| Turn expired or package repaired, no raw tx | Refuse the stale artifact; new work requires fresh endorsements. |
| Turn expired or repaired, raw tx exists | Keep the reservation and monitor its receipt; do not resign or rebroadcast stale authorization. |
| Another relayer accepted, own tx pending | Stop broadcasting but retain the own-nonce reservation until its receipt resolves. |
| Consumed nonce without this hash's receipt | Stop for investigation; no automatic replacement or forgetting. |
| Canonical receipt below confirmation policy | Retain the nonce and wait. |
| Confirmed revert | Mark reverted; no service acceptance is claimed. |
| Confirmed success and exact gate event | Mark accepted and release the reservation. |

The signed raw bytes and transaction hash are fsynced before the first
`eth_sendRawTransaction`. Send timeouts, provider rejection, malformed replies
and process interruption keep the same durable record. An acceptance event or
the gate's accepted-parent state can identify acceptance by another relayer;
an advanced parent alone never proves this package was accepted. A successful
own receipt must contain the exact gate event for the package/range/bootstrap
mode. Existing canonical durable receipts survive transient RPC absence.

Canonical block hashes are rechecked before confirmation. A reorg clears
unconfirmed observations and returns to reconciliation. Configured confirmation
depth is an operator finality policy, not an on-chain ChainLock assertion;
deep reorgs remain possible. Keep monitoring completed operations if that risk
requires it, and use the chain's authenticated finality tooling operationally.

There is no destructive forget, nonce-cancel, fee-bump or policy-migration
command. Manual recovery must establish the real canonical transaction/nonce
state first. State capacity is bounded (256 operations / 16 MiB); reaching it
stops new activity while preserving existing records. Retain backups and migrate
only after pending wallet reservations are resolved.

## Local verification

```sh
python3 -m unittest discover -s scripts/prover-service -p 'test_relay.py' -v
```

The tests use fake RPC/wallet servers and public trivial test keys. They verify
actual EIP-712 signatures and raw transaction recovery with Foundry, exact
bootstrap/service ABI calldata, crash boundaries, ambiguous send recovery,
stale turns/repair, external acceptance, competing nonces, receipts/reorgs,
deployment identity, fee/destination tampering and dry-run behavior. They do not
broadcast, deploy a chain, or qualify a production proof/VK.

## Return confirmed acceptance to the node

With opt-in node service publication enabled, run `export-handoff` against the
matching immutable node work directory after confirmed relay acceptance. It
writes only a hash-bound receipt hint; the node independently checks the exact
native gate transaction and canonical receipt before advancing execution. See
[node-publication.md](node-publication.md) for configuration, the command and
restart/activation behavior.

```sh
python3 scripts/prover-service/relay.py --config /trusted/config.json \
  --policy /trusted/relay-policy.json --state-dir /trusted/relay-journal \
  --execute export-handoff --operation-id PACKAGE_HASH_WITHOUT_0x \
  --work-dir /secure/zksys/node-service-publication/EXACT_WORK_DIRECTORY
```

The node mode defaults off. Its [configuration overlay](node-publication.md)
uses a private absolute directory, pinned gate/policy/VK/sequencer, zero
coordinator pins for bootstrap, a two-second poll interval, a 30-second RPC
timeout and a 256-record limit. It requires no node proof signing key. Keep
proved-but-unexecuted records for restart and rotate only after canonical
execution with an operator backup. Bootstrap work retains its original zero
coordinator pins and exact bytes through a service activation restart.
`confirmed.json` alone never authorizes node advancement.
