# Syscoin v32 production readiness audit — 2026-09-21

**Decision: not ready for production. Continue prover qualification, resolve the findings below, then perform the planned fresh v32 testnet reset.** The running v31 testnet is intentional, as confirmed by the operator. Its version is not a defect, and its successful operation does not qualify v32 proving.

This review covers local server revision `87c25cd0`, the pinned Syscoin execution and Era-contract patches, the companion prover, canonical Pali smart accounts, zkSYS token/gas tank/staking/reward contracts, wallet and portal integration, and read-only inspection of the two supplied hosts. Observations were taken on September 21, 2026. The initial audit made no changes outside its report and test artifacts. Subsequently authorized local C1, F1 and R3 work is recorded below; no remote service, transaction, deployment or running testnet was changed.

## Findings and release gates

The table distinguishes a reproduced code issue from incomplete release artifacts, operational gaps and explicit trust assumptions. Priorities indicate what must be resolved for launch; they do not imply every item is an exploitable vulnerability.

| ID | Priority / classification | Evidence and consequence | Required acceptance |
| --- | --- | --- | --- |
| R1 | Blocking: unfinished proving release | Server V8 VK/app/100-bit commitment identities are zero sentinels; the companion prover also has a zero VK. The production Gateway derivation is unattested and canonical fixture regeneration is outstanding. Real proving and canonical E2E deliberately refuse these states. | Freeze and independently reproduce deployment identity; generate app-bound Security100 artifacts; atomically update guest, server, prover, verifier and fixtures; pass real-proof positive and negative settlement tests. |
| C1 | P2: local remediation; deployment acceptance pending | The original unscheduled-consent replay across reinstall is fixed locally by a persistent policy epoch in approvals and operation IDs. Policy replacement clears old guardians and invalidates pending recovery. Pali reads and validates the epoch; canonical guardian bytecode/address changed. | Deploy the fixed module in the fresh v32 release and complete real recovery journeys. The user requested no legacy compatibility. A signed approval deadline remains separate optional hardening; expiration still starts at scheduling. |
| C2 | Native collateral accepted; VM/prover qualification pending | The user explicitly retained the native maximum-fee collateral requirement. Gas-tank credit does not waive it. Insufficient extra computation/pubdata budget can also select native payment despite adequate credit. For ERC-4337, the outer submitter is the bootloader fee payer. | Keep native collateral unchanged. Test actual `handleOps` plus tank bytecode through the final VM/prover and make wallet estimation/fallback behavior agree. |
| C3 | Required economics/operations qualification | Membership/reward weights reflect the last relayed NEVM facts and do not expire. No dedicated membership keeper was identified in the inspected service/container/cron inventories. Missing removals permit stale reward weight until delivery; delayed removal does not claw back minted rewards. | Identify or implement a reconciler, including removals, address changes and seniority updates; verify L2 application, funding, recovery and freshness alerts. Explicitly approve the reward/finality lag policy. |
| C4 | Required governance qualification | Token/ProxyAdmin authority can change implementations or grant arbitrary mint/burn rights; a privileged burn can impair gas-tank backing. The tank is immutable and has no pause. Staking withdrawals depend on registry/issuer callbacks. Verifier owner can rotate the canonical PLONK implementation. | Attest all actual roles/owners and upgrade powers, remove bootstrap access, enforce intended multisig/timelock controls and rehearse incident/upgrade recovery. Monitor actual tank backing, not only `surplus()`. |
| F1 | P1 release requirement: watcher finality/trust boundary | Geth [PR #42](https://github.com/syscoin/go-ethereum/pull/42) supplies Core-published finality to `safe`/`finalized`. Local hardening authenticates log ranges and stops on changed consumed history within a watcher session. Deposit ingestion retains its existing confirmation policy, also used upstream; no mandatory finalized gate is included. Checkpoints are in memory and already executed L2 effects are not automatically rolled back. | Include PR #42 and its paired Core transport. Qualify restart/reorg recovery and explicitly accept the confirmation/operator trust policy, or select a finalized input boundary. Do not describe detection as coordinated rollback. This does not replace C3. |
| O1 | P1 operations gap | Live Gateway and edge configs expose `replay_archive_type=Noop`; both sequencer processes run in login-session scopes. No node systemd unit, independent replay archive, or tested restore/failover evidence was found. Raft does not make the single batcher/proof/settlement process highly available. | Configure independent encrypted replay archives and supervised services. Rehearse clean-host restore, crash recovery and settlement-owner takeover with measured RPO/RTO and nonce fencing. |
| O2 | P2 confirmed exposure | Explorer host publicly serves metrics on 3314/3315 and a direct plaintext bridge backend on 8080; 8000 serves a webfs listing. No secret leak is asserted. | Restrict metrics to the monitoring network and bind backend services behind the intended proxy. Review/remove webfs ingress. Verify from outside after changes. |
| R2 | Required coordinated reset | Portal, wallet and explorers are configured for the present deployment. Portal transaction history is persisted under a stable network key, and token registry responses can remain cached for an hour. Reusing chain/account identities also reuses standard signing domains after a reset. | Publish one versioned v32 manifest; replace all addresses/hashes/ABIs; coordinate node/indexer state and frontend caches/history; explicitly manage old approvals, pending operations and any reused signing domains. |
| R3 | Local usage-history fix; cryptographic qualification still required | Pali uses the limited-signature SLH-DSA-SHA2-128-24 draft profile. Local remediation preserves known usage counts through signer regeneration and stale hydration, and rejects corrupt history. Seed-only restore, stale backups and separate devices still cannot establish the key's global lifetime usage. GPU/guest/deployed Solidity equivalence remains unqualified. | Run independent conformance and real-v32 validator/prover cases. Establish the supported recovery/device model and lifetime budget, including crash/cancellation behavior. Preserve accurate draft-status disclosures. |

Detailed evidence and references:

- [Smart accounts, guardian finding and wallet artifact parity](smart-accounts.md).
- [Proving identity, zkSYS contracts, gas tank and reward economics](proving-and-zksys.md).
- [Patched ZKsync contracts and 111 focused tests](zksync-contracts.md).
- [Consensus, finality and recovery](consensus.md).
- [Membership keeper design and acceptance](membership-keeper.md).
- [SLH signer lifetime accounting follow-up](slh-lifetime.md).
- [Sequencer host evidence](sequencer-operations.md).
- [Explorer/portal host and reset coordination](portal-operations.md).

## Smart-contract conclusion

The reviewed canonical paths have useful defenses: factory-controlled atomic initialization, active-validator enforcement and domain-bound signatures; exact gas-tank token deltas and reentrancy guards; bounded DA envelopes, authenticated relay callers and ordered commitment binding; canonical verifier slot 8 with production mock-proof rejection; and restricted permanent-rollup DA pairs. The local tests support those specific behaviors.

The guardian-consent lifecycle fix is prepared locally for the fresh v32 deployment; the live module has not changed. No permissionless zkSYS accounting drain or production verifier-router bypass was established in this bounded review. That does not establish safety of the final proving circuit, every inherited ZKsync contract, every privileged upgrade, or a deployment that has not yet been generated and attested. The detailed reports separate those unverified boundaries.

## Executed validation

The table records the original audit baseline. The C1 implementation follow-up
in [smart-accounts.md](smart-accounts.md#implementation-follow-up-validation)
records validation of the updated contract and wallet; it supersedes the
original stale-consent observation test.

| Surface | Result | Important limit |
| --- | --- | --- |
| Launch/recovery Python tooling | 149 tests run, 144 passed, 5 skipped | Four PyYAML-dependent tests and one Linux PTY/session test were unavailable in this local environment. Six separately run gate tests overlap this suite and are not added again. |
| Fresh patched Rust source | L1 watcher 27/27; Raft 23/23 | Built with the required wrapper; does not include the proposed consumed-history reorg test or full workspace/integration coverage. |
| zkSYS Solidity | 118/118 | Includes token, gas tank, issuer, membership, staking, weights and L1 registry bridge; NEVM/bootloader boundaries are mocked. |
| Pali Solidity | 69 distinct existing tests passed | 60 in the initial run; nine in two suites rerun under Cancun because current Forge rejects `vm.etch(0x100)` in the initial fork. Real precompiles remain a release test. |
| Pali defensive regression | 1 local reproduction passed | Confirms the stale-consent finding; passing this reproduction is evidence of the issue, not a security pass. |
| Patched ZKsync Solidity | 111 distinct tests passed | Exact reviewed Era source tree; ordinary EVM unit tests. Real final V8 proof generation/verification remains blocked. |
| Wallet | 58/58 focused tests | Mocked signature/execution/recovery/gas helpers. |
| Portal | 41/41 tests | Mocked bridge, explorer and earn flows. |
| Wallet release parity | Eight creation bytecodes + one SLH runtime hash matched | Requires Solc 0.8.28, optimizer 200, viaIR, Cancun and no metadata; not a deployed-bytecode attestation. |
| Formatting | `cargo fmt --all -- --check` passed | No application source edits. |

Subsequent local remediation validation:

- C1: 79 canonical Pali contract tests, the updated stale-consent regression and 114 focused wallet tests passed; artifact parity and remaining deployment acceptance are recorded in [smart-accounts.md](smart-accounts.md#implementation-follow-up-validation).
- F1: the patched-source watcher suite now passes **38/38 tests**, including eleven new reorg/range/retry regressions. Deposit confirmations remain unchanged. See [consensus.md](consensus.md#local-follow-up-confirmation-policy-retained-consumed-history-authenticated) for the restart and rollback limits.
- R3: **41/41 SLH tests** passed, including regeneration, persistence and lock/unlock regressions. The combined guardian/smart-account/SLH run passed **155/155 tests**; full TypeScript and targeted lint also passed. See [slh-lifetime.md](slh-lifetime.md) for changed files, commands and the remaining restore/device limits.

## Ordered plan: prover qualification → v32 testnet reset → production

1. **Resolve behavior and identity before keygen.** Include the local C1 fix and retained C2 native-collateral policy; freeze EntryPoint/account/module, token/tank, Gateway target/relay/factory and governance inputs. Independently derive the complete CREATE2 postimages and compare both address and runtime hashes. Account-only fixes need new wallet artifacts; any guest-baked tank/relay/target change also needs a new app and key.
2. **Generate and attest the final proving release.** Complete the production derivation record, reproducible application builds, Security100 key generation and stock/wrong-app controls. Update every server/prover/contract identity and regenerate the full fixture atomically. Preserve the sentinel until the complete generated set exists. Do not repurpose a v31 fixture or simply toggle a retained chain from fake to real proofs.
3. **Qualify real proofs in an isolated environment.** Verify valid single/multi-batch proofs on the actual final verifier. Reject wrong app/VK, wrong public inputs/chain IDs, modified state/DA root, malformed proof header, and fake type-3 proofs. Record proof and verifier gas/latency/memory under ordinary and worst-case workloads; test lease expiry, prover restart and backlog recovery.
4. **Run actual cross-layer contract journeys.** Exercise native and ERC20 deposit/withdrawal/finalization, failed-deposit recovery, duplicate claim rejection, malformed/unavailable/expired DA and final-L1 republishing. Exercise real `handleOps` for every validator, first deployment, recovery/rotation/cancellation and failed operations. Execute tank fund/withdraw/burn from real VM payloads, including reverts, exact/insufficient credit, low native balance, access lists, sender=coinbase and actual outer payer accounting. Check reward removals and stake withdrawals across keeper outages.
5. **Prepare the reset and production operating model.** Establish keeper ownership, archives, supervision, monitoring, finality policy, batcher takeover, governance handoff, capacity and maintenance plans. Alert on committed/proven/executed lag, oldest pending proof, DA retention/finality, replay-archive freshness, member-update lag, token roles/implementations, tank backing, signer funds and TLS expiry. Benchmark mature-chain Raft recovery because snapshots/purging are disabled.
6. **Perform the operator-approved testnet reset as one release.** Publish source/build/config/contract manifest and new deployment identity; archive the old environment; reset Gateway, edge, ENs, explorers and browser-facing deployment state coherently. Attest deployed bytecodes, verifier marker/VK, native token/tank, bridge routing and admin powers. Repeat the real-proof journeys against the reset testnet and require agreement among sequencer, independent ENs and explorer.
7. **Approve production only against recorded acceptance evidence.** Complete representative load/soak, host-loss, backup-restore, finality/reorg, key rotation and governance incident rehearsals. Agree measurable latency, backlog, disk, keeper-freshness and recovery thresholds before running them. Do not substitute container uptime or mock-proof batch counters for these results.

## Limits of this audit

No live writes, load/fault injection, testnet reset, keygen, GPU proving, live fund movement, restore drill or privileged firewall inspection was performed. `sudo -n` required a password on both hosts; firewall conclusions are limited to observed external reachability. Ownership thresholds and production custody were not verified. Full workspace lint/tests and canonical E2E were not run. Existing CI intentionally refuses canonical fixture E2E while regeneration is pending.

The SLH qualification is consistent with [NIST's current SP 800-230 Initial Public Draft](https://csrc.nist.gov/pubs/sp/800/230/ipd), which limits these parameter sets to `2^24` signatures per key and does not approve them for general-purpose use. This audit does not replace independent cryptographic or whole-system security review of the final immutable release.
