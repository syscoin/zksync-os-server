# Initial prover enrollment

`bootstrap_enrollment.py` enrolls one Sentry for the first proving period after
its membership update has finalized on zkSYS. The Sentry account and its chosen
operator first sign the same subscription. The tool verifies both EOA signatures,
the registry's reviewed runtime and configuration, finalized senior membership,
the period-zero enrollment window, and an exact simulated transaction. It then
uses the Sentry account's Foundry keystore to send the enrollment transaction.
The tool does not create keys, sign the consents, enroll an ERC-1271 account, run
the membership keeper, produce proofs or activate the service.

Prepare the account-signed subscription and operator signature using the
subscription and operator-subscription commands in [README.md](README.md). The
subscription must start in period zero, have `services: 3`, and have its registry
address, sequencer, operator, beneficiary and nonce checked against the intended
deployment. Retain the signed JSON and the operator's 65-byte hex signature in
owner-only files. The EOA account must have enough zkSYS gas and must be the same
account as the enrollment transaction keystore.

Create an owner-only JSON config with these exact fields:

```json
{
  "settings": {
    "schema_version": 1,
    "execution_chain_id": "0xREPLACE",
    "registry_chain_id": "0xREPLACE",
    "chain_address": "0xREPLACE",
    "settlement_chain_id": "0xREPLACE",
    "registry": "0xREPLACE",
    "coordinator": "0x0000000000000000000000000000000000000000",
    "proof_gate": "0xREPLACE",
    "policy_hash": "0xREPLACE",
    "vk_hash": "0xREPLACE",
    "sequencer": "0xREPLACE",
    "duties_per_round": 2
  },
  "rpc_url": "http://127.0.0.1:3050",
  "genesis_hash": "0xREPLACE",
  "registry_code_hash": "0xREPLACE",
  "max_head_age_seconds": 60,
  "rpc_timeout_seconds": 15,
  "gas_limit": 30000000,
  "gas_price": 1,
  "keystore": "/private/path/account-keystore.json",
  "password_file": "/private/path/account-password"
}
```

Replace the placeholders with independently reviewed addresses and exact hashes.
The other numeric values are examples and must reflect the actual deployment.
The registry is the zkSYS service registry, `registry_chain_id` is the zkSYS
chain ID, and `registry_code_hash` is the hash of that deployed registry runtime.
`rpc_url` must be HTTPS or loopback HTTP and must support EIP-1898 canonical
block-hash calls and the `finalized` block tag. Keep the URL free of credentials.

Preview without sending:

```sh
python3 scripts/prover-service/bootstrap_enrollment.py \
  --config /private/enrollment-config.json \
  --subscription /private/signed-subscription.json \
  --operator-signature /private/operator.sig
```

For the transaction, create an existing owner-only directory and add
`--state-dir /private/enrollment-state --send`. Repeat the same invocation to
reconcile it: the exact signed bytes and nonce are saved before broadcast; if a
submitted transaction disappears, only those bytes are rebroadcast with bounded
backoff. The journal entry remains until its successful receipt is finalized and
the exact subscription and operator mappings are verified at finalized zkSYS
state. A reverted transaction or changed subscription stops for review. Use a
dedicated account nonce while this journal is unresolved, and retain the journal
and signer through recovery.

Enrollment alone does not create a qualified prover roster or pay a proving
bonus. Real accepted bootstrap duties, roster publication and delivery, both
proof-lane activations, and the production proving path are still required.
