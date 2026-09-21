# R3: SLH-DSA lifetime accounting follow-up

Date: 2026-09-21. Status: local regeneration and stale-session defects fixed; global lifetime accounting remains open. This follows the SLH-DSA qualification requirement in [smart-accounts.md](smart-accounts.md). Work changed Pali wallet utilities, the SLH hydration/signing paths in MainController, their tests and two English guides. No canonical contracts, deployment bytecode, servers or live wallet data were changed by this follow-up.

## Baseline finding and scope

The initial P2 finding was that regenerating a previously used SLH-DSA key replaced its known local signature count with zero. The installed `@sidhujag/sysweb3-keyring` 1.0.610 derives the same setup secret from the same mnemonic and account index (`cjs/keyring-manager.js`, `deriveSLHDSASetupSecretForAccount`). Wallet provisioning rebuilt that same key but registered a newly initialized counter without preserving existing history. Settings exposes regeneration as an ordinary user action.

That violated the intended lifetime-usage bound. It did not demonstrate signature forgery, immediate asset theft or unauthorized signing. Practical impact depends on aggregate signing volume; an ordinary user is unlikely to approach the bound. Restore and cloning expose the same accounting problem without requiring a malicious storage edit.

## Completed local fix

In the `pali-wallet` repository:

- `source/utils/slhDsa/signer.ts`, `registerRuntimeSLHDSAState`: same-key registration and signing use the same queue. Registration decrypts the existing record, merges the highest candidate/persisted/runtime count, and persists before publishing the refreshed signer. Rebuilding public caches or local key material no longer resets available usage history.
- `source/utils/slhDsa/state.ts`, `validateSLHDSAProvisionedState` and `loadEncryptedSLHDSAState`: unreadable state, invalid counts, changed limits, unsupported versions and mismatched identities fail closed. Registration also rejects conflicting key material. Failed validation does not overwrite the saved record.
- `source/utils/slhDsa/signer.ts`, `putRuntimeSLHDSAState`: delayed hydration preserves the highest runtime count. Signature completion preserves an updated runtime count, and returned state cannot mutate the internal counter by reference.
- `source/utils/slhDsa/state.ts`, `saveEncryptedSLHDSAState`: storage readback checks the ciphertext actually written, in addition to key identity and version. Signing awaits persistence before returning a signature; a failed write rejects the call and retains the increased runtime count.
- Session guards cover `signer.ts`, `offscreenClient.ts`, `smartAccountSetup.ts` and the two SLH hydration/retry paths in `source/scripts/Background/controllers/MainController.ts`. Lock/clear invalidates the captured generation. Old work cannot publish runtime keys, dispatch secret material after awaiting offscreen setup, or release a completed signature through the checked signing path. Pending per-key queues survive clear so an old storage write completes before a new session writes the same key.

These changes preserve locally known history. They do not introduce a shared counter, a new key derivation scheme or a recovery policy for unknown history.

Review of the initial fix found a concrete lock regression: registration persisted first and then published runtime state without checking whether the wallet had locked during the write/readback. An isolated harness reproduced key resurrection and a subsequent worker signing attempt after clear; the signature was not returned because persistence then failed. The generation guards close that path. The regression test pauses registration readback, clears and configures a new session, resumes the old read, and verifies rejection plus zero subsequent worker calls. Additional tests cover late key preparation, queued/in-flight signing, delayed hydration, encryption and secret derivation. Normal same-session signing and regeneration remain supported.

## Completed validation

The SLH suites passed **41/41 tests**: six existing tests and thirty-five new regressions. Coverage includes ordinary and exhausted same-key regeneration after runtime restart, serialization with an in-flight signature, delayed hydration, signing persistence failure, runtime count preservation, corrupt counts/limits/identity, unreadable encryption, conflicting key material, stale storage readback, returned-object isolation and the session transitions above. Eight tests exercise the actual offscreen-client functions with mocked Chrome APIs: lock during context lookup or document creation prevents dispatch; late worker responses are rejected; current-session signing and preparation return normally.

The combined smart-account and SLH run passed **155/155 tests across 13 suites**, including the guardian remediation. Full TypeScript checking and targeted ESLint passed. An independent read-only reviewer traced the session boundary; a fresh final candidate reviewer then checked bypass and compatibility paths. Neither reported a concrete surviving bypass or legitimate workflow regression in the scoped changes.

Executed from `pali-wallet`:

```sh
NODE_ENV=test ./node_modules/.bin/jest source/utils/slhDsa source/utils/smartAccount source/scripts/Background/controllers/smartAccount --runInBand --coverage=false --cacheDirectory=/tmp/v32-pali-slh-jest-cache
./node_modules/.bin/tsc --noEmit --pretty false
./node_modules/.bin/eslint source/utils/slhDsa/signer.ts source/utils/slhDsa/state.ts source/utils/slhDsa/lifetime.spec.ts source/utils/slhDsa/offscreenClient.ts source/utils/slhDsa/offscreenClient.spec.ts source/utils/slhDsa/smartAccountSetup.ts source/scripts/Background/controllers/MainController.ts
git diff --check
```

All completed successfully. Syntax/type checks preceded the focused trigger/control verification; formatting-only lint failures were corrected, and the final lint passed. Tests mock Chrome storage and worker responses; they establish the tested state transitions, not browser crash durability or WASM cryptographic conformance. No live transaction or expensive key-generation run was performed in this follow-up.

## Remaining production requirements

**Unknown and shared history:** seed-only restoration reconstructs the same key without its usage history. Wallet import explicitly deletes local SLH state (`MainController.ts`, `resetWalletState` and `createWallet`). An old profile restores an old encrypted count; separate installations count independently. Encryption provides no global monotonic history. The local merge cannot enforce a bound across those cases.

The release design must define how trustworthy lifetime usage is recovered and coordinated, or how recovery moves to a distinct key when that history is unknown. A new local generation field alone does not solve rollback or cloning. An alternative parameter set requires its own cryptographic, performance and deployment assessment; this follow-up selects no such design.

**Crash and cancellation evidence:** process termination, power-loss durability at the storage acknowledgement boundary and actual browser worker cancellation still need qualification. The completed tests cover local signing/provisioning serialization, stale hydration and the specified lock/unlock interleavings; those cases are no longer merely untested concerns. Require browser-level fault injection for the remaining boundaries, including whether uncertain work conservatively consumes usage before another signing attempt. An in-flight Chrome storage write cannot be revoked; queue ordering prevents it from overwriting a newer same-key write within the process. This audit has not demonstrated an uncounted signature escaping through a crash.

**All signing uses count:** the canonical [SLH-DSA validator](../../../contracts/src/pali/PaliSLHDSAValidatorModule.sol) verifies signatures through view functions and does not maintain usage. Chain transaction nonces cannot reconstruct off-chain, failed-operation, ERC-1271, cross-account or cross-chain signing history. Acceptance must cover every use and copy of a signing key, not just successful UserOperations on one chain.

## Parameter-set qualification

The implemented constant is `SLH-DSA-SHA2-128-24`. It is the limited-signature profile proposed in [NIST SP 800-230, still an initial public draft when checked on 2026-09-21](https://csrc.nist.gov/pubs/sp/800/230/ipd). The strict limit is `2^24` signatures over a key's lifetime, and the profile is not approved for general-purpose use. The wallet reserves 1,000 of those signatures for rotation attempts. The [contract documentation](../../../contracts/README.md#slh-dsa-sha2-128-24-verifier-status) already describes this qualification.

The English wallet guides `docs/portal/docs/developers/slh-dsa-smart-accounts.md` and `docs/portal/docs/users/post-quantum-signer.md` now identify this actual profile, distinguish it from the FIPS-205 general-purpose parameter sets and explain the local counter's restore/device limitations. Their previous `SHA2-128s` qualification was inaccurate. Other language editions were outside this documentation edit.

R3 cannot be marked fully closed on the local fix and unit tests alone. Unknown/shared lifetime history and the remaining durability evidence still need release acceptance.
