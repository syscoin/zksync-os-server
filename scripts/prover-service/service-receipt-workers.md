# Service receipt workers

These workers deliver service-accounting receipts for zkSYS and Gateway. They
are separate from the Sentry membership keeper. The local claim worker discovers
source messages and asks the root publisher for an executed Gateway batch root.
After that root arrives on zkSYS, it submits locally verified claims. The native
relay handles Gateway-native activation and accepted reports that cannot be
bundled with a new root. One root message can authorize one Gateway-native
acceptance hash as well as the Gateway batch root.

This code has not been qualified against a live Gateway/NEVM/zkSYS deployment.
Do not run it unattended until the deployed proof, root delivery and accounting
paths have been tested and recovery from the exceptional authorized batch revert
described in [the service design](../../docs/src/design/prover_service_v1.md)
has been qualified.
For a supervised launch, verify that both gates' bootstrap reports are recorded
in the registry before delivering both activation messages. Activation is
permissionless and another caller can bypass worker ordering.

## Prepare configuration

Run from the repository root with Python 3 and Foundry `cast` installed. Create
private directories for configuration, each worker's state, retained child and
Gateway sidecars, and NEVM signer locks. Use absolute paths throughout. Each
config and password file must be owned by the operator, mode `0600`; directories
must be owned by the operator and inaccessible to other users. Do not place keys
in the config files. RPC URLs must be HTTPS or loopback HTTP.

For example, if `/private` is the intended private parent:

```sh
install -d -m 700 /private/service /private/service/claim /private/service/root \
  /private/service/native /private/service/locks /private/service/child-artifacts \
  /private/service/root-artifacts
```

Create `/private/service/receipt.json` with exactly the fields checked by
`configuration()` in [service_receipt_relay.py](service_receipt_relay.py):

- `schema_version` = `1`; `l1_rpc_url`, `gateway_rpc_url`, `zksys_rpc_url`,
  `rpc_timeout_seconds`; the three `*_chain_id` and `*_genesis` fields.
- `root_gate`, `gateway_gate`, `root_sink`, `bridgehub`, `gateway_chain_address`,
  `child_chain_address`, `l1_zksys_diamond`, `bridgehub_implementation`,
  `gateway_mailbox_facet`, `l1_priority_facet`, `receiver`, `registry`, and
  `policy_hash`; their exact code-hash fields are listed in that function.
- `signer`, `refund`, `keystore`, `password_file`, `l1_gas_limit`,
  `l1_gas_price`, `priority_gas_limit`, `gas_per_pubdata`, `child_artifact_dir`,
  `root_artifact_dir`, `gateway_start_block`, `l1_start_block`, `scan_blocks`,
  and `poll_seconds`.

Use the actual deployed addresses, genesis and code hashes and start scanning
at or before each gate's first event. The receipt signer signs zkSYS claims and
Gateway-native NEVM relays; fund it on both chains. Its keystore and password
file must correspond to `signer`. Retain the complete package and signed duties
from each gate transaction; direct gate-call calldata can also supply these.
Missing sidecars for routed or multisig calls remain pending for recovery.

Create `/private/service/root-base.json` with all fields required by
`configuration()` in [service_gateway_root_relayer.py](service_gateway_root_relayer.py),
except `requests_file`, `requests_identity`, and `signer_lock_dir`, which the
command below supplies. Its `signer`, keystore and password file are for a
**different NEVM address** from the native relay: the two continuously running
NEVM workers take an exclusive signer lock. Fund that address for root relay
fees. Verify the pinned MessageRoot proxy and implementation, Gateway executed
facet, priority facet, both chain diamonds and code hashes against the actual
deployment. These are security bindings, not placeholder values.

After creating those two operator-reviewed files and the private directories,
set the actual zkSYS gas price in wei and the native fallback delay in seconds,
then derive the three worker configs and their shared journal identities:

```sh
export ZKSYS_CLAIM_GAS_PRICE_WEI='<actual positive integer>'
export ZKSYS_NATIVE_FALLBACK_SECONDS='<positive integer at most 86400>'
python3 - <<'PY'
import hashlib, json, os
from pathlib import Path
p = Path('/private/service')
receipt = json.loads((p / 'receipt.json').read_text())
root = json.loads((p / 'root-base.json').read_text())
canonical = lambda obj: json.dumps(obj, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()
identity = lambda obj: '0x' + hashlib.sha256(canonical(obj)).hexdigest()
claim = {'receipt': receipt, 'zksys_gas_price': int(os.environ['ZKSYS_CLAIM_GAS_PRICE_WEI'])}
requests = str(p / 'claim' / 'service-root-requests.json')
root.update(requests_file=requests, requests_identity=identity(receipt), signer_lock_dir=str(p / 'locks'))
native = {'receipt': receipt, 'claim_journal': str(p / 'claim' / 'service-local-claim.json'),
          'claim_identity': identity(claim), 'requests_file': requests, 'requests_identity': identity(receipt),
          'fallback_delay_seconds': int(os.environ['ZKSYS_NATIVE_FALLBACK_SECONDS']),
          'signer_lock_dir': str(p / 'locks')}
for name, value in [('claim.json', claim), ('root.json', root), ('native.json', native)]:
    fd = os.open(p / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as out:
        out.write(canonical(value) + b'\n')
PY
```

The generator refuses to overwrite an existing config. Preserve configs and state
across restarts; changing a config changes its identity. All three commands can
first validate config without broadcasting:

```sh
python3 scripts/prover-service/service_local_claim.py --config /private/service/claim.json --state-dir /private/service/claim
python3 scripts/prover-service/service_gateway_root_relayer.py --config /private/service/root.json --state-dir /private/service/root
python3 scripts/prover-service/service_native_receipt_relayer.py --config /private/service/native.json --state-dir /private/service/native
```

Run each command continuously under a process supervisor, adding `--execute`.
For a single supervised iteration add `--once`; it does not continue polling.
Start the local claimant first: it publishes a private root-request descriptor
and its discovery journal for the other workers. The descriptor points to the
claim journal, so the publisher and native relay need read access to both files
and the journal's adjacent SQLite database. Keep the whole state directory
together across restarts and backups. Existing JSON journals and request inboxes
are read during migration. The publisher and native relay wait for those files.
Monitor process failures, missing artifacts, unconfirmed
transactions, retry/split status and accounting deadlines. The local worker
estimates and simulates zkSYS claims and splits rejected batches; no fixed small
report limit is imposed. The worker uses a `1 << 24` transaction gas ceiling;
qualify zkSYS estimation and overhead on the deployed chain. In-flight signed
transactions are journaled and retry only their saved bytes after disappearance.

Root and native publishers confirm the NEVM transaction and zkSYS delivery; the
claimant confirms finalized zkSYS state. They inherit the native priority
operation's finality assumptions. The source scanner reads every receipt of
finalized source blocks and therefore needs a complete, trusted RPC view. When a
whole-block receipt response is too large it retries individual receipts; an
individual response over the RPC size limit fails closed and requires operator
attention. Real settlement latency must fit the configured accounting grace
window or late duty reports may not earn credit.
