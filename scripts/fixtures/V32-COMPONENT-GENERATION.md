# Genuine V32/V8 component fixture generation

This records the previous `c1ab3d65…b7388fe` mock deployment and its original
generation recipe. It is not a recipe or qualification for the regenerated
Security100 circuits or the active production key. Preserve the package's actual
old identities; a new-key fixture requires genuine regeneration and separate review.

This lane is **AnvilComponentOnly**. It does not qualify Syscoin Core consensus,
NEVM/Bitcoin DA finality, real FRI/SNARK proofs, or the canonical five-role release
fixture. The canonical marker and trusted canonical descriptor registry remain
unchanged. Neither stock V30/V31 fixtures nor previously private task databases
are inputs. All 110 original live integration cases remain selected.

There are three physical L2 chains: Gateway57001 settles directly on Anvil31337;
edges6565 and6566 settle on Gateway. V32's compact-DA topology does not admit an
unrelated chain12345 settling directly on Root. Accordingly, `default/config.yaml`
is an explicitly recorded direct-L1 test view of the genuine Gateway57001
deployment, and `multi_chain/chain_57001.yaml` is its supporting-Gateway view.
These views select different ports in independently instantiated test worlds;
they do not create two simultaneous producers or claim a fourth deployment.
The package contains one actual replay archive per physical chain and records
the view-to-chain mapping in its deployment evidence.

The newly created localhost chain31337 explicitly installs a source-built,
stateless component-only mock at the otherwise empty native DA address0x63.
It assumes availability for **all** exact32-byte identifiers and reverts for
other lengths. A real caller probes the unchanged1400-gas allowance, including
two positive and empty/31/33-byte negative controls. Exact code survives the
actual dump/restore gate. This is not Bitcoin availability, finality or canonical
release qualification; production contracts, guests and VKs are unchanged.
This sole narrowly scoped code-install exception admits no other code/storage
override and no historical task chain. HTTP BitcoinDaMock remains distinct.

## Actual prerequisites, not inherited successes

The source base is server `64665f1139eaab0ec9874dfe99854188fd0f7eab` plus the
exact reviewed dirty source packet. The accepted genesis is unchanged SHA256
`5adf0dd1b618911d51c335e983c0c71cc1c74fc7db37161bf76a4b51e5055a95`;
the accepted VK is unchanged
`0xc1ab3d6506620ad299672c2c2530e8732ac7bae55cdb9d8cf1fa12355b7388fe`.
There is no VK, guest, setup, or proof regeneration.

Before an execution plan can be issued, independently qualify:

1. A **fresh isolated** current zkstack source tree, base postimage
   `2e4eed988d0014be40a7fcbdc0d9920229bfb56e`, with ONLY the reviewed
   `zkstack-anvil-component.patch`. Build its CLI using the genuine current lock
   and original source graph. The retained native `c9fef4fc…` CLI/cache is a
   possible cache input, not the new adapted executable. Never patch that old
   binary or reuse its successful-build claim for this source.
2. Fresh current-source native executables `anvil-component-bootstrap` from
   package `zksync_os_integration_tests` and `generate-deposit` from package
   `zksync_os_generate_deposit`, using `scripts/cargo-with-patched-zksync-os.sh`
   and the unchanged release guest graph. The normal production server binary
   remains unmodified; component bootstrap is a separate explicitly named bin.
3. A full isolated current Era workspace containing `contracts/l1-contracts`,
   current accepted release/applicator postimages (retained `9b4ff94` context),
   and actual compiled deployment/register/admin/Gateway/verifier artifacts.
   Contract preparation must use the existing frozen verifier source and flags,
   not generate a new VK. The generator uses the supported
   `--skip-contract-compilation-override` **only after** those actual artifacts
   are bound; it is not permission to skip source/artifact qualification.
4. Held single-link tools: owned private copies of Foundry Forge/Cast/Anvil, and
   owned or root-owned non-writable executable Python (with PyYAML installed)
   Node and Zstandard under trusted non-writable parents. This tool-only system-binary
   exception does not relax ownership of source, artifact or output files.
   Node is actually needed by DeployCTM's Blake2s FFI.
   Do not assume an old generic tool attestation also qualifies this invocation.
   The separate relay build keeps the actual Prague override; other artifacts
   retain their current Cancun policy. No user HOME is overridden; no inherited
   signing environment, generated wallet file, or remote RPC is admitted.

Typical **review-only** build entrypoints, with unique owned labels and approved
absolute source/cache/output paths bound first:

```sh
# Current isolated zkstack workspace, AFTER exact patch/preimages are reviewed:
cargo build --locked --release -p zkstack --bin zkstack
# Current isolated server workspace; each label/output is unique:
bash scripts/cargo-with-patched-zksync-os.sh component-bootstrap -- build --locked --release -p zksync_os_integration_tests --bin anvil-component-bootstrap
bash scripts/cargo-with-patched-zksync-os.sh component-deposit -- build --locked --release -p zksync_os_generate_deposit --bin zksync_os_generate_deposit
```

These commands are not an authorization, an actual build result, or a promised
duration. Existing caches may reduce work; actual build/compiler/lock/input and
ELF/ABI results must be recorded before generation. If Cargo produces hardlinked
build artifacts, qualify fresh owned create-only copies; do not waive nlink.

## One finite reviewed generation

`v32-component-generation.plan.inert.json` is deliberately unexecutable. Root
fills exact absolute held `{path,size,sha256}` refs for twelve tools (including
the accepted native Solc 0.8.28 via `FOUNDRY_SOLC`, and distinct owned EraVM
Solc 0.8.28-1.0.1 / ZkSolc 1.5.11 via the actual Forge revision's inline
`FOUNDRY_ZKSYNC` dictionary, and the lossless Zstandard compressor), accepted
genesis and a public source/build attestation, four unique unused loopback ports,
new disjoint output paths under owned 0700 parents, and a fresh deadline at most
3600 seconds after admission. The source attestation has exact fields:

```text
schema = syscoin-v32-component-tool-build-v1
scope = AnvilComponentOnly
zkstack_base_tree = 2e4eed988d0014be40a7fcbdc0d9920229bfb56e
server_revision = 64665f1139eaab0ec9874dfe99854188fd0f7eab
adapter_sha256; tools; source_files; contract_artifacts; era_root
```

It binds actual adapted CLI/common Forge sources, current deployment/CTM/Gateway/
SyscoinConfig sources, current component bootstrap sources, and actual compiled
contract artifact refs. Minimum counts do not substitute for Root's complete
source/artifact/build review. Roots and materialized files must be actual current
physical origins; no module/source aliases or private config bodies are exported.

Root reviews the final bound data and current owned-process/port/resource absence,
then may issue `execute:true` and invoke the exact generator once:

```sh
ABS_REVIEWED_PYTHON scripts/fixtures/generate-v32-component-fixture.py ABS_ACTUAL_PLAN ACTUAL_PLAN_SHA256
```

The generator performs real fresh Anvil deployments and migrations using the
current CLI and original scripts. Every runtime tool is admitted individually;
the generator passes the existing `--ignore-prerequisites` option because the
legacy bundle also demands unused Docker and Era-VM tooling. Normal CLI checks
remain unchanged. The component adapter accepts only an explicit, canonical,
fully vendored source tree under scope 31337, without fabricating Git metadata
or updating submodules. A localhost-only unlocked-sender adapter keeps
fixed public ecosystem governance/admin `0x622a…` without possessing its key.
CLI deployment and ordinary chain governance use public insecure scalar-3.
Gateway Root commit/prove/execute use scalars 1/7/2; edge6565 uses 1/f39/2 on
Gateway, and edge6566 uses 4/5/6 on Gateway. These are separate nonce domains:
the ordinary integration tests write Root transactions from f39 while proving
continues, so the Root prove signer cannot also be f39. Concurrent edges must
likewise have separate sender accounts on Gateway. Each chain's deployment
`operator` and `blob_operator` match its actual commit signer, which migration
enables and funds. Normal deployment/migration supplies operator authorization;
any additional f39 REVERTER_ROLE grant uses the real owner-authorized contract
API, including the Root-admin priority route for a migrated edge. No alias
impersonation, validator storage patch or nonce-error suppression is used.
Actual owner, role, funding, status-1 receipt and canonical-block readbacks are
required. All three actual chain-scoped timelock checks must pass
`hasRoleForChainId(chainId, REVERTER_ROLE, f39)`.
Public development balance funding and Anvil impersonation are allowed. The
sole code-install exception is the source-built strict32-byte DA mock at the
fresh Root Anvil's empty 0x63, with actual caller-side 1400-gas controls repeated
after snapshot restore. No other code/storage override, old-state import,
payload replacement, or private signer output is allowed.

Every chain's actual V32 protocol and deployed accepted VK/testnet-verifier getter
are checked. Gateway's exact fixed timelock and relay runtime bytes must be actual
deployments. Both edges use normal migration/finalization/unpause. Real priority
deposits fund the source-public test wallet. Bootstrap performs normal
`Config::schema` loading, **ConfigValidate**, then normal `server::run`, with the
explicit integration-test HTTP `BitcoinDaMock`, the scoped EVM 0x63 availability
mock, and fake-proof topology. This is real startup/RPC/transaction coverage, not production Core/DA
or real-proof coverage.

Fresh owned nodes must stop cooperatively before the actual L1 snapshot is taken.
After those writers drain, the explicitly named bootstrap's `export-replay`
command selects each **new component chain's** canonical stopped WAL anchor and
exports its public Noop archive objects, not a database. Three strict manifests
cover contiguous blocks 0 through each anchor; the replay payload is bounded
to 4096 objects and 2 GiB. The descriptor binds every object SHA/size and anchor.
Each seed node's original public `database_identity.json` is also authenticated
in the package and copied unchanged, create-only, beside its fresh recovered WAL.
Normal startup still derives the deployment identity from the actual chain and
compares every field; recovery does not synthesize or bypass that marker.
The Anvil snapshot retains historical account/storage states, which normal node
discovery needs for historical contract-code and migration queries. Its separate
limits are 64 MiB packaged and 2 GiB decoded. These are packaging ceilings,
not promises about CI memory use. The original Anvil gzip dump is retained before
decoding so a size failure can be diagnosed without deploying again. The package
uses `l1-state.json.zst`: lossless long-window Zstandard compression preserves
the exact decoded JSON bytes and history, while avoiding gzip's poor compression
of repeated historical state. Both compressed and decoded identities are bound
by the descriptor. Existing canonical/legacy gzip fixture handling is unchanged.
The unmodified L1 snapshot is then restored into a new owned Anvil. The existing
replay archive recovery API reconstructs only fresh WALs, then normal node
startup reconstructs execution state. All three actual RPC anchor hashes and
transaction/receipt checks must pass before the descriptor is issued. Packaged
configs use a Noop writer; consumers recover registered records to fresh DBs.

This applies **only to newly generated Anvil component fixtures**. It does not
authorize replay, copying, exporting or changing any original mock Core/Geth/
index/funded database, historical FULL evidence, or its recovery constraints.
No old or generated L2 DB is packaged. Any incomplete archive, unexpected object,
anchor mismatch or failed startup leaves an unregistered partial package and
fails closed; it is not a fixture or CI qualification.

Commands and nodes use cooperative TERM and bounded 60-second drain; no force
kill or automatic retry exists. Overdue commands retain public PID/tool attention
evidence and private on-host logs. No runtime wall-time estimate is an actual
measurement; the generation plan fails when its deadline cannot be honored.

## Mandatory actual CI qualification

Generator output is **unregistered**, and its result explicitly leaves original
integration-suite, config-smoke and canonical qualification false. No registry
writer is implemented. After complete public package and fresh-restore evidence
review, issue the descriptor's exact source registration, materialize only its
whitelisted public files at `local-chains/anvil-component-only/v32.0`, rebuild
current consumers, then run:

1. `.github/scripts/test-configs.sh --anvil-component-only`: component-bootstrap
   ConfigValidate -> server::run -> actual RPC -> transaction/receipt. CI downloads
   that explicitly named same-source binary. It never silently substitutes the
   test DA mock under the production server name.
2. The **unchanged ordinary workspace nextest entrypoint** in `.github/workflows/ci.yml`:
   all original 110 live component cases, config/RPC/tx/receipt/restart/migration
   coverage retained. No filter/skip or fake proof-qualification claim is added.
3. Canonical/prover/five-role release gates remain separate and blocked until
   their own genuine backend/runtime fixture exists.

Neither source tests nor a deployment alone make either failed CI job green.
Actual source build, generated package, source registration, config smoke and
original suite results are still required. No rental/lease/cap changes or live
worker workload are implied by this component-only generation workflow.
