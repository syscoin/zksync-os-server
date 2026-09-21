# Patched ZKsync contract review

Reviewed the source patch `scripts/patches/era-contracts-syscoin.patch` at server revision `87c25cd0`, including compact direct DA, Gateway relay, final settlement parser/public inputs, verifier routing, DA governance, chain configuration and deployment identity. This is a focused review of the Syscoin integration and exercised inherited paths, not a line-by-line audit of all upstream Era contracts.

## Exact test source

An isolated clone of the locally available Era-contracts Git object database was checked out at `8fb7c29a4e3174335c6480b23f57822e054f9d5f`. The official repository helper applied the current patch using the explicitly allowed `no-proofs`/testnet source-materialization gate. It verified the exact patched tree `2a28a08e439d35ff25643d3d108c05e846cdf0fe`. No live deployment or canonical fixture was created.

Temporary test root: `/var/folders/vl/5jg336cs02qcpjs6y7yh37p40000gn/T/syscoin-v32-contracts-audit-r7rmu2p4`. Solc 0.8.28 and the pinned upstream dependency graph were used. The sibling working checkout under `zksync-era/contracts` was an older, modified v31 tree and was deliberately not used as the v32 source under test.

## Controls checked

- `SyscoinL1DAValidatorZKsyncOS` accepts a nonempty sequence of complete 32-byte references, bounds it to 32, calls NEVM's raw `0x63` precompile with bounded gas, requires exactly the reference back, and checks the ordered-reference commitment. Patch lines 1202–1278; tests include empty/partial/oversized/mismatched/unavailable/reverting/wrong-result inputs.
- `SyscoinRelayedSLDAValidator` authenticates the calling edge diamond through canonical Gateway Bridgehub registration, bounds references and checks the commitment before sending a versioned chain/batch message. Patch lines 1905–1978. The guest separately binds the canonical relay identity; deployment tests reject absent or changed relay runtime.
- `Committer` parses a canonical envelope, checks per-message and aggregate 32-reference limits, opens the ordered root, rechecks every reference on final L1 and includes the edge DA root in batch output. Empty edge input is allowed only with its zero root. Patch lines 1550–1780. Unit tests reject malformed envelopes, incorrect commitment/root/chain/settlement identity, and DA precompile failures.
- `ZKsyncOSDualVerifier` routes only PLONK type 2 at canonical version 8, rejects mock type 3 in production and leaves the advertised FFLONK compatibility artifact unreachable. Constructor/rotation reject zero/non-contract verifiers; rotation is owner-only and atomic. Patch lines 2029–2154. `ZKsyncOSTestnetVerifier` checks both execution-chain and ultimate root-chain identity and rejects roots 1 and 57; tests cover Gateway deployment paths.
- `SyscoinRollupDAManager` only enables/disables one immutable validator/scheme pair. Permanent-rollup tests reject incompatible DA changes, including KZG. The canonical max-transaction-gas setting can only be persisted idempotently, matching the fixed guest. Patch lines 1979–2028 and 1531–1549.
- Public-input tests validate the golden vector, chain configuration binding, single-batch behavior and hashing concatenated per-batch values once. This validates the Solidity encoding vectors, not the yet-ungenerated final proving key.

No new permissionless router or DA commitment bypass was established by this review. All privileged authority and real-proof qualifications below remain necessary.

## Governance and integration limits

1. `replaceVerifier()` only requires owner authorization and contract code; it does not cryptographically restrict the owner to an honest verifier. Ownership/timelock/custody and observed implementation/VK changes are therefore part of settlement security. Source-level mock rejection does not remove upgrade governance trust.
2. The launch guide explicitly leaves `isPermanentRollup=false`. Before permanence, the admin can change DA pairs without the restrictive permanent-rollup allowlist. Production must attest this governance choice; permanence is irreversible and must follow real-proof/migration validation. See `docs/src/guides/gateway_launch.md:315`.
3. The 30-day maximum commit age is a catch-up bound, not DA retention or a target settlement SLA. Actual reference availability, republishing and finality must be exercised against Syscoin, including outages approaching retention boundaries. See `docs/src/guides/gateway_launch.md:308`.
4. Successful mock precompile/verifier tests do not qualify the real NEVM implementation, patched guest output or app-bound proof. Require actual final-v32 positive and negative proof/DA vectors and native/ERC20 bridge journeys before production.
5. The production target/relay CREATE2 derivation, owners, compiler inputs and Security100 app/VK remain unfinished. The existing candidate identity and pinned source are useful test inputs, not production attestation.

## Tests executed: 111 distinct passes

| Suite | Passed |
| --- | ---: |
| ZKsyncOSDualVerifier | 40 |
| SyscoinRelayedSLDAValidator | 6 |
| SyscoinRollupDAManager | 6 |
| MakePermanentRollup | 8 |
| SetZKsyncOSChainConfig | 7 |
| GatewayCTMDeployerZKsyncOS | 5 |
| CommittingZKsyncOS / CommittingTest | 22 |
| SyscoinL1DAValidatorZKsyncOS | 8 |
| ZKsyncOSPublicInput | 5 |
| SyscoinGatewayCTMDeployerDA | 4 |
| Total | 111 |

The initial Gateway deployer run had one missing `projectRoot/out` artifact because output was redirected to `/tmp`; the Committer constructor similarly required DA artifacts. The isolated workspace's `l1-contracts/out` was linked to its isolated test output and the DA package was compiled. The affected suites then passed without source changes. These were harness preparation failures, not ignored contract assertions. Three selected fuzz tests each ran 256 cases.

Commands, with `ERA_AUDIT_ROOT` set to the isolated, attested source root:

```sh
FOUNDRY_TEST=test/foundry/l1/unit forge test --root "$ERA_AUDIT_ROOT/l1-contracts" --offline \
  --match-contract 'ZKsyncOSDualVerifier|SyscoinRelayedSLDAValidator|SyscoinRollupDAManager|SetZKsyncOSChainConfig|MakePermanentRollup|GatewayCTMDeployerZKsyncOS' \
  --out /tmp/syscoin-v32-era-forge-out --cache-path /tmp/syscoin-v32-era-forge-cache --summary
forge test --root "$ERA_AUDIT_ROOT/da-contracts" --offline \
  --match-contract SyscoinL1DAValidatorZKsyncOS --summary
FOUNDRY_TEST=test/foundry/l1/unit forge test --root "$ERA_AUDIT_ROOT/l1-contracts" --offline \
  --match-path '**/CommittingZKsyncOS.t.sol' \
  --out /tmp/syscoin-v32-era-forge-out --cache-path /tmp/syscoin-v32-era-forge-cache --summary
FOUNDRY_TEST=test/foundry/l1/unit forge test --root "$ERA_AUDIT_ROOT/l1-contracts" --offline \
  --match-path '**/ZKsyncOSPublicInput.t.sol' \
  --out /tmp/syscoin-v32-era-forge-out --cache-path /tmp/syscoin-v32-era-forge-cache --summary
FOUNDRY_TEST=test/foundry/l2/unit forge test --root "$ERA_AUDIT_ROOT/l1-contracts" --offline \
  --match-path '**/SyscoinGatewayCTMDeployerDA.t.sol' \
  --out /tmp/syscoin-v32-era-forge-out --cache-path /tmp/syscoin-v32-era-forge-cache --summary
```

Logs were recorded under `/tmp/syscoin-v32-era-{forge,commit-tests,deployer-tests,da-tests,input-tests,l2-da-tests}.log`. New dependencies and test/build artifacts remained in disposable paths. No source or deployed contracts were modified.
