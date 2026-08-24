## Local fixture regeneration pending

Historical v32.0 local-chain material was removed because it represented stock
testnet/Gateway state rather than the canonical Execution V7 / Proving V8
identity with the final patched-v0.4 Era contracts.

The complete state, genesis, databases, contract addresses, verification key,
and version metadata must be regenerated and attested together. The gate and
removal conditions are recorded in
[`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED`](../../../local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED).

Runnable `run_local.sh`, Anvil, fake-prover, and ephemeral-mode examples will be
restored only when that marker is removed in the same change as the fresh
canonical V8 fixture.
