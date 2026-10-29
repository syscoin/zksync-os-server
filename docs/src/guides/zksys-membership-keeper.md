# zkSYS Sentry membership keeper

The keeper discovers Core Sentry addresses, reconciles the NEVM bridge with the
zkSYS membership registry, and activates mature Sentry reward weights. A node
still needs its own NEVM address registered on Core. The issuer's rewards must
be distributed and claimed separately. Start early enough for the initial
priority operations to reach zkSYS and activate before issuer `startTime()`.

The command defaults to reporting only. `--send` enables signed transactions;
`--watch` repeatedly runs it. Run exactly one keeper per signer and state
directory. The state directory, config, keystore and password files contain
sensitive data. In particular, `transactions.json` holds signed spend
authorizations; back up and protect it, and do not erase it after a timeout.
The keeper records signed bytes before broadcast and resumes the same bytes on
restart. It does not automatically replace a nonce or recover a reverted or
conflicting transaction.

## Configuration

Provision an existing, absolute, owner-only state directory (`chmod 700`) and an
owner-only config JSON file. Independently attest the exact networks, deployed
addresses, proxy implementations and runtime bytecode hashes. A signer on each
chain needs native SYS for gas; the L1 signer also funds priority-operation
base costs. Neither signer needs admin permissions. Use separate dedicated
keystores and budget funds. Choose gas limits after measuring actual priority
operations on the target release, not from deposit defaults. Example schema:

```json
{
  "l1_chain_id": 5700,
  "l2_chain_id": 57057,
  "l1_genesis": "<NEVM genesis hash>",
  "l2_genesis": "<zkSYS genesis hash>",
  "bridge": "<L1 registry bridge proxy>",
  "bridgehub": "<L1 Bridgehub>",
  "diamond": "<L1 zkSYS chain diamond>",
  "membership": "<L2 membership registry proxy>",
  "weights": "<L2 reward weight registry proxy>",
  "issuer": "<L2 issuer proxy>",
  "l1_alias": "<L1 bridge alias on L2>",
  "l1_signer": "<L1 keeper EOA>",
  "l2_signer": "<L2 keeper EOA>",
  "refund": "<reviewed L2 refund recipient>",
  "l1_keystore": "/secure/l1-keystore.json",
  "l1_password_file": "/secure/l1-password",
  "l2_keystore": "/secure/l2-keystore.json",
  "l2_password_file": "/secure/l2-password",
  "max_batch_size": 32,
  "l1_gas_limit": 1000000,
  "l1_gas_price": 1000000000,
  "priority_gas_limit": 5000000,
  "gas_per_pubdata": 800,
  "max_priority_fee": 1000000000000000000,
  "max_l1_transaction_cost": 2000000000000000000,
  "l2_gas_limit": 1000000,
  "l2_gas_price": 1000000000,
  "max_l2_transaction_cost": 10000000000000000,
  "diamond_facets": [
    {"selector": "<facetAddress(bytes4) selector>", "address": "<getters facet>", "code_hash": "<getters runtime hash>"},
    {"selector": "<bridgehubRequestL2Transaction selector>", "address": "<mailbox facet>", "code_hash": "<mailbox runtime hash>"},
    {"selector": "<l2TransactionBaseCost selector>", "address": "<mailbox facet>", "code_hash": "<mailbox runtime hash>"}
  ],
  "code_checks": [
    {"chain": "l1", "address": "<bridge>", "code_hash": "<proxy runtime hash>", "implementation": "<bridge implementation>", "implementation_code_hash": "<bridge implementation hash>"},
    {"chain": "l1", "address": "<bridgehub>", "code_hash": "<proxy runtime hash>", "implementation": "<bridgehub implementation>", "implementation_code_hash": "<bridgehub implementation hash>"},
    {"chain": "l1", "address": "<diamond>", "code_hash": "<runtime hash>"},
    {"chain": "l2", "address": "<membership>", "code_hash": "<proxy runtime hash>", "implementation": "<membership implementation>", "implementation_code_hash": "<implementation hash>"},
    {"chain": "l2", "address": "<weights>", "code_hash": "<proxy runtime hash>", "implementation": "<weights implementation>", "implementation_code_hash": "<implementation hash>"},
    {"chain": "l2", "address": "<issuer>", "code_hash": "<proxy runtime hash>", "implementation": "<issuer implementation>", "implementation_code_hash": "<implementation hash>"}
  ]
}
```

All angle-bracket values and example gas/fee values must be replaced with
reviewed deployment-specific values. Hashes are Ethereum Keccak-256 hashes of
deployed runtime bytecode. The keeper verifies L1 and L2 chain IDs/genesis,
registry wiring, bridge alias and code hashes. It checks EIP-1967 implementation
slots for the registry bridge, Bridgehub and the three L2 contracts. The required
diamond selector entries pin the getter, priority-request and fee-quote facets
and their runtime hashes. This is not a full diamond or governance audit: the
getter is itself contract code and other selector changes may affect behavior.
Independently attest the full diamond and monitor upgrades before enabling sends.

Set `ZKSYS_CORE_RPC_URL`, `ZKSYS_L1_RPC_URL`, and `ZKSYS_L2_RPC_URL` to the
private authenticated endpoints. When needed, set their corresponding
`ZKSYS_CORE_RPC_AUTHORIZATION`, `ZKSYS_L1_RPC_AUTHORIZATION`, and
`ZKSYS_L2_RPC_AUTHORIZATION` environment variables. Run from the checked-out
server release, replacing the placeholders consistently:

```sh
python3 scripts/zksys-membership-keeper.py \
  --state-dir /secure/zksys-keeper-state --bridge '<L1 registry bridge proxy>' \
  --membership '<L2 membership registry proxy>' --l1-chain-id 5700 --l2-chain-id 57057
```

Review the report first. Once identity, funding and end-to-end test gates are
satisfied, run the same command with `--send --config /secure/keeper.json
--watch --interval 60` under a supervised service. Arrange service startup after
Core, NEVM and zkSYS RPC readiness and their contract bootstrap; keep the private
state directory persistent across restarts. Do not start multiple service
instances sharing a signer, even if they use different state directories. The
launch scripts do not install or start this service.

For a Linux systemd deployment, an operator can adapt this example after
reviewing the config and the checks below. Use a dedicated service account and
an owner-only environment file containing the RPC URLs and authorization headers.
This is a template; replace the release path and contract addresses, then install
and enable it through the operator's normal deployment process.

```ini
[Unit]
Description=zkSYS Sentry membership keeper
Wants=network-online.target
After=network-online.target

[Service]
Type=exec
User=zksys-keeper
Group=zksys-keeper
WorkingDirectory=/opt/zksync-os-server
EnvironmentFile=/etc/zksys-keeper/rpc.env
Environment=PATH=/usr/local/bin:/usr/bin:/bin
UMask=0077
ExecStart=/usr/bin/python3 /opt/zksync-os-server/scripts/zksys-membership-keeper.py --state-dir /var/lib/zksys-keeper --bridge <L1-bridge-address> --membership <L2-membership-address> --l1-chain-id 5700 --l2-chain-id 57057 --send --config /etc/zksys-keeper/config.json --watch --interval 60
Restart=on-failure
RestartSec=10
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

The dedicated account must be able to read the private config, keystores and
password files and write its state directory; `cast` must be present on PATH.
The watch loop reports per-pass failures and keeps running, so process restart
alone is not an alert: monitor its error output and reconciliation status.

The outbox is serial: while an L1 request or activation waits for finality,
discovery continues, but new updates, including urgent removals, wait. Monitor
oldest unsent removal, finality lag, transaction failures, insufficient funds,
process health and stale discovery; set and enforce an operator-owned maximum
lag. A live `--watch` process does not establish successful synchronization.
The current exact receipt-block state checks fail closed if a later transaction
in the same block supersedes a delivered update or activation; operator
reconciliation is required. Before launch, test initial sync, removals, address
changes, delayed delivery, failed L2 execution, restart after ambiguous
broadcast, finality outage, signer/funding failures and serial-outbox backlog on
the intended Gateway topology. The focused mock tests do not establish this
acceptance. See [keeper release gates](../../audits/syscoin-v32-production-readiness-2026-09-21/membership-keeper.md).
