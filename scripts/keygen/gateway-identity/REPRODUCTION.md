# V32 offline guest-bound deployment identity

This task-local bundle derives a **planned** fresh V32 namespace. It does not
deploy, access RPC, establish signer control, or attest independent hosts.
The user selected a single keygen host. Never relabel these results as production
attestation or independent-host reproduction.

## Source and compiler provenance

- Era base: `8fb7c29a4e3174335c6480b23f57822e054f9d5f`.
- Source-patched Era tree: `3eefa0f127d1deff365ebffcf489b183cde0e756`.
- Public namespace plan: `../public-inputs/planned-v32-namespace.json`.
- Salt: `0x7a7ae2cf64eaa133584178cf81c0c2f0b2eafd0b5eb5d05c208760eb00459fb0`.
- Linux/amd64 `forge 1.3.5-foundry-zksync-v0.1.5`, commit `807f47ac`;
  binary in `../foundry-zksync-v0.1.5/forge`.
  Binary SHA256: `789c539cc69ccbfbeee308b6305321edab651a327cca2c438961e3150448e987`.
- Solc `0.8.28+commit.7893614a`, optimizer enabled, 9,999,999 runs,
  no via-IR, IPFS/CBOR metadata. Ordinary contracts Cancun; relay Prague.
  Solc binary SHA256: `9a0fb7e0db2c0641dbae1c5cc645dc686820c83af516226abb1c0a2f76636f25`.
- Local Linux/amd64 build image ID:
  `sha256:ae81a545b4b78391c20cb6f06501c24f2fc0776a77c68552f2e934578ec15f07`.
- Build inputs use the unmodified Era `l1-contracts/foundry.toml`, including
  its remappings and locked submodule/dependency installation.

`cancun-out` and `prague-out` are canonical-settings identity artifacts;
`harness-out` is **not** deployment bytecode. `foundry.toml` here applies only to
the harness and adds task-local read/write permissions. Docker overlays it for
the harness without editing Era. The canonical Cancun artifacts are overlaid
read-only at the helper's expected `l1-contracts/out` path. The only checkout
addition made for that mount is an empty ignored `out` directory.

## Build recipe

From the task directory, run Docker with:

```sh
docker run --rm --platform linux/amd64 \
  --mount type=bind,source="$PWD",target=/work \
  --mount type=bind,source="$PWD/era-contracts",target=/work/era-contracts,readonly \
  --mount type=bind,source="$PWD/identity/compiler-cache",target=/root/.svm \
  --workdir /work/era-contracts/l1-contracts \
  airbender-build:nightly-2026-02-10 /work/foundry-zksync-v0.1.5/forge build \
  contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerProxyAdmin.sol \
  contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerValidatorTimelock.sol \
  contracts/governance/Governance.sol \
  --root /work/era-contracts/l1-contracts --out /work/identity/cancun-out \
  --cache-path /work/identity/cancun-cache --evm-version cancun --use 0.8.28 \
  --optimizer-runs 9999999 --optimize true \
  --build-info --build-info-path /work/identity/cancun-build-info
```

The initial invocation may download the pinned solc. Subsequent invocations can
add Docker `--network none` and Forge `--offline`. Build the relay separately,
using source `contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol`,
`--evm-version prague`, and distinct `prague-out`, `prague-cache`, and
`prague-build-info` paths; all other canonical compiler options stay the same.

## Calculation and independent checking

`prepare_input.py --help` lists explicit required source/tree bindings. It
validates the public namespace plan, derives Governance from its creation bytecode
and `(admin, securityCouncil, minDelay)` constructor, and produces exclusive-create
JSON input. New evidence must name the exact newly patched guest tree and use new
filenames. `critical-input-authenticated-admin.json` and
`results/guest-bound-authenticated-admin.json` bind the recovered administrator
and guest tree `6935489bdbc7b1ed31e608677d1b2418b10691b5`. Earlier inputs and outputs
are retained separately as superseded; never reuse their target or guest payload.
That historical checked result's SHA256 is
`0b0b2f2f8032167257a179712783c14c7d6e6a3799848ec77b9a1d471683a48f`.
After the fresh-CTM capacity correction, `critical-input-ctm-capacity.json`,
`results/helper-ctm-capacity.json` and `results/guest-bound-ctm-capacity-published.json`
repeat the real helper dry-run and independent check against the new source tree.
The checked result SHA256 is
`fad8fa83881e4412b49497a39568f90304c09e01d1068b03b3e58758f3b00877`;
every derivation, address, constructor and runtime hash is unchanged. No rental
workload or network access was used for this local cached reproduction. The
published helper differs from raw Forge JSON only by a final newline; the final
independent check hashes those exact published bytes.

The source-only inventory refresh subsequently corrected the two old Era
verifier rows and two admin ABI snapshots after a pinned full inventory
reproduction. `critical-input-source-inventory.json`,
`results/helper-source-inventory.json`, and
`results/guest-bound-source-inventory-published.json` repeat the genuine offline
helper and checker against tree `331f5b833c34379ad7f2fe9ea25279380e7aba52`.
The new checked result SHA256 is
`e6c5ced394e4934d64e92d98e850472b8c4200322a0ba6e93126a49e220b5777`;
the entire derivations object still equals the preceding capacity-fixed record.
No candidate app verifier, guest binding, runtime identity or production gate
changed. This reproduction used local pinned Docker without network access,
and the checker again consumed the exact published helper bytes.

The subsequent domain-correct PLONK generator source is bound by
`critical-input-domain-generator.json`, `results/helper-domain-generator.json`,
and `results/guest-bound-domain-generator-published.json`. The genuine helper and
independent checker ran again on tree `3eefa0f127d1deff365ebffcf489b183cde0e756`;
the checked result SHA256 is
`2efa802311ab4efb3827e820ba41df33867820d11e7f08c3ec62f27c7dc3bf3a`.
All critical derivations remain byte-value identical to the preceding record.
The original failed 2^24 generated verifier and successful regenerated 2^25
real-proof EVM results are retained separately; this source-only critical record
does not claim full CTM graph validation or deployment.

```sh
bash identity/run_offline.sh critical-input.json helper-critical-new.json
python3 identity/check_identity.py \
  --input identity/critical-input.json \
  --helper-output identity/results/helper-critical-new.json \
  --era-root era-contracts --artifacts-root identity/cancun-out \
  --relay-artifact identity/prague-out/SyscoinRelayedSLDAValidator.sol/SyscoinRelayedSLDAValidator.json \
  --output identity/results/guest-bound-identity-new.json
python3 -m unittest discover -s identity -p test_check_identity.py -v
```

The runner forces no network and no RPC, takes no extra flags, and refuses to
overwrite outputs. The Solidity script also requires Forge's `ScriptDryRun`
context. The independent checker uses its own stdlib Keccak-f1600, ABI and CREATE2
implementation, verifies compiler settings and every source hash in artifact
metadata, and cross-checks the helper's exact factory calldata. Its hash/address
test vectors and bad-input rejection tests run without any network.

Critical scope calls the exact helper `_calculateProxyAdminDeployer` and
`_calculateValidatorTimelockDeployer` functions, checks the frozen relay artifact,
and independently derives planned Governance. It intentionally does **not** call
full `calculateAddresses`, does not invent missing CTM inputs, and does not emit
provisional downstream zero addresses.

## Full CTM input assembly after VK

Use the **new planned root ecosystem**, not legacy on-chain configs:

1. Retain this exact namespace salt and predicted Governance. The planned new
   Bridgehub's owner must be that Governance contract. Its owner EOA is not the
   address to alias. `GatewayVotePreparation.initializeConfig` gets this value
   from `Bridgehub.owner()` and checks representative CTM protocol version.
2. Bind `l1ChainId` to the intended L1 chain (5700); the actual initializer uses
   `block.chainid`. Read
   `eraChainId` from the new root AssetRouter's constructor/configuration. The
   latter is what `AddressIntrospector.getEraChainId(assetRouter)` later returns;
   do not assume it equals gateway chain ID 57001 or edge ID 57057.
3. Set `isZKsyncOS=true`, `testnetVerifier=false`. Derive six selector arrays
   using the same `Utils.getAllSelectorsForFacet` runtime-bytecode scanner for
   the newly compiled Admin, Executor, Mailbox, Getters, Migrator and Committer.
   Preserve its ordering and exclusion of `getName()`.
4. Use newly generated V32 genesis/chain-creation data from
   `Utils.genesisConfigPath(true)` / `DeployCTMUtils.getChainCreationParamsConfig`:
   `ChainCreationParamsLib` reads the new genesis root and semantic version.
   Its zkOS branch explicitly sets batch commitment to `bytes32(1)` and leaves
   bootloader/default-account/EVM-emulator hashes and repeated-storage index
   zero. These are source-defined zkOS semantics, not guessed missing values.
   Protocol 0.32.0 packs as `137438953472`. Do not copy old v31 genesis values.
5. Supply the exact new `.force_deployments_data` byte string from the prepared
   gateway conversion input. It participates in CTM chain-creation parameters.
   It is not an arbitrary empty default.
6. Recompile the **complete** ordinary artifact set after replacing the real
   PLONK verifier, keeping relay Prague isolated. `BytecodeUtils.readBytecodeL1`
   reads the exact `out/<file>/<contract>.json` artifacts. With explicit full
   configuration and `scope="full-ctm"`, this harness calls unchanged
   `GatewayCTMDeployerHelper.calculateAddresses`.
7. Cross-check all guest-bound addresses and bytecodes remain identical. Update
   verifier/deployer/final-CTM/bundle hashes separately; their values are expected
   to change. The verifier deployer's creation code embeds the PLONK verifier,
   so a blanket "only PLONK artifact changes" rule is not correct.
8. Only later, in the separately authorized deployment workflow, compare the new
   actual Bridgehub owner, AssetRouter chain ID, protocol and addresses with this
   offline plan, check collisions and signer control, and publish coordinated
   source/VK/verifier evidence before deployment/cutover.

## Derived identities

- Planned root Governance: `0x7d631ab177342c99d3a925703d985d2de3fbfdf8`.
- Its L1-to-L2 alias: `0x8e741ab177342c99d3a925703d985d2de3fc0f09`.
- Guest-bound ValidatorTimelock proxy: `0xabb69e8e899c06e51414efde62d4423de4f35004`.
- Proxy runtime: 2840 bytes, Keccak
  `0xd98965fa7f49fc4302a2d161454fb0ef619516fbb05a24724e64bb3a3e06e5c4`.
- Relay unchanged: `0x758b06cda80bdd016f79afd0df1a984039067a21`, runtime 1590 bytes,
  Keccak `0x4c86ffe57098cb09a48ee6dfa4f21b2cce8e327409e1da1dc6be4545220b89e0`.

Preserve the harness, public inputs, source bindings, compiler pins, build-info,
metadata-bearing canonical artifacts, helper output and independent-check result
when publishing the completed validation evidence. No credentials are required.
