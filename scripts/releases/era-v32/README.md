# Generated Era sources and separate canonical fixture certification

## Canonical query-bit candidate: offline proof and EVM qualification

The candidate key-generation source pins common crypto
`5eafa1f914519d88a13c577a9e8cc5c7db1dfc77` (tree
`c52391d0df82126d27e5c76810e9df85c61875d0`) for canonical recursive transcript
query-bit constraints and modulus-boundary addition witnesses. The workflow pins the
paired Prover candidate `875975949ba3b856faaf5b0c092b8b81762c947a`; its normal
producer qualification remains pending. The new app-bound VK is
`0x22b4a58c40ab11839bfc79c96957f24711776f3e1992fa3cf14c1c3b898a2153`.
One new compression proof and one dependent zero-knowledge SNARK passed stock and
independent CPU verification, reusing the unchanged RISC proof/VK for the retained
batch-1/batch-2 input. No RISC proof or old performance baseline was rerun.

The generated verifier accepted that actual proof on isolated Anvil with the normal
Solc 0.8.28 / Prague / 9,999,999-run profile. Changed public input and a canonical
scalar-opening mutation both reverted with the exact quotient-evaluation error;
estimated verification gas was 414,006. Normal L1 EVM/EraVM compilation and the full
278-row inventory check changed only PLONK and its embedding Gateway deployer.
The generated tree is `39aba041f59da6733f1562f2dee74ef39fc0c024`; the source tree
retains the merged Gateway priority-limit setter fix.

Independent CPU key-generation comparison and normal producer release builds are
still pending. This does not qualify live deployment, fixture activation or an
overall release. Historical qualification below remains bound to its old source/key.

## Preceding optimized Security100 offline qualification

The preceding app-bound key is
`0x62bd64130fba79809f37a652bc7957b87fa847fde30c4444d91fcf876357f144`.
It binds the preserved guest source tree
`6935489bdbc7b1ed31e608677d1b2418b10691b5`, Security100 continuation commitment
`0xff43db6564b43491d6c25a613270e7802a4fca2bc67ad621e5cb08dd57d22975`,
and the optimized crypto fork `4f1e95bf742ff87fe4ce18acfad4e2a305b94c7a`.
Current CPU/GPU setup keys matched byte-for-byte; the genuine minimal native
batch-1/batch-2 input passed all three stock native stages and separate CPU
verification. Its genuinely serialized proof passed the exact normal-profile
EVM verifier on isolated local Anvil. Changed public input and a valid-field
scalar-opening mutation reverted with `invalid quotient evaluation`.
Estimated verifier gas was 406,832. This is offline qualification of that input,
not live server HTTP, service/DA/settlement, testnet or canonical fixture acceptance.

The base is `8fb7c29a4e3174335c6480b23f57822e054f9d5f`, reviewed source tree
`0cb850a5c77436f17b49e747a1117dac5ee90888`, and generated tree
`d194956535ddeded60829706e9301e9adbe6cb5b`. Four exact paths are overlaid.
A clean normal build of all four contract packages reproduced all 278 baseline
inventory rows. Actual new-key incremental L1 EVM/EraVM builds and full stock
hash recomputation/check changed only PLONK and its embedding Gateway deployer;
the other 276 rows, selectors, FFLONK and genesis stayed exact. The EVM build uses
Solc 0.8.28, Prague and optimizer 9,999,999; EraVM uses EraSolc 0.8.28-1.0.1
and ZkSolc 1.5.11. Only pinned normal compiler identities and default profiles
were used; no mock verification, live deployment or fixture is certified.
Earlier key receipts remain under their historical manifest fields; they are not
relabelled as current qualification. Overall release, live deployment, independent
host reproduction and canonical fixture activation remain separate gates.

### Gateway priority-limit setter follow-up

The setter now accepts the existing chain-local ChainTypeManager on both L1
and Gateway. Its CTM authorization and 16,777,216 ceiling are unchanged; no
other `onlyL1` restriction is removed. This fixes the Gateway `NotL1(57001)`
rejection without granting direct owner/EOA access or changing an on-chain limit.
Three Gateway regressions cover successful storage/event updates, above-cap
rejection without side effects, and unauthorized callers. All 28 focused tests
passed. The normal pinned L1 EVM/EraVM build and full 278-row hash check changed
only AdminFacet; its ABI is unchanged and runtime is 22,482 bytes.

The generated verifier overlay, key and prior proof evidence are byte-identical.
The preceding optimized-key build receipt remains under
`historical_pre_gateway_priority_setter_contract_generation_provenance`; the
current receipt records this Admin-only refresh separately. No proof/key was
regenerated, and no live deployment or storage migration is asserted here.

The bundle is not a live fixture/deployment certificate.
The helper's four-entry `PREIMAGES` binds the source inventory, stock PLONK/key
bytes and absence of the generated source copy; the manifest binds
all four exact postimages. Both the intermediate source tree and final generated
tree are checked, including the ignored generated source copy.

## Historical 2ac3 verifier generation and source-hardening receipts

### Fresh-release priority admission and independent upgrade budget

New priority admission uses `2^24` (16,777,216) gas, including the default chain
setting, CTM setter ceiling, free service requests, Gateway relay wrappers and
ordinary deployment helpers. Genesis and protocol-upgrade transactions retain
a separate 72,000,000 gas allowance, independent of lower or zero user admission
limits. Generic minimum-gas and pubdata checks are retained.

This canonical release has not deployed. No existing-state migration,
getter reinterpretation, queue rewriting, block-budget reduction or new runtime
gas rejection of previously queued work is introduced.

The gas constants live in `PriorityGasLimits.sol`, with every admission/default
and upgrade consumer explicitly importing that module. The untouched vendor
constants in `Config.sol` have no authoritative admission or upgrade consumer.
This keeps the canonical DA relay's complete bytecode, metadata and CREATE2
identity unchanged; no relay repinning or genesis regeneration is needed.

Pinned EVM and zk builds reproduce the 13 affected source inventory rows and
the two generated-verifier baseline rows. Those 13 source rows are refreshed;
the generated overlay retains its existing verifier-generation delta. ABIs
remain unchanged. The exact build evidence has SHA-256
`60e4344a40e31bef505e5de663062cd4b86f90371f65d61deb8bcfc28125b2e3`.
Other package rows, genesis and verifier/key bytes are retained, not newly
regenerated or requalified. This is changed-closure contract qualification, not
full storage-layout, fresh-proof, deployment or public-rollout acceptance.

The complete previous Admin-only receipt is retained unchanged under
`historical_pre_priority_cap_contract_generation_provenance`, bound to source
tree `08bc65200c2813c818573aeae85d74d206e24b77` and generated tree
`3f5f6dc63d130a0382e6c0e11fbb025dea0cfac3`. The following guard evidence is
historical; it is not relabeled as qualification of the priority-cap change.

`permanentlyAllowPriorityMode()` now rejects a nonzero deposit-pause timestamp
with the existing `NotCompatibleWithPriorityMode` error, matching the reverse
migration-pause guard. ABI and storage declarations are unchanged. Regressions
use real priority requests and migration pause/unpause calls, including an empty
queue and failed-opt-in recovery. The stock executor fixture now consistently
selects its already-allowed Syscoin DA scheme and mocks empty Ethereum blob
arrays; these are test-only corrections.

Pinned before/after EVM and zk builds reproduced the prior AdminFacet inventory
row exactly and refreshed its five changed fields. Only AdminFacet bytecodes
changed in the compiled closure; both ABIs matched. Runtime size is 22,490 bytes,
up from 22,432. The targeted result has SHA-256
`1b0865801dd8bd8ca31b4d8459f43bd057076c21363cef98b34847b4aa94d554`.
The other 277 inventory rows, verifier/key and genesis bytes are retained, not
newly regenerated. This is not a full inventory, storage-layout, proof or live
deployment qualification. The previous complete generation receipt remains
unchanged under `historical_pre_paused_guard_contract_generation_provenance`,
bound to its original source tree `264d98e758c3a032942dfb08ee7d87a3f46288b4`
and candidate tree `76af1dc7837a5c1cb1d1098ee75bb3c03a65ffae`.

That historical candidate preserved the reproduced Syscoin guest tree
`6935489bdbc7b1ed31e608677d1b2418b10691b5` and binds the rebuilt proving circuits'
Security100 key
`0x2ac3231439b0ba30b688a78eba0119fdfcf7a8364cf75037606cfb61f92c0b90`.
The recursive guests are the literal canonical-recipe postimages of patched
Airbender tree `98a3e82a726bca322340ec675263a4533857250a`; their new program
commitment is
`0x08e47e4531d0dc3409c5ae1db30b45bfec4b61893c8444f45f80e5c254d5bd94`.
Both generated PLONK copies have SHA-256
`265ff76ec295d3aea569e937ff54d793179d2e44576f842c588969d38f813856`;
the scheduler key has SHA-256
`c3cec62f1b8d47ad23773bcf4906b1779ac1a2be58f9b7ce01f69956c0924a23`.
The generated overlay has SHA-256
`7f670e876b272d6123d227f4eb76a31ab203369019452faac873a23e87afe764`.
Only its inventory pre/postimage Git blob header changed for the Admin refresh;
the verifier/key delta bytes did not change.

The retained pre-guard contract-generation result, SHA-256
`2973ac60ac2d18d3c3aa9f55d43af3b8c89b8ea8118657d0a6bb9701559bd828`,
records four passing native PLONK generator tests, generation from this new key,
the full `recompute_hashes.sh` build/recomputation, successful
`calculate-hashes:check`, byte-identical retained FFLONK reproduction and
byte-identical genesis regeneration. The source/index and submodule identities
were rechecked after generation. This is contracts-only qualification on one
host. It does not assert a fresh FRI/SNARK proof, EVM proof acceptance, real DA or
settlement run, edge/bridge qualification, sustained performance, five-role
snapshot/restore, deployment or public rollout for the new key. No fixture
descriptor or successful restore is manufactured by this change. The generation
receipt binds the fresh keygen result SHA-256
`7020f0c8447da54b15b65e319f958182e319e59df859fac1c04581b8370ae204`.

The retained `historical_2ac3_pending_proof_qualification` was explicitly
`pending_fresh_canonical_proof_qualification`: its result map is empty, proof/EVM
verification flags remain false, and verified-stage/control counts remain zero. Fresh
normal GPU FRI proofs for retained batch 20/21, combination, CPU standard wrapper
and SNARK, all three native verifications, genuine serialization, production EVM
controls and owning server verification were required for that earlier key.
Those unrun requirements are not relabeled
as completed; the distinct current minimal-input qualification is documented above.

`historical_d5bc_contract_generation_provenance` and
`historical_d5bc_proof_qualification` retain the previous d5bc key's generation
and offline proof evidence unchanged. The latter records batch 20/21 FRI proofs,
combination, three native wrapper verifications, genuine serialization, ten EVM
controls and seventeen owning server/type tests for that previous candidate.
Its historical source archive authenticates those tested runtime bytes, not the
current key or auxiliary commitment.

That previous candidate's additional Linux rebuild did not reproduce its macOS
bytes for
`recursion_in_unrolled_layer_security_100_bits.bin` and its `.text`; the cause is
unresolved in that historical receipt and the other three guest pairs were not
compared after that failure. The failed receipt remains historical.

Those canonical-recipe guests passed their separate historical cross-host check:
all twelve `.bin`/`.text`/`.elf` outputs matched byte-for-byte between the Mac
Docker build and an independent Linux Docker host. The old-source control also
reproduced all eight checked `.bin`/`.text` outputs. The successful receipt has
SHA-256 `89935b6c988dc36827060ea33b5fc01fa5123839aba562ad787010d7e916713a`.
This establishes cross-host guest-byte reproduction, not independent-host
key generation or whole-stack qualification.
Current live HTTP submission, service/DA/settlement acceptance,
deployment and canonical fixture acceptance remain unqualified.

Key generation used one host under the explicitly approved validation constraint.
It is not two-independent-host reproduction. The existing production workflow's
identity checks and two-runner reconciliation remain outstanding and are not claimed
to have passed. Those historical workflow results must not be invented, and that
workflow is not a prerequisite imposed by this source-materialization helper.

`scripts/apply-era-contracts-syscoin-release.py --check-bundle ERA_ROOT` is
read-only: it reconstructs both trees using private Git index/object storage and
classifies the current upstream/source/generated worktree. It does not download,
compile, activate, initialize submodules, change the real index, or authorize a
launch. No tool or fixture hashes can be supplied as command-line overrides.

Ordinary application and `--assert-applied` authenticate the final server app/VK
source bytes, exact bundle, complete source/generated trees, and clean initialized
submodules. They do not require a local-chain snapshot before the deployment that
would produce it. Both return `canonical_fixture_authorized: false`; successful
source materialization does not certify deployment, restore or rollout.

The fixed V32 Era/zkstack source pins are source-controlled independently of the
absent fixture's `versions.yaml`. The real launcher still requires its existing
GPU modes without a mock verifier, authenticates live Gateway identities, and the
node checks the deployed production verifier against the compiled V8 VK. The
unchanged source-only helper retains its explicit no-proofs/mock lane and rejects
the generated tree. No operator approval switch or hash override is added. The
zkstack build fingerprint includes the release helper and exact bundle.

`scripts/apply-era-contracts-syscoin-release.py --check-canonical-fixture` is a
distinct read-only check. It fails first on the regeneration marker, then requires
the source-controlled `CANONICAL_BINDING`, which intentionally remains `None`.
Even deleting the marker cannot certify the absent fixture. `run_local.sh` and the
Rust fixture consumers remain blocked with an empty trusted-descriptor registry.

## Canonical fixture binding, only after genuine fixture acceptance

Fill `CANONICAL_BINDING` with exactly this structure (no nulls/placeholders):

```text
descriptor_sha256: SHA256(local-chains/v32.0/l1-backend.json)
versions_sha256: SHA256(local-chains/v32.0/versions.yaml)
identity_sha256:
  source_identities: SHA256(file named by descriptor inventory.source_identities)
  proof_identities: SHA256(file named by descriptor inventory.proof_identities)
  deployment_record: SHA256(file named by descriptor inventory.deployment_record)
  clean_boundary: SHA256(file named by descriptor inventory.clean_boundary)
  snapshot_manifest: SHA256(file named by descriptor inventory.snapshot_manifest)
```

These must come from the accepted completed five-role fixture, never a synthetic
test record. The same descriptor hash must appear once in the consumer's literal
`TRUSTED_DESCRIPTORS` registry for `v32.0`. The helper verifies the real
31337→57001→{6565,6566} topology, five-confirmation policy, all five snapshot and
seven tool roles, all referenced file sizes/hashes, distinct safe paths, and
the pinned final server VK/FRI identity source bytes. This hashes the complete
fixture inventory on each explicit fixture check; there is no untrusted stamp
or cached-success shortcut. Changing app identity source requires renewed review
of `APP_SOURCES`, not an environment override.

Promote that binding, registry entry, complete fixture and marker removal
coherently only after actual stopped-boundary, restore, DA, server-frontier and
proof/settlement qualification. This source interface does not create those
records, interpret synthetic tests as qualification, or assert that private
57001 evidence alone qualifies production root-chain deployment.

The target Era checkout must remain at the exact upstream HEAD, with no staged,
partial or unrelated changes. Application accepts only the upstream tree or
exact reviewed source tree and produces the exact generated tree; assertion
accepts only the generated tree. Already initialized, clean exact submodules
are required. No automatic repair, reset or retry is performed after a partial
failure. The real Git index is not changed. Existing ignored build outputs are
not artifact attestation; normal artifact/build gates remain necessary.

Focused tests use synthetic temporary repositories/fixtures, with an optional
read-only real-bundle reconstruction using `ERA_GENERATED_RELEASE_TEST_ROOT`.
That check reconstructs both exact trees without modifying the real index or
worktree; it does not compile or qualify a proof. No deployment, signing,
original-checkout mutation or fixture registry activation is part of this
source-materialization implementation.

## Priority observation source integration

The reviewed service source already contains the concrete Getters facet's
priority transaction timestamp and tree-height observations, their selectors,
generated inventory row, and regression test. That source patch is unchanged by
this key regeneration. The shared IGetters interface, genesis predeploys and
guest application identity also remain unchanged; the new VK and generated
PLONK/key bytes are the explicit changes in the separate generated overlay.
The source-only inventory and generated overlay remain distinct exact trees;
the overlay still changes only its two permitted verifier inventory rows.

`historical_crypto_validation_provenance` retains the original PR323
source/generated trees and old
`0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe`
key for the existing real-proof, EVM, Cancun and native-build archive records.
Every old proof/artifact hash is nested there, not presented as current evidence.
Those records concern the old key and unchanged crypto bytes subsequently carried
by generated tree `ff5565cd22b61259d6f886e9a0130f788bbdb11c`; they do not qualify
the new key or generated tree. `contract_generation_provenance` records only the
new completed generation checks, while `proof_qualification` records the completed
current-key minimal-input offline native and EVM checks and their scope limits.
The distinct historical d5bc records preserve
their previous results and limits without qualifying the current candidate.
The published offline
critical Gateway input/helper/result under `scripts/keygen/gateway-identity` is
also preserved byte-for-byte at its historical source tree. It is not consumed
as current deployment authorization. Before a fresh service deployment, rebuild
and rederive the full CTM configuration, including the new Getters facet address
and selectors, and recheck the critical timelock/relay identities against those
artifacts. Production identity qualification and canonical fixture regeneration
remain outstanding.
