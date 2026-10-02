## Local fixture regeneration pending

Historical v32.0 local-chain material was removed because it represented stock
testnet/Gateway state rather than the canonical Execution V7 / Proving V8
identity with the final patched-v0.4 Era contracts.

The complete state, genesis, databases, contract addresses, verification key,
and version metadata must be regenerated and attested together. The gate and
removal conditions are recorded in the repository marker
`local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED`.
That marker blocks fixture consumption, not exact contract-source materialization
or fresh deployments using the completed app-bound V8 key. Real nodes still verify
the deployed production verifier and matching VK; a running deployment does not
qualify the absent packaged fixture.
<!-- SYSCOIN: Keep the repository-relative marker as literal text because mdBook
link checking deliberately rejects links that escape the book root. -->

Runnable `run_local.sh`, Anvil, fake-prover, and ephemeral-mode examples will be
restored only when that marker is removed in the same change as the fresh
canonical V8 fixture.
