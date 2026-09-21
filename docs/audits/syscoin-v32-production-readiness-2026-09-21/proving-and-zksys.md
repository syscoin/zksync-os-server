# Syscoin v32 proving, contracts, and release-readiness audit

Audit date: 2026-09-21. Scope: read-only review of the local `zksync-os-server` and adjacent `zksync-airbender-prover` repositories, plus local regression tests. No transactions, deployments, server mutations, or source edits were performed. Forge outputs/caches were isolated under `/tmp`.

## Deployment context and conclusion

The user clarified that the running deployment is intentionally v31, with v32 planned after prover testing and a testnet reset. The local v32 regeneration sentinels are therefore **planned transition prerequisites, not evidence that the existing v31 deployment is incorrectly configured**. This audit does not certify v31 or compare its binaries with current v32 source.

The reviewed v32 tree currently cannot provide real canonical Syscoin proofs: its app identity, Security100 commitment, verification key, production Gateway derivation, and fixture are deliberately unfinished. These must be completed before the v32 reset can count as a release candidate. They must not be bypassed merely to get a running testnet.

## Confirmed evidence

1. **Canonical app and verification key are absent.**
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/lib/types/src/protocol/proving_version.rs:34-36`: all-zero `V8_VK_HASH`, `V8_VK_REGENERATION_REQUIRED = true`.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/node/bin/src/prover_api/fri_proof_verifier.rs:273-283`: source tree `9e677f536230cc87c1bce8011f3a8074eb39e37a`, zero app MD5/end parameters/Security100 chain, regeneration flag true. Lines 94-103 reject real verification pending regeneration, including library/recovery callers.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/node/bin/src/lib.rs:2035-2045`: external real proving is rejected while regeneration is incomplete; fake proving must use both FRI and SNARK pools. Lines 2075-2105 require the explicit testnet verifier for fake mode, production verifier for real mode, nonzero compiled VK, and an exact deployed VK match.
   - `/Users/sidhujag/Documents/GitHub/zksync-airbender-prover/crates/protocol_version/src/lib.rs:85-104`: companion prover has the zero VK sentinel and an older nonzero app commitment. Lines 135-139 explicitly reject release and require atomic prover/server/Era updates. Do not substitute that existing binary/commitment for the newly reviewed guest.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/.github/workflows/syscoin-v32-v8-keygen.yml:63-79`: all app identity values and Security100 commitment are placeholders; lines 156-177 reject unattested identity or zero parameters.

2. **Production Gateway/relay identity must precede final app/keygen.**
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/local-chains/v32.0/gateway-identity.v1.json:3-5`: `integration-candidate`, `production_attested=false`; evidence scope is an Anvil fork of Tanenbaum.
   - Same file, lines 40-46 and 111-118: some candidate CREATE2 inputs remain unknown and ValidatorTimelock address derivation was not independently recomputed.
   - Same file, lines 130-168: production owners, governance, factories/salts and machine derivation record are null; recomputation booleans false.
   - Same file, lines 84-85: the **historical** settlement probe stopped on an integration-candidate identity mismatch. This is not a statement about current live v31 health.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED:22-35`: explicitly orders final identity attestation before guest/app/VK/genesis/L1 state/database/address regeneration; changed target/relay identity requires repinning before keygen.

3. **Canonical end-to-end fixture is not available.**
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/local-chains/v32.0/CANONICAL_V8_REGENERATION_REQUIRED:17-20`: app binary, end parameters, Security100 chain, VK, fixture not regenerated.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/.github/workflows/spec-tests.yaml:216-223`: canonical E2E CI deliberately exits while that marker exists.
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/scripts/gateway-launch/_common.sh:546-560`: source resolution allows only the reviewed no-proofs/testnet exception while the canonical fixture is pending.

4. **Production fake-proof rejection is explicitly represented in the reviewed patch.**
   - `/Users/sidhujag/Documents/GitHub/zksync-os-server/scripts/patches/era-contracts-syscoin.patch:2112-2139`: sole canonical V8 selector; production wrapper reports `IS_TESTNET_VERIFIER=false`; canonical PLONK VK introspection.
   - Same patch, lines 2174-2193: explicit testnet subclass rejects root L1 and execution chain IDs 1/57 and returns its testnet marker; mock proofs do not establish cryptographic correctness.
   - The adjacent `zksync-era/contracts` checkout was observed to contain an unpatched upstream verifier. It is not used here as evidence of the reviewed patch or live deployment.

## Ordered prover and reset acceptance gates

### Gate 1: Freeze the candidate release identity

Record exact server, guest, Era source/patch, prover, Airbender, wrapper, compiler and toolchain identities. Freeze the intended final root chain ID, Gateway/edge IDs, CREATE2 factories/modes/salts/init code, target and relay runtimes, governance, owner and proxy implementation/admin relationships. Independently recompute both target and relay addresses from the complete typed preimage; retain a hash-bound derivation record. Confirm the source pins and generated artifacts agree. Any address/runtime change that the guest binds requires a new reviewed guest identity before keygen.

### Gate 2: Reproduce app and Security100 key material

Build the final guest twice from clean pinned inputs and compare the binary, executable text, hashes, end parameters and Security100 recursion commitment. Generate the app-bound PLONK key/verifier using the reviewed workflow and validate the workflow's independent lanes/control. Install identical nonzero app/VK values into server, companion prover, verifier artifacts and release metadata together. Clear regeneration flags only with that complete set; do not replace sentinels in isolation.

### Gate 3: Prove correctness before a public v32 reset

Use a disposable deployment with production verification enabled and the exact candidate artifact set. Required matrix:

| Area | Acceptance evidence |
| --- | --- |
| Honest proofs | Real FRI and wrapped SNARK acceptance for Gateway and edge batches through commit/prove/execute; verify active on-chain production marker and exact VK. |
| App binding | Reject stock upstream guest proofs, old Syscoin app proofs, wrong protocol/header/security level, wrong chain configuration and wrong public inputs. |
| Tampering | Reject corrupted/truncated/trailing-byte proofs, reordered or duplicated batches, noncontiguous aggregation, and modified DA/public-input fields. |
| Transactions | Native SYS and ERC-20 transfers, deployments, reverted execution, priority operations, gas tank funded/unfunded paths, and bridge deposit/withdrawal flows. |
| Bitcoin DA / Gateway | Publish, retrieve and verify actual DA data; validate direct and relayed commitments against execution; test unavailable or mismatching data and target/relay identity mismatch. |
| Capacity | Representative full batches and aggregation windows on intended hardware; measure memory, proving time, queue growth, end-to-end finality and headroom against expected load. |
| Recovery | Kill/restart workers and node at accepted FRI/SNARK and commit/prove/execute boundaries; demonstrate lease expiry/reassignment, durable replay, exactly-once or safely idempotent settlement, and no skipped/reordered proof batches. |
| Adversarial availability | Invalid proof submissions, worker loss, settlement RPC loss, L1 reorg/recovery within supported assumptions, DA delay/unavailability and proof-storage pressure all fail closed and produce actionable alerts. |

These are acceptance requirements, not tests claimed to have run in this audit. Reuse existing meaningful tests where available; add missing scenarios only when needed. Preserve receipt hashes, batches, artifact hashes, benchmark data and recovery logs in the candidate release record.

### Gate 4: Prepare and execute the announced v31-to-v32 testnet reset

Back up and label v31 data/configuration before reset. Record the reset boundary and communicate that state/balances/addresses may change. Regenerate the canonical v32 genesis, L1 snapshot, addresses, databases, metadata and fixture atomically from the accepted artifacts. Preserve v31 separately; do not mix its replay/WAL/proof journals/databases with v32. Pin sequencer, external node, explorers, portal, wallet configuration and contract metadata to the same v32 deployment identity. Remove the regeneration marker only when the full generated set exists and the associated checks succeed.

### Gate 5: Accept the fresh v32 testnet as a production candidate

Verify all expected chain IDs, genesis/block hashes, contract code/runtime/implementation hashes, active verifier mode/VK, role owners and Gateway relations. Show real-proven batches advancing for Gateway and edge. Complete native and token bridge round trips with reconciled balances and final settlement receipts. Verify explorers and portal show the same finalization state as the chain, without cached v31 addresses. Confirm external-node catchup/rebuild, persistence after restart, a clean restore drill, bounded finality under load, and alert delivery. Run the restored canonical E2E fixture suite. An arbitrary soak duration is not sufficient by itself; choose a window long enough to exercise scheduled emissions, proof aggregation, renewals and representative load/restarts.

### Gate 6: Mainnet governance and operations signoff

Verify actual owners and all role grants/revocations, multisig signer independence/thresholds, upgrade delays and recovery procedures; remove temporary bootstrap authority and test the intended governance path. Ensure operators/keepers, proof workers and failover/restore capacity are funded, monitored and documented. The current local evidence does not certify any live owner, threshold, upgrade policy, production proof throughput or recovery objective.

## Governance observations

`/Users/sidhujag/Documents/GitHub/zksync-os-server/scripts/gateway-launch/zksys-l2-bootstrap.sh:361-364` requires the bootstrap signer itself to control the configured token admin. Lines 730-765 attest common ProxyAdmin ownership and grant issuer minter, staking-vault updater, and gas-tank burner roles, but do not implement a later governance handoff.

`/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/SyscoinZKSYSToken.sol:55-59` grants the configured admin role-admin authority; `mint` is role-gated and capped at lines 78-84, and the burner role can burn from any holder at lines 87-89. `/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/ZkSysProxyAdmin.sol:8-10` assigns upgrade authority to the configured owner. These are privileged design powers, not an identified access-control bypass. They require live governance verification and an explicit handoff policy before production. Source does not prove that deployed authority is already unsafe or safe.

## Membership freshness: concrete consequence and operational requirement

The local v32 design uses **last relayed membership facts**, with no membership expiry or L1 observation timestamp:

- `/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/ZkSysRegistryBridge.sol:99-149`: anyone may request updates, but NEVM is sampled only during that call. Lines 127-144 derive collateral height and seniority weight and enqueue them for L2.
- `/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/ZkSysMembershipRegistry.sol:20-29`: facts carry account/height/weight, with no freshness deadline or observation block. Lines 138-153 persist those values and notify the reward registry on change.
- `/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/ZkSysRewardWeightRegistry.sol:137-138`: `weightOf()` reads stored weight. Lines 194-204 remove/decrease sentry weight only after a delivered update; lines 228-269 can activate already queued membership increases without checking L1 again.
- `/Users/sidhujag/Documents/GitHub/zksync-os-server/contracts/src/zksys/ZkSysIssuer.sol:99-127`: permissionless distribution allocates scheduled rewards using stored total weight. Lines 194-215 allow claims using stored account weight and mint rewards. On a later removal, lines 224-235 checkpoint elapsed rewards with `oldTotalWeight` and settle the member using `oldWeight` before changing its debt.

Therefore, if a sentry loses its collateral or changes its reward address on L1 and nobody relays the removal, its old L2 weight remains active. It may continue sharing subsequent distributions and claiming rewards; a later removal does not claw back already minted rewards and also checkpoints completed periods up to processing using the old weight. Even an existing pending increase can mature and activate if its L1 invalidation was never delivered. Other eligible members receive a smaller share during that stale interval. A missing seniority update can also underweight a still-valid member. This does **not** bypass the issuer's global schedule/supply cap or the registry's authorized bridge caller; it is a liveness/freshness dependency of snapshot semantics.

No keeper implementation was found in the searched launcher/docs or adjacent `dapp-portal`, `syscoin`, and `zksys-bridge` source paths. A live keeper elsewhere remains possible and was not independently inspected by this subaudit. The parent reported no obvious keeper in the sequencer process list; deployment investigation should settle that uncertainty.

Production requires a named service/operator to continuously reconcile L1 membership with the L2 active set, relay removals and address changes as well as additions/seniority changes, verify final L2 application, maintain funding, retry/recover after outages, and alert on oldest unapplied change and missed reward-period deadlines. Because delivered removals affect accounting prospectively rather than retroactively, the accepted lag/finality policy must be explicit. If the intended economics require exclusion at the actual L1 removal time regardless of relay lag, the current snapshot semantics do not implement that policy and require a separate design change. No new tests or source changes were made for this observation.

## Executed validation

### Solidity: 118 passing, 0 failures, 0 skipped

Commands (from `/Users/sidhujag/Documents/GitHub/zksync-os-server`):

```sh
forge test --root integration-tests/test-contracts --match-path 'test/*ZkSys*.t.sol' --offline --out /tmp/syscoin-v32-audit-forge-out --cache-path /tmp/syscoin-v32-audit-forge-cache --summary
forge test --root integration-tests/test-contracts --match-path 'test/SyscoinZKSYSToken.t.sol' --offline --out /tmp/syscoin-v32-audit-forge-out --cache-path /tmp/syscoin-v32-audit-forge-cache --summary
```

| Suite | Passed |
| --- | ---: |
| ZkSysGasTankTest | 24 |
| ZkSysIssuerTest | 28 |
| ZkSysMembershipRegistryTest | 13 |
| ZkSysNativeStakingVaultTest | 8 |
| ZkSysRegistryBridgeTest | 14 |
| ZkSysRewardWeightRegistryTest | 23 |
| SyscoinZKSYSTokenTest | 8 |
| Total | 118 |

Gas-tank fuzz case ran 256 inputs. Compiler completed with existing mutability warnings only.

Important scope limit: `integration-tests/test-contracts/test/ZkSysGasTank.t.sol:182-198` emulates bootloader ledger writes with `vm.store`; `integration-tests/test-contracts/test/ZkSysRegistryBridge.t.sol:120` and related tests mock the NEVM precompile. Even the issuer test named `testBridgeEncodedSentryFactCanDriveIssuerRewardsEndToEnd` at `integration-tests/test-contracts/test/ZkSysIssuer.t.sol:296-337` uses a mock bridgehub, mock NEVM, and pranked aliased sender. These passing tests verify contract behavior, not actual Syscoin/Gateway/L2 transmission or cryptographic proving.

### Python: 6 targeted tests passed

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest \
  scripts.tests.test_local_patched_zksync_os.LauncherStaticTests.test_pre_keygen_app_identity_is_explicitly_fail_closed \
  scripts.tests.test_local_patched_zksync_os.LauncherStaticTests.test_os_keygen_workflow_attests_current_source_inputs \
  scripts.tests.test_local_patched_zksync_os.EraAttestationStaticTests.test_era_keygen_workflow_attests_current_source_inputs \
  scripts.tests.test_local_patched_zksync_os.CanonicalFixtureGateStaticTests.test_canonical_v32_fixture_is_blocked_until_atomic_v8_regeneration \
  scripts.tests.test_local_patched_zksync_os.CanonicalFixtureGateStaticTests.test_pending_v8_mock_source_pins_require_the_full_testnet_gate \
  scripts.tests.test_local_patched_zksync_os.CanonicalFixtureGateStaticTests.test_gateway_identity_is_authenticated_before_edge_creation
```

Result: six tests passed in 0.121 seconds. Parent agent is separately running the broader Python suite and formatting check; do not double-count these six if included there.

## Explicitly unverified

No real canonical v32 proof generation or on-chain proof acceptance was possible with the current sentinel state. No GPU benchmark, live bridge transaction, live role enumeration, governance threshold check, production derivation attestation, backup restore, or live membership synchronization was performed by this subaudit. No external formal security audit is implied by this review or the passing local tests.

## Extended gas tank and zkSYS economic-contract review

The user subsequently expanded scope to the gas tank, smart accounts and ZKsync contracts. This subaudit deepened the gas-tank, token, staking, membership and issuer review; other agents own smart-account and ZKsync-settlement review. No additional tests or source changes were made in this extension. The 118 Solidity tests above remain the exact executed count.

### Accounting and execution controls checked

- **Precharge, refund and tip conservation:** the guest deducts full fee prepayment from only the sender credit, leaving `totalCredits` unchanged during payload execution. It returns unused gas to the refund recipient, pays priority tip in zkSYS credit and decreases aggregate credit only by `prepayment - refund - tip`. References: `scripts/patches/zksync-os-syscoin-v0.4.0.patch:959-978`, `:1035-1046`, `:1107-1137`, `:1485-1503`, `:1545-1579`. This prevents a payload calling `burnSurplus()` from destroying outstanding refunds/tips. Sender/coinbase aliasing uses sequential reads of current credit, avoiding a stale overwrite.
- **Backing and exact token deltas:** `contracts/src/zksys/ZkSysGasTank.sol:117-164` checks solvency and exact tank/recipient balances for withdrawals; `:190-231` verifies existing backing and exact tank/funder deltas before issuing credit; `:169-185` burns only true surplus and demands exact post-burn backing. Fee-on-transfer and over-debit behavior revert atomically. Transient reentrancy guard at `:62-68` covers funding, withdrawal and surplus burn.
- **Storage integration:** tank slots 0/1 are expressly consensus-critical, `contracts/src/zksys/ZkSysGasTank.sol:28-44`. Guest compare-and-write operations check the expected current slot, preserve EVM warmth and suppress refund deltas for the active precharged sender slot, `scripts/patches/zksync-os-syscoin-v0.4.0.patch:2670-2717`, `:2744-2826`. The guest reserves bounded storage/hash work and 198 bytes of worst-case fee-ledger pubdata, patch `:675-729`.
- **Runtime identity:** bootstrap binds tank init-code hash, address and runtime hash, `scripts/gateway-launch/zksys-l2-bootstrap.sh:660-667`, `:721-728`. Node startup validates the observed runtime independently, `node/bin/src/lib.rs:3735-3774`. The tank is deployed without a proxy, with immutable token address and 18 decimals checked in its constructor, `contracts/src/zksys/ZkSysGasTank.sol:28-32`, `:71-81`.
- **Staking custody:** only the caller's own position is withdrawn; accounting and reward weight change before native value transfer, protected by reentrancy guard, `contracts/src/zksys/ZkSysNativeStakingVault.sol:41-54`, `:73-96`. Failed callbacks revert the whole transaction. Forced native donations do not grant stake weight.
- **Reward activation and snapshots:** increases queue for a configurable bounded 1–7-period delay; decreases remove weight immediately when processed. The registry preserves mature lower pending increases, clears pending weight on decreases, and separates stake from sentry weights, `contracts/src/zksys/ZkSysRewardWeightRegistry.sol:157-224`. Activation first checkpoints old weight through completed periods, preventing a late entrant receiving old emissions; issuer `contracts/src/zksys/ZkSysIssuer.sol:224-246`. Empty-registry periods are skipped, issuer `:132-148`.
- **Mint/burn limits and authority:** token initializes at zero supply; mint enforces current supply <= 210 million units adjusted for decimals, `contracts/src/zksys/SyscoinZKSYSToken.sol:49-59`, `:78-89`. The sole canonical issuance schedule separately caps cumulative scheduled rewards, `contracts/src/zksys/ZkSysIssuer.sol:111-127`, `:158-178`. Claims settle caller weight, clear accrued liability and decrement available scheduled rewards before mint, `:194-215`. The role admin and ProxyAdmin remain powerful privileged actors, as documented above.

No concrete permissionless asset-drain or accounting-conservation bypass was established in this reviewed canonical code. That finding is limited to this review and the listed tests, not a statement that the contracts are comprehensively audited.

### Concrete functional constraint: zkSYS credit still requires native SYS collateral

The final-v0.4 guest validates native balance against the transaction's maximum fee cap plus value **before selecting the gas-tank fee source**. The Syscoin patch does not replace that validation. The server's consistency checker explicitly reproduces it at `/Users/sidhujag/Documents/GitHub/zksync-os-server/lib/revm_consistency_checker/src/syscoin_gas_tank.rs:120-136`; its existing test at `:675-715` asserts rejection even with ample tank credit when native balance is zero or one unit below the bound. The exact pinned upstream implementation is available locally at `/Users/sidhujag/.cargo/git/checkouts/zksync-os-2f0630d2f6d36234/69bc430/basic_bootloader/src/bootloader/transaction_flow/zk/validation_impl.rs:451-463`.

Consequently, an ordinary sender with zkSYS tank credit but no native SYS cannot send a transaction using this path. When a transaction does pass native collateral validation, sufficiently funded tank credit can still fall back to native payment if the additional computational/pubdata reservation does not fit, `scripts/patches/zksync-os-syscoin-v0.4.0.patch:1462-1482`. This is explicitly encoded behavior, not a newly discovered exploit. Wallet/portal estimates and product expectations must reflect it; do not describe ordinary tank funding as eliminating native collateral. Decide any intended change **before** freezing the guest and generating its app-bound key.

For ERC-4337, the relevant sender for this native bootloader path is the outer transaction sender/bundler; this review did not establish how any separate paymaster reimbursements are presented in the wallet. The smart-account subaudit should check that boundary independently.

### Privileged backing and emergency-response boundaries

The tank's own code is immutable, but its token address refers to an upgradeable token. Token admin roles can authorize a burner that burns arbitrary accounts, including the tank (`SyscoinZKSYSToken.sol:55-59`, `:87-89`); the common ProxyAdmin can replace token/issuer/registry/vault implementations. Thus code-hash attestation of the tank does not by itself attest future token backing or economics.

If a privileged burn or incompatible token upgrade reduces tank backing below outstanding credit, canonical funding/withdrawals fail closed. However, guest fee selection checks `totalCredits >= senderCredit` at patch `:1485-1490` and does not query ERC-20 backing on each transaction. This can leave a ledger with nominally usable credit but impaired withdrawal backing until governance repairs the deficit. This is a privileged-compromise/upgrade scenario, not a permissionless attack. Monitor `token.balanceOf(tank) >= tank.totalCredits()` directly: `surplus()` clamps a deficit to zero (`ZkSysGasTank.sol:94-97`) and cannot alone signal insolvency. Monitor token implementation and role changes as well as credit totals.

The tank has no pause/guardian/fee-disable operation; `SYSCOIN_REQUIRE_GAS_TANK` is only a startup presence policy (`node/bin/src/lib.rs:3725-3774`). It does not disable the guest's baked fee path. A tank change requires new app/VK/verifier artifacts according to bootstrap guards (`scripts/gateway-launch/zksys-l2-bootstrap.sh:663-667`, `:721-722`). Before production, document and rehearse what can actually halt or recover fee processing under an incident. Do not assume toggling that variable pauses gas-tank usage. Absence of a pause is a design choice, not itself proof of a vulnerability.

Native staking withdrawals synchronously depend on the weight registry and issuer callback (`ZkSysNativeStakingVault.sol:89`; `ZkSysRewardWeightRegistry.sol:286-297`; `ZkSysIssuer.sol:229-235`). A broken/reverting downstream upgrade can therefore block principal withdrawal until governance repairs the dependent implementation. The canonical code path and local reentrancy tests pass, but a production upgrade runbook must preserve callback compatibility and restore withdrawals. There is no independent emergency withdrawal path in the reviewed vault.

### Remaining gas-tank/contract acceptance gaps

The guest patch includes two useful full-transaction fee-selection tests, `scripts/patches/zksync-os-syscoin-v0.4.0.patch:4907` and `:5014`. Their setup injects tank ledger storage and an empty transaction payload (`:4826-4876`); it does not deploy and call the actual tank bytecode. Combined with the Solidity suites' manually simulated bootloader writes, this leaves an integration seam that must be exercised before release:

1. Deploy the exact pinned token proxy and tank runtime on the final v32 guest and prove real transactions that fund, sponsor, withdraw and burn surplus **inside a tank-paid payload**. Verify balances, credits, `totalCredits`, actual token supply and receipt fee values together.
2. Exercise successful and reverted payloads, exact/insufficient credit, zero/low native collateral, high fee caps, insufficient conditional reserve, sender=coinbase, warm access-list slots, EIP-7702 sender code, and blob-fee behavior where supported. Match the guest, REVM checker and real proof output. Neither arbitrary storage fixtures nor only an ordinary EVM run establish this.
3. Include tank/issuer/token/staking implementation and role attestations in reset verification. Test the actual governance upgrade path and rejected unauthorized upgrades/grants; bootstrap's positive role checks do not enumerate every unexpected pre-existing grant.
4. Reconcile `sum(account credit)` with `totalCredits` from indexed state, `token.balanceOf(tank)` with outstanding credit, total staking balances with vault custody, and issued rewards with the schedule during representative load and restarts. Monitor remaining mint capacity and keeper/distribution freshness.
5. Rehearse loss of the membership keeper, unavailable reward callbacks and a deficient tank backing state in a disposable environment, with an explicit recovery objective. Do not perform those destructive cases on the running testnet.

The local portal source explicitly offers a permissionless `distribute()` action and distinguishes undistributed closed-period rewards, so the issuer's separate distribution step was not treated as a discovered frontend defect. References: `/Users/sidhujag/Documents/GitHub/dapp-portal/composables/zksys/useEarnTransactions.ts:87-91`, `/Users/sidhujag/Documents/GitHub/dapp-portal/store/zksys/earn.ts:424`.
