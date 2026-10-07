# Gateway Launch (Checkpointed Canonical Flow)

Gateway + edge launch is now a **single canonical command** with checkpointed resume and explicit repair.

For an operator-authorized replacement of an existing public testnet, use the
[fresh testnet procedure](#fresh-public-testnet-replacement) before the normal
launch command. It is not a mainnet reset or upgrade procedure.

## Host prerequisites

Install the host toolchain before starting the launcher. The launcher can
materialize the pinned `zksync-era` workspace and apply Syscoin patches itself,
but it expects the base build and signing tools to already be available.

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config libssl-dev clang lld cmake protobuf-compiler \
  libclang-dev git curl jq unzip zip ca-certificates python3 python3-pip \
  python3-venv tmux screen expect moreutils gnupg
```

If you are building the local Syscoin Core node on the same host, also install
the Unix build dependencies from Syscoin's `doc/build-unix.md`. Boost, libevent,
SQLite, and ZMQ are needed for the command-line node / wallet / DA RPC path used
by launch:

```bash
sudo apt-get install -y \
  libtool autotools-dev automake bsdmainutils libgmp-dev \
  libevent-dev libboost-dev libsqlite3-dev libzmq3-dev \
  libminiupnpc-dev libnatpmp-dev
```

Then build Syscoin Core:

```bash
cd /path/to/syscoin
./autogen.sh
./configure --without-gui
make -j"$(nproc)"
```

The launch flow assumes descriptor wallets backed by SQLite. Berkeley DB is only
needed if you intentionally enable legacy wallets; do not add it for the normal
Gateway launch path.

Install Rust and the RISC-V support used by the `V32` OS-server line:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default
export PATH="$HOME/.cargo/bin:$PATH"
rustup toolchain install nightly-2026-01-22
rustup default nightly-2026-01-22
rustup target add riscv32i-unknown-none-elf
rustup component add llvm-tools-preview rust-src
cargo install cargo-binutils
```

Install Node/Yarn:

```bash
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
sudo apt-get install -y nodejs
sudo corepack enable
corepack prepare yarn@stable --activate
```

Install **foundry-zksync**, not vanilla Foundry. `zkstack` checks that
`forge build --help` contains `ZKSync configuration`; a regular Foundry
installation does not satisfy this prerequisite.

```bash
curl -L https://raw.githubusercontent.com/matter-labs/foundry-zksync/main/install-foundry-zksync | bash
export PATH="$HOME/.foundry/bin:$PATH"
foundryup-zksync -i v0.1.5
```

The migration repair journal accepts this pinned build (and the audited
vanilla Foundry 1.7.1 fallback) only; changing Forge requires re-auditing its
sequence persistence and resume behavior.

Cache the Solidity and ZKsync Solidity compilers used by the v32 contracts
before the first offline
launch. Both `run-gateway-launch.sh` and `gateway-launch-repair.sh` default
`FOUNDRY_OFFLINE=true`, so an empty
compiler cache will otherwise fail on missing `solc 0.8.28` or `zksolc 1.5.11`.

```bash
tmp="$(mktemp -d)"
cd "$tmp"
cat > foundry.toml <<'EOF'
[profile.default]
src = "src"
out = "out"
libs = []
solc_version = "0.8.28"

[profile.default.zksync]
zksolc = "1.5.11"
EOF
mkdir -p src
cat > src/CacheSolc.sol <<'EOF'
// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;
contract CacheSolc {}
EOF
FOUNDRY_OFFLINE=false forge build
FOUNDRY_OFFLINE=false forge build --zksync
cd -
rm -rf "$tmp"
```

Import the L1 funding/deployment signer into Foundry's encrypted account store:

```bash
cast wallet import funder --interactive
cast wallet address --account funder
```

For unattended launches, create a `0600` password file and pass it via env:

```bash
umask 077
printf '%s' '<keystore-password>' > "$HOME/.foundry/funder.password"
export FUNDER_PASSWORD_FILE="$HOME/.foundry/funder.password"
```

If using a separate Gateway governor signer for migration repairs, import it as
another Foundry account (for example `governor`) and set
`EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME=governor`.

## Fresh public testnet replacement

<!-- SYSCOIN: A destructive testnet reset needs its own inventory and acceptance
record; SSH authentication, resumable launch, and mainnet rollout are not equivalent. -->

Replacing v31 with a fresh v32 testnet discards the old Gateway/edge history and
dependent indexes. Obtain explicit reset authorization and record the chosen
proving mode before stopping services. Preserve wallets and signer access even
when the old chain does not need to be retained. A local reset cannot erase old
contracts or escrow on the persistent Tanenbaum L1.

### Operator and source preflight

1. Inventory the sequencer, external-node/explorer, faucet, portal and bridge
   hosts. Record service/container owners, actual config paths, RPC routing,
   chain IDs, database/volume names, free disk and the running source identity.
   A host may serve more than one of these roles.
2. Check SSH and privileged access independently on every relevant host:

   ```bash
   ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=yes -o UpdateHostKeys=no \
     -i "$SSH_KEY_PATH" "$REMOTE_HOST" 'id -un'
   ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=yes -o UpdateHostKeys=no \
     -i "$SSH_KEY_PATH" "$REMOTE_HOST" 'sudo -n -l'
   ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=yes -o UpdateHostKeys=no \
     -i "$SSH_KEY_PATH" "$REMOTE_HOST" 'passwd -S'
   ```

   A private SSH key authenticates the SSH account; it does not grant sudo.
   Inspect the allowed commands, not only group membership. Resolve a password
   prompt or missing deployment privileges through the operator's approved
   administrative access before an unattended reset. Do not bypass sudo with
   privileged containers or infer root permission from Docker access.
   `passwd -S` reports account status, not a password: `P` is usable, `NP` means
   no password is configured and `L` is locked. A key-only SSH login can work
   while password-based sudo is unavailable. Use the approved account-password
   setup/recovery path; the SSH key cannot reveal an account password.
3. Pin the server, Core/Geth and client source revisions, upstream patch trees,
   compiler versions and deterministic deployment inputs. Use a clean release
   checkout; do not pull over unrelated local changes. Read the current source
   namespace/address plan instead of copying an old CREATE2 salt or governor.
   Mock proving does not waive the immutable Gateway DA target/relay or gas-tank
   address checks. Stop for reviewed repinning if a fresh deployment differs.
4. Locate the actual wallet YAML, encrypted Foundry accounts and password-file
   references. Store required wallet files outside the reset workspace, with
   private permissions, and verify public signer addresses and live balances on
   root chain 5700. An isolated chain-31337 test receipt is not that balance.
   Preserve the required Core DA wallet separately. Do not commit secret values,
   credentialed RPC URLs, host inventories or private operator paths.
5. Load the current signer configuration explicitly. Tanenbaum defaults to
   encrypted account/keystore signing; do not carry raw private-key environment
   variables from an old command. Generated prover configs still require a
   password of at least 32 characters, even for this mock-proof deployment.

<!-- SYSCOIN: Documentation follows the reviewed helper fixes without duplicating
their implementation in the runbook change. -->
Use the reviewed launch helpers, including the administrator authentication and
receipt-anchor changes in [server PR #333](https://github.com/syscoin/zksync-os-server/pull/333).
This runbook update does not itself add those code changes. For an external
encrypted administrator, set all three signer families explicitly; for example:

```bash
export FUNDER_SIGNER=account
export FUNDER_ACCOUNT_NAME=launch-admin
export FUNDER_PASSWORD_FILE=/secure/operator/launch-admin.password
export DEPLOYER_SIGNER=account
export DEPLOYER_ACCOUNT_NAME=launch-admin
export DEPLOYER_PASSWORD_FILE=/secure/operator/launch-admin.password
export EDGE_GATEWAY_GOVERNOR_SIGNER=account
export EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME=launch-admin
export EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE=/secure/operator/launch-admin.password
unset FUNDER_PRIVATE_KEY DEPLOYER_PRIVATE_KEY EDGE_GATEWAY_GOVERNOR_PRIVATE_KEY
cast wallet address --account "$DEPLOYER_ACCOUNT_NAME" \
  --password-file "$DEPLOYER_PASSWORD_FILE"
```

Use an absolute, owner-only regular password-file path: Forge changes working
directory. Public YAML addresses with `private_key: null` are supported only
for authenticated governor/deployer roles, not generated runtime operators.
The helpers verify every requested role against the decrypted account and
forward an explicit Forge `--sender` bound to that authenticated address.
Account selection alone was insufficient for pinned Forge 0.1.5. Address
derivation receives only account selectors, not Forge's `--sender` option.
Gateway conversion checks its chain deployer, chain governor and ecosystem
governor together; one global external selector must match all three actual
actors. Distinct generated-key administrators remain supported when no external
selector is forwarded. Do not bypass a role mismatch with an arbitrary sender
or an ambient wallet environment variable.

### Confirmation progress and broadcast recovery

<!-- SYSCOIN: Slow root confirmations are not permission to erase a broadcast
journal or re-run a deterministic deployment from scratch. -->
The current Tanenbaum rehearsal uses a **150-second block target** and its
canonical core deployment sequence contains **43 transactions**. These are
observed/configured rehearsal inputs, not a fixed ETA or a universal deployment
transaction count. Sequential receipt waits, actual block cadence and required
confirmations can make a healthy run appear quiet for several minutes. Inspect
the recorded transaction hashes, canonical receipts, sender's latest/pending
nonce and launcher progress before diagnosing a failure. Do not start a second
deployment or stop a progressing broadcast because a wall-clock estimate elapsed.

<!-- SYSCOIN: Cast's submission wait is separate from the root block cadence
and RPC request timeout; preserve submitted transactions after a timeout. -->
For the dated 2.5-minute Tanenbaum lane, the pinned Linux `cast send --help`
confirms that `ETH_TIMEOUT` controls transaction confirmation waiting. Set
`export ETH_TIMEOUT=1800` in the reviewed invocation environment before a new
broadcast; this does not change gas pricing, finality or the RPC request timeout.
A Cast timeout does not prove that its transaction failed or disappeared.
Reconcile its hash and canonical receipt before selecting a supported remainder;
do not automatically retry the whole helper.

Distinguish these recovery cases before changing any checkpoint or artifact:

| Evidence | Permitted next step |
| --- | --- |
| Verified pre-broadcast failure: no submitted transaction, no new latest/pending sender nonce, no deployment receipts or code, and no partially broadcast deployment output | Correct the diagnosed selector/configuration problem and retain the evidence. The absent-graph recovery below permits only an explicit same-input invocation of the canonical deployment helper; checkpoint repair/revalidation follows only after the full live graph exists. A missing output file alone is not proof that nothing was broadcast. |
| Any transaction submitted, confirmed or pending; a changed sender nonce; or a partial deployment/broadcast artifact | Preserve the entire checkpoint, Forge broadcast journal, inputs, hashes and receipts. Reconcile the exact sequence using supported repair/resume; do not reset, replay or reconstruct an apparently fresh deployment automatically. |

<!-- SYSCOIN: A blocked L1 checkpoint's ownership-only repair cannot create a
missing graph; this narrow no-broadcast recovery is not a partial-journal replay. -->
For the verified no-broadcast/absent-graph case, the current blocked
`gl.l1_ecosystem_deployed` repair uses ownership-only recovery and rejects a
missing graph. Do not delete its checkpoint or artifacts to manufacture freshness.
After recording the no-broadcast evidence and approving the diagnosed fix,
retain the exact network, source, namespace/salt, wallet/admin inputs and invoke
the canonical helper explicitly with the already validated launch environment:

```bash
GATEWAY_ECOSYSTEM_RESUME_FIRST=false \
  bash scripts/gateway-launch/gateway-deploy-l1.sh
```

This helper still authenticates the serialized launch context and rejects an
existing partial/invalid graph; it is not a bypass. Only once the complete live
graph exists, use supported checkpoint repair/revalidation and then resume the
launcher. **Never use this fresh-helper recovery for a submitted, pending or
partially broadcast journal**, even when its normal output file is absent.

An `eth_call` from an owner address proves call compatibility, not possession of
its signing credential. Do not treat that simulation as a custody or broadcast
receipt. Keep the live deployment status in the private operator record; this
guide does not infer full-ecosystem or public acceptance from Core receipts.

### Confirmed Core journal interrupted before CTM initialization

<!-- SYSCOIN: Core-only continuation is an explicit reviewed journal recovery,
not the fresh absent-graph invocation or a complete-graph ownership-only repair. -->
A successful Core deployment can be followed by an interrupted administrator
handoff before zkstack persists `configs/contracts.yaml`. A missing config does
not make that graph absent. Preserve the Core inputs/output, original journals,
receipts and checkpoint; do not invoke the fresh-helper recovery above.

The reviewed CLI in PR #333 exposes the narrow
`ecosystem init-core-contracts --core-journal-only --resume` entry point. Before
using it, independently bind a protected recovery manifest and exact invocation
to the selected network/genesis, source and artifact identities, unchanged
initial deployment config and Core input/output bytes, complete journal,
actor/nonces/calldata, canonical successful receipts, full deployed runtimes,
proxy slots and administrator state. Authenticate the wallet selectors and
qualify the actual CLI binary and its normal build stamp separately. Hold the
canonical launcher lifecycle lock and repeat those checks immediately before
execution; neither journal presence nor a receipt claimed inside it is sufficient.

This mode requires an absent contracts config, existing input/output and a
complete Core journal with no failed or pending receipts. The reviewed
invocation includes `--zksync-os`, `--update-submodules false`,
`--skip-contract-compilation-override true`, `--deploy-erc20 false` and
`--support-l2-legacy-shared-bridge-test false`, with the authenticated root RPC
and external wallet selectors. It never regenerates inputs, deploys CTM
contracts, or falls back to a fresh Core script after a resume error, including
a missing journal.
The existing state-gated handoffs run after the fixed Core resume. Only the
exact same-owner pending transfer to the authenticated governor EOA may use
the pinned direct acceptance path; foreign and aliased successors remain errors.

Require clean owner/pending-owner postchecks before the canonical typed Core
config is saved. The reviewed writer uses the unchanged YAML serializer and
owner-only, exclusive creation followed by file sync. An existing regular file,
empty/malformed config, symlink or broken link is never overwritten. A write or
sync failure can leave a newly created partial config: stop and inspect it,
rather than deleting it or retrying against an assumed absent path. Retain all
original journals and verify that Core input/output bytes did not change.

<!-- SYSCOIN: Dated receipt evidence records completed scope without upgrading
a Core-only observation to CTM, launch, finality or funding authorization. -->
The 2026-10-06/07 Tanenbaum rehearsal used release
`d8d5cc2fbb8db13109d68897431e610b59788491` for this continuation. At canonical
block 985288, the independent post-Core audit found 61 successful receipts,
zero failed or pending receipts, unchanged prior 59 commitments and no replay
of the original 43 Core transactions. The only new handoffs were:

| Administrator nonce | Action | Canonical transaction |
| --- | --- | --- |
| 69 | Bridgehub `setPendingAdmin` + `acceptAdmin`, atomically via ChainAdmin | `0x7cc634145f32528cd10b6590eb85ded56e8cf6ef68d452c4b1150e60f51cb70b` |
| 70 | NativeTokenVault direct `acceptOwnership()` | `0xd52db01f32eef2fa6ca933032192a2fe9ad772f347d65e1f8e0b82b2642857ac` |

Both had at least two canonical confirmations at that snapshot; this is not a
consensus-finalized assertion. The persisted Core-only config was mode `0600`,
SHA256 `edf1728bcd4552c04d00b084254a12fb8025b294b5d60c3b1d8587d3e6907339`,
with CTM absent. The retained audit digest is
`a00079746ad8b7584652494d96af55f93397b8756a7a99a3cc08f3079401c71f`.
These are dated evidence identifiers, not inputs to copy into a new deployment.
This qualification does not complete CTM registration, checkpoint repair,
Gateway settlement, funding, service installation or public acceptance.

### Restore canonical context before a separate fresh CTM deployment

<!-- SYSCOIN: The canonical asset-ID derivation is an in-process dependency;
operator-provided overrides must remain rejected by launch fingerprint guards. -->
On Tanenbaum/mainnet, CTM initialization requires the derived
`ZKSYS_ZK_TOKEN_ASSET_ID` (with `ZK_TOKEN_ASSET_ID` as its exported alias).
The normal `gateway-deploy-l1.sh` performs this derivation before invoking
zkstack. A standalone CTM command after Core-only recovery does not inherit
that earlier shell process's exported value. In this rehearsal, its absence
caused an initialization panic **before broadcast**: latest/pending nonce
remained 71 and no CTM journal, input or output was created. A panic or missing
output alone is not sufficient proof; establish that full no-broadcast evidence
before correcting context and invoking the separate fresh CTM command.

Keep both asset-ID variables **unset in protected operator launch environment
files**. Do not insert a copied constant or relax their preflight/fingerprint
rejection. Derive the value in-process from the authenticated canonical source
after validating the launch context. The reviewed function is
`derive_and_export_zksys_zk_token_asset_id()` in
`scripts/gateway-launch/gateway-deploy-l1.sh` (source SHA256
`9ec03f7df3fac8093fa8f0bb8a4fdfd25eaf427a8de57825b8f2fd3d24e1dc10`
for the d8 rehearsal). Bind its normalization helpers, exact
`forge inspect --no-metadata` bytecodes and toolchain, canonical `0x4e59...`
deployer, three selected salts, token admin, token name/symbol/decimals and
edge-chain ID. Record the resulting ProxyAdmin, implementation, proxy address
and full preimages without changing canonical deployment artifacts.

The asset ID is `keccak256(abi.encode(edgeChainId, L2NativeTokenVault,
derivedZksysTokenProxy))`, where the v32 vault is
`0x0000000000000000000000000000000000010004`. It is not the native SYS asset
ID and does not use Root or Gateway chain ID. The token proxy is still a
deterministic **future** L2 address; this derivation does not deploy the token,
start issuance, attest live token code or authorize its use as a value recipient.

Do not source the entire deployment script to evade its partial-graph guard.
Any recovery harness must be independently reviewed, exact-source/hash-bound
to only the required derivation functions and retain normal source, signer,
artifact and lifecycle-lock checks. It is not a generic launcher feature or
permission for broad replay. Ordinary fresh launches need no such harness.
The standalone process must also carry the effective Solidity CREATE2 salt,
not only the outer launcher's `GATEWAY_CREATE2_FACTORY_SALT`. The approved
2026-10-07 Tanenbaum context explicitly binds these distinct variables:

```bash
export GATEWAY_CREATE2_FACTORY_SALT=0x7a7ae2cf64eaa133584178cf81c0c2f0b2eafd0b5eb5d05c208760eb00459fb0
export CREATE2_FACTORY_SALT=0x7a7ae2cf64eaa133584178cf81c0c2f0b2eafd0b5eb5d05c208760eb00459fb0
export LEGACY_GOV_SALT=0x0000000000000000000000000000000000000000000000000000000000000000
```

These are dated testnet inputs, not mainnet defaults. Require both CREATE2
values to equal the normalized existing initial deployment salt. Keep the
legacy governance operation salt separate; do not substitute a script default,
rewrite the existing input, or copy a dry-run artifact into the live namespace.
Omitting the effective salt from the recovery invocation was the launch
operator's context error, not an upstream vulnerability.

The unchanged CLI checks global prerequisites before CTM command dispatch.
A sanitized execution `PATH` must retain the qualified Cargo route: this
rehearsal uses `/home/ubuntu/.cargo/bin` alongside the pinned Foundry and system
tools. Validate the installed Cargo/rustup proxy and normal prerequisite checks;
do not add `--ignore-prerequisites`, install a replacement toolchain, or source
an ambient Cargo shell environment to bypass the failure.

After rechecking the qualified Core graph, no-broadcast CTM evidence and
current nonce, run the separate canonical fresh CTM initialization with the
derived value in that process, **without a resume flag**. If anything was
submitted, stop and reconcile its actual journal instead of assuming this case.

CTM initialization and `ecosystem register-ctm` are distinct steps. Source-bind
their inputs/calldata, canonical receipts, CREATE2/runtime/proxy/immutable
identities, explicit testnet verifier mode, owners and Bridgehub registration.
Only after the complete registered graph satisfies the existing live probes
may supported checkpoint repair/revalidation run. Never manufacture a passed
checkpoint. The actual CTM receipts and whole-graph qualification remain
separate operator evidence; this section does not assert they have passed.

### Dated recovery findings and non-final CTM status

<!-- SYSCOIN: Separate a genuine runtime compatibility defect from test-only
observation changes and operator invocation mistakes; none imply live adoption. -->
The DA recovery mismatch is real: the pinned SDK can return a public-cloud
response as bare ASCII hex, while the d8 recovery gate authenticates those
wire bytes as though they were the decoded blob. A successful HTTP retrieval
therefore does not establish usable authenticated recovery data. The later
reviewed [4726 source](https://github.com/syscoin/zksync-os-server/commit/4726f87e205a5869250dc3aefc2ce0f6ead1fed8)
corrects this by authenticating raw bytes first, then
allowing one bounded strict bare-hex decode only when it matches the same
committed Blake2s digest, before attempt reservation or wallet publication.
It does not change the pinned SDK, finality policy or republish controls.
That source is **not live-adopted**. The d8 CTM invocation has exited, but normal
source adoption and new canonical Gateway/edge builds and stamps remain
required. Do not republish the already confirmed DA readiness marker to test it.

The issuance chain-ID normalization in the same reviewed release is also a
genuine launcher correctness fix: a configured/persisted hexadecimal edge
chain ID must be normalized before token-prelude sends, and receipt-anchor comparisons
must treat its decimal/hex aliases as the same chain while keeping every other
manifest binding strict. This preserves the receipt-timestamp-plus86400 policy;
it neither changes the issuance schedule nor establishes live token deployment.

The CI process-exit observation change is **test-only**: a bounded two-second
observer lets an already signalled owned task reach its terminal process state,
checks its start identity and still rejects a live child. It does not fix or
relax a runtime cleanup timeout. The default-salt omission above and the
missing Cargo `PATH` below are launch-operator errors, not upstream security
findings.

The first normal CTM invocation, capsule `f3e6e15c`, failed in the global Cargo
prerequisite gate before CTM dispatch. Independent reconciliation found zero
submissions, latest/pending administrator nonce 76 and no normal CTM
input/output/broadcast/cache namespace. Preserve that failed intent and all
diagnostics. The corrected capsule `02b03df1` was separately reviewed and
explicitly approved with a distinct intent namespace; it is not an automatic
retry or permission to erase/reuse the first intent. At this dated checkpoint,
its signed **35 deployment transactions plus seven owner/admin handoffs** are
observed with canonical successful `status=1` receipts for administrator nonces
76 through 117; latest/pending nonce is 118. This progress observation is not
the full source, runtime, poststate or registration qualification.

The original guard session `87536` actually exited with code **1**, observed at
`2026-10-07T04:46:53Z`. The underlying normal CLI exit status is unknown, and
the original `completed.json` is absent. Preserve the failed intent, normal
journals and diagnostics: successful transactions do not permit fabricating
an exit-zero/completion record or retrying the deployment. Separate CTM,
registration and Registry qualification remain required; no full ROOT
ecosystem, service cutover or public-testnet completion is claimed.

An additional offline reproduction found a false failure in the operator's
Python postcheck, not in the contracts: the pinned Rust YAML serializer emits
large hex payload Strings unquoted, while Python's generic safe reader coerces
them to integers. Preserve the original invocation and its ordinary transaction
sequence. Qualify the resulting state through a separately reviewed,
read-only exact-text/schema-aware check; never replay transactions or fabricate
the original guard's completion/exit result to satisfy a later registration gate.

<!-- SYSCOIN: Dated operator reconciliation distinguishes file metadata and
raw-artifact provenance from canonical receipts and complete live qualification. -->
Post-exit inspection found six public AdminFunctions journal JSON files and
six corresponding private-cache JSON files at mode `0664`. One exact,
identity-bound operation tightened those **12 files only** to `0600`, preserving
their bytes, hashes, inodes, sizes and modification times. It did not reset or
remove journals, change sender nonces, or modify contracts. A subsequent
metadata-only capture passed at `2026-10-07T04:56:36Z`; that capture is not a
receipt, runtime, ownership or whole-graph audit.

The first separate read-only reconciliation was **unqualified**. It stopped
before full receipt/poststate qualification because 21 unique L1 artifact
JSON files have different raw SHA256 identities from the original source plan.
All compared deployment calldata, runtime, constructor, initializer and
source-unit summaries matched; those summaries do not establish equality of
the complete artifact JSON. No checked preserved candidate recovered an
original recorded raw artifact hash. Serialization-only equivalence is
therefore **unproven**; the raw-hash mismatch alone is not evidence of a
contract vulnerability or proof failure.

The subsequent independent current-artifact qualification passed: genuine
capture `d3eb3e5e` and separately activated result `effafb83`, both with exit
zero, bind all 48 full raw artifacts, complete source/compiler context and the
42 canonical transactions. The final snapshot was ROOT block 985489 with 80
confirmations for the last receipt and next administrator nonce 118. All
853 function selectors and 239 physical source units were independently
checked. Fees for those 42 unique receipts were booked once.

<!-- SYSCOIN: Compiler metadata projection and source-derived event checks are
operator qualifications, not changes to contracts or already signed payloads. -->
Canonical raw Solc metadata is authoritative; parsed Foundry metadata has a
narrow, explicitly checked projection for empty ABI arrays and empty remapping
contexts. Preserve both complete representations without claiming full JSON
equality. Exact handoff checks were also corrected against contract source and
actual receipts: fresh CTM administration starts at zero, and ChainAdmin
multicalls emit their own audit event. Nonces 113 and 117 have exactly three and
two ordered logs respectively. The signed deployment and handoff bytes did
not change.

This qualifies current CTM source, artifacts, receipts and state only. Retain
the original raw-hash mismatch and unknown CLI exit as historical limitations;
do not rewrite pins,
restore a fabricated artifact, replay any of the 42 transactions, manufacture
the original completion record or manually mark a checkpoint passed. Nonce
118 alone is not authorization to run registration; a separate source-bound
normal registration invocation and receipt/poststate qualification are required.
The original failed guard's completion stays absent.

<!-- SYSCOIN: A later independent registration qualification does not rewrite
the original wrapper exit, manufacture its completion or authorize replay. -->
The distinct registration invocation submitted two successful transactions at
nonces 118 and 119, but its wrapper exited with an error and its normal CLI exit
remains unknown. A later independent read-only qualification, `e583eb9d`, passed
at ROOT block 985519 with 27 confirmations for the last receipt. It binds the
complete current RegisterCTM artifact and all 64 source units, rechecks the
whole contracts/CLI source context before and after, and verifies both canonical
receipts, exact ordered events, bidirectional registry entries and preserved
Core/CTM configuration and checkpoint state. The two unique receipt fees were
booked once; do not resend either transaction or fabricate the missing original
completion. This independently qualifies registration only; the later Registry
and Root checkpoint milestones below do not establish public-service acceptance.

<!-- SYSCOIN: A source-preserved remainder completes the missing transaction
without replaying ownership handshakes or rewriting the failed invocation. -->
The later normal ownership/Registry invocation submitted successful rows
120–122, then its wrapper exited **1** when stock Cast timed out waiting for the
implementation receipt. The underlying helper's exact exit remains unknown,
and that original intent's completion marker remains absent. A separately
accepted read-only audit, `8ace1206`, qualified the three canonical receipts and
exact existing implementation at ROOT block 985546, with latest/pending nonce
123. Their **11,246,562,718 wei** in fees were booked once.

Do not rerun the whole ownership helper for this remainder: the pinned normal
ownership path emits two admin handshakes on each invocation, even when those
admins are already accepted. The reviewed temporary operator instead extracted
only the byte-exact reusable Registry function definitions and original context,
factory, private-inspection/trap and encrypted-account preparation from the
existing helper. It kept fresh source, custody, config, exact existing
ProxyAdmin/implementation, empty-proxy, nonce, fee and lifecycle-lock gates, then
called the stock Registry function once with `ETH_TIMEOUT=1800`. It neither
changed production code nor replayed rows 120–122.

That remainder invocation, session `18572`, exited zero and its accepted result
`02c08317` qualifies all four canonical rows **120–123**, exact Registry
runtime/proxy/initializer readbacks and the single Registry-address config
delta at ROOT block **985556**, with next administrator nonce **124**. The
proxy-only nonce 123 receipt added **6,114,086,867 wei** in new fees; do not charge
the already-booked first three again. The original failed invocation and unknown
helper exit remain unchanged, and this Registry result did not advance a
checkpoint or qualify services.

<!-- SYSCOIN: A successful already-valid repair is not proof that a future
repair's fallback has been atomically disabled by an earlier readiness probe. -->
The subsequent normal Root checkpoint repair, session `66941` with output
`62c0f18f`, exited zero and reported `gl.l1_ecosystem_deployed` already valid and
marked repaired. No ownership fallback or new broadcast was observed in that
invocation. Its initial readiness preprobe does **not** atomically disable the
repair's fallback; retain normal live validation and never assume a later repair
is incapable of broadcasting. The next normal Gateway prefix, session `11030`,
is running at this dated checkpoint. Gateway/Edge readiness, the fresh native
bridge route and public-service acceptance are not yet claimed.

For this disposable testnet rollout, the operator has accepted the explicit
legacy manual-deposit risk described below and selected a clean fresh native
bridge route using the existing contracts. Those decisions do not establish
receipt, route or public-service acceptance; the deployment is not complete.

### Retire old onchain deposit entry points

<!-- SYSCOIN: A disposable L2 database does not make persistent L1 governance,
external collateral or undelivered deposits disposable. -->
Before destructive retirement, inventory the old Bridgehub's registered chain
list, diamonds, ChainAdmins, Governance and their actual signer custody. Keep
decryptable/restorable controller credentials and verify their derived public
addresses against live owners. SSH/sudo, an available newly deployed governor,
TSYS balances and a successful owner-address simulation do not establish old
contract control. Missing old signing authority is a strict stop for mainnet,
and is never implicit permission to retire an accepting deposit endpoint.

<!-- SYSCOIN: Disposable-testnet deprecation is an explicit operator exception,
not an onchain pause claim or a mainnet custody/retirement shortcut. -->
For a disposable **testnet only**, the operator may explicitly approve a
different boundary: deprecate the old unpaused contracts, replace official
clients/endpoints and publish that direct manual deposits to those exact old
contracts can still be accepted and may remain unprocessed or unrecoverable on
the retired chain. Record the specific addresses/chain IDs, the retirement
boundary and accepted manual-deposit risk. Do not claim old onchain intake
stopped or reset shared external contracts and collateral. The treatment of
existing claims must be explicit, and this exception can never be copied to
mainnet. The operator explicitly accepted this exception for the current
v31-to-v32 testnet replacement.
Official old UI/routes still must be disabled; old contracts are **not claimed
paused**. General authorization to wipe databases alone is not sufficient for
another rollout.

<!-- SYSCOIN: This explicitly disposable rollout does not preserve or replay
v31 claims; the normal paused-retirement procedure below remains for other launches. -->
For this replacement, v31 rollup/queue/order/history preservation or migration
is not a launch prerequisite. Retire the exact old official routes and workers;
do not replay old messages or credit old backing to v32. Leave shared L1/Sepolia
contracts, collateral and validator/relayer history untouched. This is an
explicit testnet discard decision, not a completed mainnet claim-reconciliation
procedure.

For the normal paused-retirement path, prefer the deployed per-chain
`pauseDepositsBeforeInitiatingMigration()` route when its admin/CTM authority
is available. Bind its selector and runtime to the
old release, inspect the actual pause delay, and verify `depositsPaused()` rather
than assuming a source-only function or zero-delay setting. For the reviewed
v31 Tanenbaum deployment the delay is zero. Pausing the migrated edge on ROOT
also queues a free service transaction through the old Gateway to set the mirror
pause. Request the edge pause **before** pausing the ROOT Gateway. Keep the old
nodes live until that service executes, both pause flags are verified, active
queues drain and committed/verified/executed batch counts agree. The ROOT edge's
post-migration ingress mirror is not the active Gateway execution queue; compare
their totals, start indexes and priority roots instead of misreading its raw size.

A fallback `pause()` on the **dedicated old Bridgehub instance** is appropriate
only after proving its complete scope is exactly the two retired Tanenbaum
chains (57001 and 57057), verifying the deployed deposit guards, and establishing
that withdrawal/proof forwarding remains available. It is not permission to
pause shared AssetRouter/Nullifier contracts or another ecosystem. The reviewed
old Governance has zero minimum delay but still requires a scheduled operation:
`scheduleTransparent(operation, 0)` followed by `execute(operation)`.
`executeInstant` also requires a pending/scheduled operation; it is not an
unscheduled shortcut. Use only verified owner custody and approved exact targets.

For the normal paused-retirement path, pause the old source fast-path ingress
as well, then reconcile deployment-to-pause event windows, canonical old-L2
receipts, escrow paid/used state and queue
processing boundaries. Worker ledgers alone can omit historical orders. Preserve
cancellation, reimbursement and reserve/liability records; never replay a
cancelled or reimbursed message against the new Bridgehub. Preserve unchanged
Tanenbaum/Sepolia state, external collateral and shared validator/relayer history.
The operator-authorized discard covers v31 rollup databases and indexes, not
those shared assets. For paused retirement, repeat final queue/event checks
after onchain intake is closed. Under a separately approved testnet exception,
follow its recorded claim/discard boundary, close official/fast-path intake and
explicitly acknowledge that later manual legacy deposits can still arrive.

<!-- SYSCOIN: Clean testnet replacement must not require new production contract
logic just to retain an obsolete route's collateral or one-time peer bindings. -->
An existing collateralized NativeIngress proxy has no Bridgehub setter in the
reviewed old implementation. Its original initializer already accepts a
Bridgehub, so a fresh proxy can target the new ecosystem without adding a setter
or upgrading the old proxy. The operator narrowed the earlier rebind approval
to simple operations and selected this fresh route. The staged setter candidate
is unused; old proxy/admin/implementation, backing and Sepolia synthetic supply
remain untouched.

Deploy fresh GasVault/NativeIngress instances, a new Sepolia synthetic router
with **zero initial supply**, route-specific pause/timelock/aggregation modules
and a fresh paused fast-path pair using the original contract logic. Attest any
reused shared Mailboxes/validators/relayer without resetting their history or
policy. Do not retarget one-time timelock or fast-path peer bindings, manually
mint synthetic supply, move old backing or credit it to the new route.

The current Sepolia fee-plus-value ceiling remains **0.05 ETH**; an offer to
provide more testnet ETH does not raise it automatically. Obtain actual bounded
transaction estimates before signing. New native sponsorship, canaries and
fast-path inventory must fit the approved operating allocation, not the
separate faucet reserve. The documented 1,000-wSYS example minimum is not a
requirement to spend 1,000 TSYS: use reviewed smaller testnet limits or obtain
additional funding approval.

Fresh addresses alone are not acceptance. Require reciprocal new-router
enrollment, actual owner/code/asset readbacks, route preflight and authenticated
canonical credit through the new Root/Gateway/edge before enabling the UI or
fast lane. Keep new routes closed until these checks pass. UI closure alone does
not block direct calls to old source contracts.

### Exact reset inventory

Stop the identified owners and verify their processes have exited before
removing chain-derived state. Record the resolved targets; do not perform a
wildcard home-directory cleanup or global Docker pruning.

| Component | Fresh-reset scope | Preserve |
| --- | --- | --- |
| Gateway and edge | Obsolete ecosystem workspace, both complete runtime database/recovery trees and the exact matching launch checkpoint namespace | External wallet YAML, signer/keystore material, required deployment inputs |
| zkSYS external nodes | Both configured public/debug database and recovery trees | Peer identities, private config inputs and build prerequisites |
| Blockscout | Exact Gateway/zkSYS project database, Redis and backend DETS volumes | Compiler caches, secrets, TLS and branding |
| Faucet and applications | Old-chain nonce/rate caches, contract/start-block configuration and explicitly identified L2-derived worker state | Faucet wallet, application secrets and unrelated chain history |
| Tanenbaum L1 | Not part of the discarded rollup | Core/Geth datastore, DA wallet and root-L1 funds/history |

Fresh checkpoint state is normally beside `GATEWAY_DIR`, under
`.gateway-launch-state/<sha256(realpath(GATEWAY_DIR))>`; a supported legacy
deployment can instead have state inside `GATEWAY_DIR/.gateway-launch`.
Resolve the active path with `gl_checkpoint_state_dir` before reset. Deleting
`gateway` alone is not sufficient, and deleting the entire sibling namespace
can destroy another deployment's state.

`deploy-zksys-en-rpc.sh` rebuilds/reconfigures nodes but does not clear their old
databases. Blockscout `deploy-remote.sh` recreates services but retains chain
indexes. Both require the explicit, inventoried reset above. Keep bridge ingress
paused while old L2 routes are replaced; do not erase unchanged Tanenbaum,
Sepolia, external checkpoint or shared queue history as a side effect.

### Explicit mock-testnet launch

Load the approved wallet, namespace, admin and RPC inputs described below, then
select the complete testnet verifier mode:

```bash
export PROTOCOL_VERSION=v32.0
export SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=true
export PROVER_MODE=no-proofs
export GATEWAY_PROVER_MODE=no-proofs
export EDGE_PROVER_MODE=no-proofs
unset MIGRATE_EDGE REUSE_ECOSYSTEM
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum
```

`PROVER_MODE=no-proofs` alone is not sufficient. The explicit mock flag and all
three modes must agree. Use the actual Tanenbaum root chain 5700 and the
canonical Gateway/zkSYS chain configuration, not a local fork masquerading as a
public launch. Do not populate the blocked canonical local-chain fixture.

The staged invocation omits migration while the independent
`GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS` pin is acquired and verified from the
deployment record. Once that pin and the launch identity gates pass, resume:

```bash
unset GATEWAY_WALLET_CREATION GATEWAY_WALLET_PATH
# SYSCOIN: Keep approved edge in-file selectors and EDGE_REUSE_GATEWAY_GOVERNOR=false
# while the fresh edge is absent.
bash scripts/gateway-launch/run-gateway-launch.sh \
  --l1 tanenbaum --reuse-ecosystem --migrate-edge
```

Clear edge wallet-creation inputs only when reusing an already created edge.
Until then, retain the exact protected edge in-file path/creation inputs and
`EDGE_REUSE_GATEWAY_GOVERNOR=false`; do not discard its selected custody.
`--migrate-edge` transitions the newly initialized edge to Gateway settlement;
it does not mean retaining or migrating the old v31 chain. Keep the proving
mode and deterministic inputs unchanged across retries. Use the checkpoint
repair procedure for a diagnosed failure, not manual checkpoint deletion.

### Independently derive and then attest the wrapped-token pin

<!-- SYSCOIN: Published genesis bytes, not mutable Forge output or an RPC-selected
recipient, determine the wrapped native-token CREATE2 identity. -->
Derive the pin before using it as a native-value recipient. Preserve the exact
reviewed genesis JSON/hash, source routing, proxy creation/runtime bytes,
constructor inputs and CREATE2 preimage in the deployment record. The proxy
creation bytes must come from the **published genesis** implementation, not
whatever `contracts/out` currently contains after a different build/profile.
The reviewed v32 seed embeds a 4,171-byte proxy creation blob in the initial
GenesisUpgrade runtime at `0x10001`; its observed byte offset is 7,381. Those
numbers identify this reviewed seed, not a rule for future releases: validate
the genesis hash and a unique, aligned bytecode match before extraction.

For this source route, `ComplexUpgrader` at `0x800f` remains the EVM CREATE2
caller through delegatecalls, with salt zero. ABI-encode the proxy constructor
`(logic, admin, data)` using implementation `0x10007`, the independently recorded
aliased ROOT Governance, and `initializeV3(name, symbol, assetRouter,
nativeTokenSentinel, baseTokenAssetId)` from the reviewed native-token inputs.
The current Tanenbaum initializer uses `Wrapped Syscoin`/`WSYS`, asset router
`0x10003`, native sentinel `0x1`, and the native asset ID derived from ROOT chain
5700, native token vault `0x10004` and that sentinel. Apply ordinary EVM CREATE2
to the **exact extracted creation bytes plus ABI constructor data**. The
ecosystem namespace salt selects ROOT Governance; it is not the wrapped-token
CREATE2 salt, and Gateway chain ID is not an extra constructor-preimage field.
Source inspection must also exclude an intervening force-replacement/reset of
the seeded implementation or native-token-vault state during conversion.

A narrowly checked Solidity IPFS-metadata comparison may establish source
correspondence and locate that unique embedded blob. It must never normalize,
strip or substitute metadata in the CREATE2 preimage or in live code acceptance:
the actual published bytes are the identity. A mutable Forge artifact with only
a different metadata digest can produce a different address and must be rejected.
Do not copy this rehearsal's address, Governance alias, asset ID or extracted
offset into a mainnet launch; rederive from its reviewed source and deployment.

After fresh Gateway startup and conversion, verify the chain/genesis identity,
ROOT Governance/native-asset bindings, NTV `WETH_TOKEN()` at `0x10004` and
GWAssetTracker `wrappedZKToken()` at `0x10010` against the independent pin. Also
attest exact proxy runtime bytes, EIP-1967 implementation/admin slots, exact
implementation code and token name/symbol/decimals/bridge/vault/asset getters.
Only then supply the pin for settlement-fee funding/migration. A source-derived
record is **not live attestation**; any mismatch stops use instead of replacing
the pin with a value discovered from the RPC receiving the transaction.

### Contract and client deployment

After the chain is live, run `zksys-l2-bootstrap.sh` for the current token,
ProxyAdmin, membership/weight registries, issuer, native staking vault and
canonical gas tank. Verify all proxy, role, receiver and immutable runtime
bindings. The persisted tank authority ends the first-boot exception; do not
disable its startup gate after bootstrap.

<!-- SYSCOIN: Anchor the approved issuance policy to a verified token receipt;
never publish a wall-clock guess or silently advance the original start. -->
`ZKSYS_ISSUER_START_TIME` is an absolute future Unix timestamp, bound in the
bootstrap manifest before broadcasts. For the approved exact 24-hour
post-token-deployment policy, use the receipt-anchor helper from PR #333:
leave that variable unset, run `zksys-l2-bootstrap.sh --token-prelude`, then run
the full bootstrap. It records and revalidates the exact token proxy deployment
receipt and derives start from its canonical block timestamp plus 86,400 seconds.
Persist the receipt/prelude/bootstrap manifests and later `startTime()` readback;
an interrupted recording may recover only that exact attested deployment hash.
Do not recompute the time on retries, use `now + 86400`, reuse v31's past value
or publish a start before its receipt/readback exists. Complete initial issuer
deployment before the anchored start; otherwise its initializer reverts. Never
roll the anchor forward: retries of an already deployed issuer retain the original
start. Defaults remain daily periods, a 365-day schedule year and a three-period
positive-weight activation delay.

<!-- SYSCOIN: This rehearsal delegates the mandatory Pali suite to the wallet,
not a duplicate server-side deployment. -->
For this public rehearsal the Pali wallet's Advanced settings owns deployment
and attestation of its nine mandatory infrastructure contracts, starting with
canonical EntryPoint v0.9. The server must not duplicate that suite. EntryPoint's
official artifact profile is optimizer 1,000,000, viaIR and default IPFS metadata;
Pali account/validator/recovery/factory artifacts use their separate optimizer-200,
metadata-free profile and matching wallet constants in
[the contracts guide](https://github.com/syscoin/zksync-os-server/blob/main/contracts/README.md).
Do not redeploy legacy custom EntryPoints/paymasters or use stale `contracts/out`.
An explicitly selected helper-based deployment is a different workflow: its
keystore interface uses `DEPLOYER_ACCOUNT` with `DEPLOYER_SIGNER` unset, unlike
the launcher. Map that environment separately and do not claim it deploys the
whole wallet suite. Supply exact hashes to `check-pali-deployment.sh` and attest
factory/EntryPoint/validator relationships, not only explorer verification.

Inventory Multicall3 and other required standard infrastructure separately.
For canonical `0xcA11bde05977b3631167028862bE2a173976CA11`, follow the
[official deployment instructions](https://github.com/mds1/multicall3#new-deployments),
check the deployer nonce and signed transaction gas limit, fund minimally and
verify the exact deployed runtime. Its publicly compromised deployment EOA is
not the project funding wallet. Opt-in service-V1 proof/reward contracts are not
enabled by token bootstrap and are not automatically part of mock proving.

Refresh EN source/build stamps, sequencer enode and direct RPC inputs, both
explorers, token branding, wallet, portal, bridge routes/start blocks and faucet
configuration from the new deployment outputs. Reset the faucet's in-memory
nonce state and fund its preserved dispenser on the new chain before opening
it. Old v31 faucet balances do not carry into the fresh chain; do not grant an
ad-hoc ZKSYS mint role to seed it.

### Persistent sequencer handoff (dated testnet reference)

<!-- SYSCOIN: Reproducible staging assets do not authorize service cutover or
replace canonical migration ownership, configuration, source or app guards. -->
The [2026-10-06 Tanenbaum reference assets](reference-assets/v32-public-tanenbaum-20261006/INSTALLATION-GATES.md)
pin source `f638db92e5c5ea31507e089b61f122fd95cf9083` and the explicitly dated
fresh runtime. They include two inactive systemd templates, a guarded edge
adapter recipe, eight offline tests and a staging record. **They are mock-testnet
references, not installed services or a completed launch. Never blindly copy
their paths, chain IDs, no-proofs flags or deployment inputs to mainnet.**

<!-- SYSCOIN: Preserve historical staging evidence rather than relabeling its
source or copied binary stamp as a newly qualified release. -->
That f638 bundle is a historical snapshot, **not installable against the later
d8 recovery release**. Before service installation, independently review an
updated bundle against the final approved source and private-validation gates,
regenerate normal build stamps and record actual final artifact hashes. The d8
CLI and Gateway native binary were rebuilt and qualified through the normal
helpers; this neither restamps the f638 references nor authorizes node start.

Follow the linked gates in order using the final reviewed bundle: source the
exact protected `launch.env.sh`, export `ZKSYNC_OS_SERVER_PATH` to its matching
approved checkout, and preserve the same approved deployment inputs.
Require the fresh canonical contracts/configs,
normal edge `build-prebuilt` and config-bound `exec-prebuilt -- --help` before
generating the adapter. Record actual final binary/stamp/script hashes; copied
Cargo caches and earlier Gateway hashes are not new build attestations.

The adapter preserves every canonical prefix byte (cookie refresh, context,
exact config path and execute-operator FD9 lock) and changes only the terminal
edge runner mode to `exec-prebuilt`. Source, binary, protocol, app and context
checks still run. Gateway uses its unmodified generated prebuilt start script.
Neither service may start while canonical migration/repair owns its temporary
Gateway process: finish/revalidate migration and final configs, confirm all
owned jobs exited and listeners are free, then obtain the explicit cutover and
installation approval. Attest Gateway before edge; recheck exact PID/socket,
genesis, live postimages and settlement bindings, not chain ID alone. The
references do not weaken old-deposit retirement or external-collateral gates.

Reviewed reference-asset SHA256 values:

| Reference asset | SHA256 |
| --- | --- |
| [Gateway unit](reference-assets/v32-public-tanenbaum-20261006/zksys-v32-gateway.service) | `757028d30fbbaef6c0812f4d4aa71e3d9b23d3a03f09c094a1e080a0502722ce` |
| [Edge unit](reference-assets/v32-public-tanenbaum-20261006/zksys-v32-edge.service) | `8d284e7f88d1b60a97ea5662c84a348843d7e2311418f075878bd13e0aaecfca` |
| [Adapter recipe](reference-assets/v32-public-tanenbaum-20261006/generate-edge-prebuilt-adapter.py) | `a44ce5c40a309f224cf0cceaf8443d03af417683da05d57130df40b9a04f179c` |
| [Offline tests](reference-assets/v32-public-tanenbaum-20261006/test-edge-prebuilt-adapter.py) | `38b12b6da896d9a0147750ccbe400b9ed0d81c60e435821a82a57b3f8bafb790` |

### Acceptance record and mainnet boundary

Keep an operator-only record of actual commands, pinned inputs, resolved reset
targets, approvals, failures and repairs. Keep proposed/not-run checks separate
from completed checks. Publish the final chain identities, contract address/
runtime manifest, successful transaction receipts and issuance start without
publishing secrets or credentialed endpoints.

Before declaring the public replacement complete, verify:

- Root/Gateway/edge chain and genesis identities, explicit testnet verifier
  marker and compiled address bindings agree.
- Both nodes and DA are healthy and mock batches commit/prove/execute; do not
  report this as real SNARK verification.
- Token/registry/issuer/staking/gas-tank wiring and current Pali/native-gas and
  tank-backed account flows pass their deployment checks.
- Both ENs sync the new chain and use the direct sequencer upstream, both
  explorers index fresh state, and clients/bridge/faucet use the new addresses.
- Intended public endpoints work while Gateway/debug/admin/prover boundaries
  retain their access restrictions. An ordinary restart uses the attested
  binaries without rebuilding or reusing stale stamps.
- Public endpoints identify the release as a fresh mock-proof testnet.

Mainnet is a separate rollout: this destructive reset and mock verifier recipe
are forbidden there. Require production proofs and the current deployed
verifier/VK, chainlocked finality, reviewed governance/activation and asset
reconciliation, durable encrypted off-host recovery, and an approved cutover/
rollback plan. Do not copy testnet one-confirmation, custody or reset choices
into a mainnet launch. A completed mock-testnet rehearsal does not close those
release gates. Before destroying any old runtime or removing hot keys, prove
continued signing ability for its deposit administrator and Governance; retain
the corresponding encrypted recovery credentials and test their restoration.
Do not repeat a testnet's missing-old-owner custody gap in mainnet retirement.

## Canonical command

Start local Syscoin RPC bridge first (Tanenbaum/Mainnet launcher expects local `L1_RPC_URL`):

```bash
./syscoind --testnet -server=1 -daemon=1 \
  -gethcommandline=--http \
  -gethcommandline=--http.addr=127.0.0.1 \
  -gethcommandline=--http.port=8545 \
  -gethcommandline=--http.api=eth,net,web3 \
  -gethcommandline=--http.vhosts=localhost,127.0.0.1
```

Keep the local RPC bound to loopback and do not enable wildcard CORS. Only add
extra namespaces such as `txpool` or `debug` for a short, trusted debugging
session, then restart the node with the restricted API list above.

Run from a `zksync-os-server` clone:

```bash
cd /path/to/zksync-os-server
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.tanenbaum.io
export PROVER_API_AUTH_PASSWORD=...
export FUNDER_SIGNER=account
export FUNDER_ACCOUNT_NAME=funder
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --migrate-edge
```

Mainnet:

```bash
export L1_RPC_URL=http://127.0.0.1:8545
export GATEWAY_ARCHIVE_L1_RPC_URL=https://rpc.syscoin.org
export PROVER_API_AUTH_PASSWORD=...
export FUNDER_SIGNER=account
export FUNDER_ACCOUNT_NAME=funder
bash scripts/gateway-launch/run-gateway-launch.sh --l1 mainnet --migrate-edge
```

<!-- SYSCOIN: Keep historical startup and settlement-proof authentication on the configured
archive trust boundary instead of silently falling back to a potentially pruned live provider. -->
`L1_RPC_URL` is written as the normal OS-server L1 provider and can point at the
local sysgeth used for live traffic. `GATEWAY_ARCHIVE_L1_RPC_URL` is written as
the archive L1 provider used for historical committed-batch startup reads and
hash-pinned Gateway-to-L1 settlement-proof authentication. Point it at an
archive-capable Syscoin L1 RPC unless the local `L1_RPC_URL` node was synced in
archive mode from genesis. When this archive URL is configured, historical
proof reads fail closed on that endpoint rather than retrying against the live
provider; the live provider is the fallback only when no archive is configured.

Do not pass production private keys as raw command-line arguments. Import the
launch signer into Foundry's keystore first with
`cast wallet import funder --interactive`, or use the `keystore`, `ledger`,
`trezor`, `aws`, or `gcp` backend. On Tanenbaum/Mainnet, deployer and governor
signing default to the same funder signer/account unless `DEPLOYER_*` or
`EDGE_GATEWAY_GOVERNOR_*` overrides are set. `FUNDER_PRIVATE_KEY` and
`DEPLOYER_SIGNER=private-key` are local/disposable-network fallbacks only; on
Tanenbaum/Mainnet they are rejected unless
`GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true` is set explicitly.
The `--migrate-edge` flag is required for the normal full launch because it pauses deposits and finalizes the edge-chain migration to Gateway settlement. If omitted, the launcher stops after edge-chain initialization and can be resumed later with the same command plus `--migrate-edge`.

Optional log override:

```bash
bash scripts/gateway-launch/run-gateway-launch.sh --l1 tanenbaum --log /tmp/gateway-launch.log
```

To intentionally continue from an existing ecosystem directory that already has
`ZkStack.yaml`, pass `--reuse-ecosystem`. Without this flag, the launcher fails
instead of silently bypassing wallet creation controls.

## Checkpoint model

State file:

```text
The launcher prints the checkpoint state path. Fresh deployments keep it in a
private `.gateway-launch-state` directory beside `$GATEWAY_DIR`, so checkpointing
does not pre-create zkstack's target ecosystem directory. Existing legacy state
under `$GATEWAY_DIR/.gateway-launch/state.json` is still honored.
```

Ordered checkpoints:

1. `gl.workspace`
2. `gl.ecosystem`
3. `gl.wallets_funded`
4. `gl.l1_ecosystem_deployed`
5. `gl.gateway_chain_inited`
6. `gl.gateway_settlement`
7. `gl.os_configs_gateway`
8. `gl.edge_chain_inited`
9. `gl.migration`
10. `gl.os_configs_final`

Behavior:

- A checkpoint is skipped only when state says `passed` **and** probe validation passes.
- Any failure marks checkpoint `blocked` and stops the run.
- Resume is automatic on the same command after repair.
- Resume is rejected if fingerprinted launch context changed (network/chain/sha/env mismatch).

## Repair workflow

Inspect state:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum status
```

Repair one blocked checkpoint:

```bash
bash scripts/gateway-launch/gateway-launch-repair.sh --l1 tanenbaum repair gl.edge_chain_inited
```

Then rerun the canonical launcher command.

## What the launcher does

- Creates a new ecosystem, or reuses one only when `--reuse-ecosystem` is explicit.
- Funds required wallets.
- Deploys/initializes gateway contracts and chain.
- Converts gateway settlement.
- Creates/initializes edge chain.
- Runs edge migration to gateway settlement.
- Ensures DA pair correctness and deposit unpause via migration script guards.
- Generates final `os-server-configs` launchers.

> **SYSCOIN Gateway-head trust boundary:** Generated edge-chain configs set
> `l1_watcher.optimistic_gateway_head: true`. This lets Gateway commit, execute, interop-root,
> and MessageRoot-proof paths use the current Gateway head without waiting for Gateway-to-L1
> settlement. Imported roots cannot be rolled back after destination execution, so production
> deployments should expose only a quorum-canonized Gateway head (OpenRaft) through a trusted RPC.
> Leave the option `false` for a reorgable Gateway RPC; direct-L1 watchers retain their configured
> confirmation depth either way.

## Configure replay archive before production start

Before the first mainnet node start, enable an independent replay archive in
each final OS-server config that you intend to rely on for recovery. External
nodes keep their own `block_replay_wal` once fully synced, but explorer EN
disks are operational redundancy, not a cold backup. Keep at least one
off-node replay archive for Gateway and zksys so replay records survive local
RocksDB loss, host rebuilds, or accidental EN database wipes.

For production, prefer encrypted S3 or another S3-compatible object store:

```yaml
replay_archive:
  type: S3WithCredentialFile
  bucket_base_url: syscoin-mainnet-replay-archive
  s3_credential_file_path: /etc/zksync-os/replay-archive-s3-credentials
  endpoint: null
  region: us-east-2
  encryption:
    type: AgeX25519
    recipient: age1...
```

The node only needs the age public recipient key. Store the corresponding
`AGE-SECRET-KEY-...` separately from the hot node and use it only for recovery.
If S3 is not ready at launch, enable a filesystem replay archive on a durable
path and back that path up off-host:

```yaml
replay_archive:
  type: FileSystem
  root_path: /var/lib/zksync-os/replay_archive
  encryption:
    type: AgeX25519
    recipient: age1...
```

Do not treat `replay_archive: { type: Noop }` as sufficient for mainnet
operations. Test recovery before relying on the archive: download the archive
objects, rebuild `db/block_replay_wal` with `replay_archive_recovery`, and
start a node from the recovered replay WAL using a known canonical anchor.

<!-- SYSCOIN: Explicit release policy; the current non-Ethereum root-chain allowance is retained. -->
Syscoin currently retains the pinned contracts' **30-day maximum commit age**, including on
mainnet. This is an outage/catch-up allowance for the first block timestamp when a batch is
committed, not a target settlement delay, proof-expiry deadline, or DA-retention guarantee.
Monitor settlement lag well below that limit. It does not change the separate priority-mode timer.

Before production, explicitly attest the chosen rollup permanence and recovery policy for each
chain. The launcher leaves `isPermanentRollup` false; until made permanent, the chain admin can
change the DA pair without the restrictive Syscoin rollup DA manager's allowlist. Making a chain
permanent is irreversible under the reviewed contracts and has migration/priority-mode
prerequisites, so this guide does not enable it automatically. Complete real-proof validation
before making that governance decision. Also provision the [ordinary idle-tail heartbeat](../design/prover_api.md#ordinary-idle-tails)
if settlement must progress without application traffic; it is opt-in and needs a dedicated wallet.

## Start nodes after successful launch

<!-- SYSCOIN: First Gateway boot precedes Edge creation in the canonical
launcher; deployment staging must follow that producer/consumer ordering. -->
The canonical launcher's first supervised Gateway start is an earlier,
Gateway-only lifecycle step: it generates Gateway configs with
`MATERIALIZE_EDGE_CONFIG=false`, starts Gateway, then initializes/migrates Edge.
Both final config sets are generated only afterward. A first-boot prestart
gate must bind the exact reviewed source and Gateway config/genesis/start script,
canonical native binary and source stamp; it cannot require not-yet-generated
final Edge artifacts. Keep the later service-publication gate separate and
strict: both final native/config/genesis identities, completed migration and
serialized acceptance still need qualification before public cutover.

When only Gateway config generation needs repair, the supported
`gateway-launch-repair.sh --l1 tanenbaum repair gl.os_configs_gateway` route
generates and live-validates that Gateway-only output without starting Gateway.
Do not manufacture a passed checkpoint or fake Edge artifacts to satisfy
prestart. An operator staging gate that demands final Edge output before this
first Gateway start is an ordering error, not a protocol defect or a reason to
bypass the canonical lifecycle. Source/build ancestor permissions and ordinary
Cargo binary/stamp provenance must also qualify before installing a staged
publisher; a packet or syntax test alone is not deployment readiness.

```bash
"$GATEWAY_DIR/os-server-configs/gateway/start-node.sh"
"$GATEWAY_DIR/os-server-configs/zksys/start-node.sh"
```

## Post-deploy opsec

After `gl.os_configs_final` passes and both final node config directories exist,
the hot runtime only needs the operational node configs and chain artifacts:

```text
$GATEWAY_DIR/os-server-configs/*/config.yaml
$GATEWAY_DIR/os-server-configs/*/start-node.sh
$GATEWAY_DIR/os-server-configs/*/contracts.yaml
$GATEWAY_DIR/os-server-configs/*/genesis.json
```

The `config.yaml` files contain the runtime operator keys:

```text
operator_commit_sk
operator_prove_sk
operator_execute_sk
```

Do not delete those final `config.yaml` files unless you are intentionally
rotating/rebuilding runtime operator keys. Keep the Syscoin DA wallet material
needed by the local Syscoin node if the node will publish blobs.

Before removing launch-time keys, make an encrypted operational backup on an
operator-controlled encrypted or off-host backup path. Do not create plaintext
secret archives under `/tmp`.

The operational backup should contain the chain-scoped Gateway and zksys wallet
files plus the final OS-server runtime configs. These are the management and
runtime keys that matter after launch:

```text
$GATEWAY_DIR/chains/gateway/configs/wallets.yaml
$GATEWAY_DIR/chains/zksys/configs/wallets.yaml
$GATEWAY_DIR/os-server-configs/gateway/config.yaml
$GATEWAY_DIR/os-server-configs/zksys/config.yaml
```

<!-- SYSCOIN: Distinct ecosystem/old governance keys are required custody, not
redundant files that may be deleted after checking only runtime operators. -->
The final `config.yaml` files contain the operator private keys used by the
running nodes. The chain-scoped `wallets.yaml` files contain deployer, governor,
fee and operator keys needed for repair/governance/migration. Inventory ROOT
Governance and chain deposit-admin ownership separately: an ecosystem/root
wallet or an external encrypted account may hold a distinct controller key not
present in a chain wallet. Include every such required signer in the encrypted
recovery plan and verify its restored address. Avoid merely duplicate files such as
`$GATEWAY_DIR/configs/wallets.yaml`, `$GATEWAY_DIR.wallets.yaml`, hidden
`.*wallets.yaml` copies or `*.backup` files only after proving that they contain
no unique required controller. Exporting unrelated keys increases recovery risk;
omitting a distinct old Governance key can make safe deposit retirement impossible.

```bash
(
set -euo pipefail
umask 077
BACKUP_DIR="${BACKUP_DIR:?set BACKUP_DIR to an encrypted/off-host backup directory}"
install -d -m 700 "$BACKUP_DIR"
backup_archive="$BACKUP_DIR/gateway-launch-secrets-$(date -u +%Y%m%dT%H%M%SZ).tar.gz.gpg"

secret_list="$(mktemp "$BACKUP_DIR/gateway-launch-secrets.XXXXXX.list")"
trap 'rm -f "$secret_list"' EXIT

shopt -s nullglob
secret_paths=(
  "$GATEWAY_DIR"/configs/wallets.yaml
  "$GATEWAY_DIR"/chains/gateway/configs/wallets.yaml
  "$GATEWAY_DIR"/chains/zksys/configs/wallets.yaml
  "$GATEWAY_DIR"/os-server-configs/gateway/config.yaml
  "$GATEWAY_DIR"/os-server-configs/zksys/config.yaml
  "$HOME"/.foundry/*.password
)

for path in "${secret_paths[@]}"; do
  printf '%s\n' "$path" >> "$secret_list"
done

if [ -d "$HOME/.foundry/keystores" ]; then
  printf '%s\n' "$HOME/.foundry/keystores" >> "$secret_list"
fi

sort -u -o "$secret_list" "$secret_list"
[ -s "$secret_list" ] || {
  echo "no launch-time secret files found; refusing to create an empty backup" >&2
  exit 1
}

tar -czf - -T "$secret_list" \
  | gpg --symmetric --cipher-algo AES256 \
      --output "$backup_archive"
chmod 600 "$backup_archive"

gpg --decrypt "$backup_archive" \
  | tar -tzf - >/dev/null
printf 'encrypted backup created: %s\n' "$backup_archive"
)
```

Keep the encrypted archive and its passphrase separated. The command exits on
backup or verification failure. Listing the archive is not a signer-restoration
test: explicitly include any externally referenced controller wallet/account,
restore/decrypt it securely and compare its derived address to every required
old/current onchain owner. Do not delete source keys unless those checks pass
and the encrypted backup has been copied to durable storage.

After copying that backup off the hot host, remove launch-time wallet files,
duplicate wallet copies, and Foundry signer material. Do not remove final
`config.yaml` files unless you are intentionally rotating/rebuilding runtime
operator keys.

```bash
rm -f "$GATEWAY_DIR".wallets.yaml
rm -f "$GATEWAY_DIR"/*.wallets.yaml
rm -f "$GATEWAY_DIR"/.*wallets.yaml
rm -f "$GATEWAY_DIR"/*wallets.yaml.*
rm -f "$GATEWAY_DIR"/.*wallets.yaml.*
rm -f "$GATEWAY_DIR"/configs/wallets.yaml
rm -f "$GATEWAY_DIR"/configs/wallets.yaml.*
rm -f "$GATEWAY_DIR"/chains/*/configs/wallets.yaml
rm -f "$GATEWAY_DIR"/chains/*/configs/wallets.yaml.*
rm -f "$GATEWAY_DIR"/os-server-configs/*/wallets.yaml
rm -f "$GATEWAY_DIR"/os-server-configs/*/wallets.yaml.*

rm -f "$HOME"/.foundry/*.password
rm -rf "$HOME"/.foundry/keystores
```

This is safe for normal node restarts, but it intentionally removes hot
repair/upgrade/admin capability. Restore the backup or re-import the relevant
signers before running checkpoint repair, governance, migration, or upgrade
commands.

For internet-facing prover access, keep the node prover APIs bound to
`127.0.0.1` and terminate HTTPS in the host nginx that already fronts RPC and
explorer traffic. The config generator writes nginx vhost files next to the node
configs:

```bash
"$GATEWAY_DIR/os-server-configs/gateway/prover-api.nginx.conf"
"$GATEWAY_DIR/os-server-configs/zksys/prover-api.nginx.conf"
```

If Gateway and zksys are on the same host, install the combined generated file:

```bash
"$GATEWAY_DIR/os-server-configs/install-prover-api-nginx.sh"
```

This uses the same host nginx TLS setup as RPC and explorer. Certificates for
the prover hostnames must already be provisioned by the host's normal nginx /
Let's Encrypt flow, with certs available at
`/etc/letsencrypt/live/<hostname>/`.

If they run on separate hosts, install the per-chain `prover-api.nginx.conf` on
the corresponding host instead. Override `GATEWAY_PROVER_API_DOMAIN` and
`EDGE_PROVER_API_DOMAIN` before generating configs if the prover hostnames are
not `prover-gw.dev11.top` and `prover-zk.dev11.top`.

Then start Airbender provers with a credentialed HTTPS sequencer URL, for
example `https://syscoin-prover:...@prover-gw.dev11.top` for Gateway and
`https://syscoin-prover:...@prover-zk.dev11.top` for zksys.

<!-- SYSCOIN: Remote prover admission requires a buffering protected transport boundary. -->
Generated configs and the node bind the application prover API to `127.0.0.1`; a
non-loopback bind fails closed even when Basic Auth is configured. The generated
nginx HTTPS vhost is the supported remote ingress and can fully spool each bounded
response while serving a slow worker, so remote client pace cannot retain all
node-side pick permits. If nginx runs in a container, give it access to the host
loopback listener through the same network namespace or host networking; do not
replace this boundary with a direct plaintext bind or an unbuffered tunnel. The
public vhost intentionally returns 404 for tokenless peek/failed-proof debugging;
run those diagnostics from the trusted node host through loopback.

### Prover restart recovery

<!-- SYSCOIN: Operators must preserve the durable prover authority while bounded recovery uses the
protected worker surface before public node readiness. -->
On a proving-node restart, expect the node to advance through
`Recovering -> Drainable -> Ready`:

1. `Recovering` reserves the prover address without listening while canonical committed metadata,
   accepted-wrapper journal records, and their replay ownership are validated. Public node
   readiness remains HTTP 503.
2. `Drainable` opens only the loopback prover service behind its buffering HTTPS proxy, when enabled.
   Public readiness remains HTTP 503 while durable FRI files are fed in canonical order
   through the bounded RAM job map and external Airbender or in-process fake workers drain them.
3. `Ready` means every startup batch has a canonical classification and explicit owner or recovery
   disposition. It does not require every recovered FRI to have completed SNARK wrapping; public
   readiness may leave HTTP 503 only after this phase and the independent database gate are ready.

The default `prover_api.max_assigned_batch_range: 256` is an in-memory operating-span bound. It
does not need to cover the complete restart backlog: the configured proof-storage directory is the
durable overflow queue, and recovery admits more work as workers free RAM-map capacity.

Preserve each chain's complete configured `prover_api.proof_storage.path` across ordinary restarts,
including its `snark_journal` subdirectory. Back up and restore that tree as one unit; do not delete
FRI files to force readiness or restore the journal without its companion proof storage. Do not
change `PROVER_MODE`, the effective `GATEWAY_PROVER_MODE`, or the underlying fake-prover flags while
retaining this chain and recovery state. Clearing launcher checkpoints alone is not a proving-mode
migration.

<!-- SYSCOIN: The accepted-wrapper journal applies the same availability budget live and on boot. -->
The SNARK journal admits at most 256 MiB per record, 8 GiB in aggregate, and 65,536 ranges. Reaching
the aggregate cap during an extended L1/Gateway confirmation outage makes new wrapper persistence
retryable; it does not authorize deleting files or abandoning leases. Capacity returns as confirmed
records are durably retired, and an identical retry does not consume the budget twice.

Recovery fails closed instead of publishing `Ready` when canonical metadata or ownership is
inconsistent, a future non-V32 proving-version boundary is unsupported, or a real singleton can
never acquire a compatible companion. A normal V32 tail singleton remains pending for its next
canonical same-version batch.

If the explorer runs on a separate host, keep sequencer/node RPC bound locally
or behind the host reverse proxy and allowlist only the explorer server's public
IP at nginx/firewall level. Do not expose unrestricted Gateway node RPC directly
to the internet.

For zksys public RPC, prefer running external nodes on the explorer host instead
of exposing the sequencer RPC directly. The normal topology is two zksys EN
processes on the explorer host:

- public EN: `rpc.enable_debug_namespace=false`, local nginx upstream for
  `rpc-zk.tanenbaum.io`
- internal/debug EN: `rpc.enable_debug_namespace=true`, used by Blockscout only

Each node process needs its own `network.secret_key`, `network.port`,
`general.rocks_db_path`, and `rpc.address`. Both ENs use the same trusted
sequencer boot node:

```text
network.boot_nodes = "enode://<zksys-main-peer-id>@<sequencer-host>:3060"
```

The sequencer zksys node must have p2p enabled first. Its private
`network.secret_key` stays on the sequencer; ENs only receive the public enode.
Use the sequencer helper to create/reuse that key and patch the zksys config:

```bash
cd scripts/explorer/blockscout

SEQUENCER_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
./enable-zksys-sequencer-p2p.sh
```

The helper prints `MAIN_NODE_ENODE=...`. By default it does not restart the
sequencer; set `RESTART_ZKSYS=1` after confirming the maintenance window.

The Blockscout helper can install the two zksys ENs after launch:

```bash
cd scripts/explorer/blockscout

REMOTE_HOST="ubuntu@<explorer-host>" \
SEQUENCER_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
MAIN_NODE_ENODE="enode://<zksys-main-peer-id>@<sequencer-host>:3060" \
./deploy-zksys-en-rpc.sh
```

The generated EN configs set `general.main_node_rpc_url` to the direct
sequencer RPC (`http://<sequencer-host>:3050` by default). After
`rpc-zk.tanenbaum.io` points at the public EN, do not use that DNS name for
`main_node_rpc_url`; otherwise EN transaction forwarding loops back into the EN
instead of reaching the sequencer.

Then install the public zksys RPC vhost on the explorer host, pointing at the
public EN and leaving the existing Gateway RPC vhost alone:

```bash
RPC_NGINX_REMOTE_HOST="ubuntu@<explorer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
ZKSYS_RPC_UPSTREAM="http://127.0.0.1:3050" \
RPC_NGINX_INCLUDE_ZKSYS=1 \
RPC_NGINX_INCLUDE_GATEWAY=0 \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

After the public zksys RPC has moved to the explorer host, remove the old zksys
RPC vhost from the sequencer while keeping Gateway RPC private/allowlisted:

```bash
RPC_NGINX_REMOTE_HOST="ubuntu@<sequencer-host>" \
SSH_KEY_PATH="/path/to/ssh-key" \
RPC_NGINX_INCLUDE_ZKSYS=0 \
RPC_NGINX_INCLUDE_GATEWAY=1 \
RPC_NGINX_REMOVE_ZKSYS=1 \
GATEWAY_RPC_ALLOWLIST="<explorer-host-ip-or-cidr>,<admin-ip-or-cidr>" \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

Also restrict the sequencer's raw zksys RPC port to the explorer host only; ENs
need this path for transaction forwarding, but it should not be publicly
reachable:

```bash
# Run on the sequencer host.
sudo iptables -I INPUT 1 -p tcp -s <explorer-host-ip> --dport 3050 -j ACCEPT
sudo iptables -I INPUT 2 -p tcp --dport 3050 -j DROP

# Persist the rules using your host firewall manager. On Ubuntu without another
# firewall manager, netfilter-persistent can save them:
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y iptables-persistent netfilter-persistent
sudo netfilter-persistent save
sudo systemctl enable netfilter-persistent
```

The Blockscout deployment helper includes a host nginx installer for the normal
Tanenbaum topology:

```bash
cd scripts/explorer/blockscout

# Deploy or refresh the explorer containers.
REMOTE_HOST="<ssh-user>@<explorer-host>" ./deploy-remote.sh zksys
REMOTE_HOST="<ssh-user>@<explorer-host>" ./deploy-remote.sh gateway

# Optional: enable Blockscout sensitive API endpoints, such as token metadata imports.
API_SENSITIVE_ENDPOINTS_KEY="<random-secret>" \
REMOTE_HOST="<ssh-user>@<explorer-host>" ./deploy-remote.sh zksys

# Install RPC vhosts on the node host after rpc-zk/rpc-gw DNS points there.
RPC_NGINX_REMOTE_HOST="<ssh-user>@<node-host>" \
GATEWAY_RPC_ALLOWLIST="<blockscout-ip-or-cidr>,<admin-ip-or-cidr>" \
LETSENCRYPT_EMAIL="<ops email>" \
RPC_NGINX_ENABLE_TLS=1 \
./deploy-rpc-nginx.sh
```

`rpc-zk.tanenbaum.io` is installed as the public zksys RPC. `rpc-gw.tanenbaum.io`
is installed as a private Gateway RPC that only allows localhost plus
`GATEWAY_RPC_ALLOWLIST` entries, typically the Blockscout host and operator
admin IPs. The Gateway Blockscout env hides RPC docs/links and disables the
Next.js proxy so the public explorer API remains available without turning the
explorer into a public Gateway RPC passthrough.

The generated `start-node.sh` now preflights the open-file limit before starting the node:

- it tries to raise `ulimit -n` to `1048576`
- it warns if the resulting limit is below the recommended `131072`
- it fails fast if the resulting limit is below `65536`

If the script cannot raise the limit high enough, increase the shell / service hard limit first (for example via systemd `LimitNOFILE`, Docker `--ulimit`, or host user limits).

## Important env vars

<!-- SYSCOIN: Generated prover settings separate durable restart overflow from bounded resident
work and bind recovery to one verifier mode. -->

| Variable | Purpose |
|---|---|
| `L1_RPC_URL` | Required HTTP(S) JSON-RPC endpoint used for broadcasts (expected: local Syscoin node/proxy, e.g. `http://127.0.0.1:8545`) |
| `GATEWAY_DIR` | Ecosystem workspace path (default `~/gateway`) |
| `REUSE_ECOSYSTEM` | Set to `true` only when intentionally reusing an existing `$GATEWAY_DIR/ZkStack.yaml`; equivalent to `--reuse-ecosystem` |
| `MIGRATE_EDGE` | Set to `true` only when intentionally pausing deposits and migrating/finalizing the edge chain; equivalent to `--migrate-edge` |
| `GATEWAY_ARCHIVE_L1_RPC_URL` | Recommended runtime archive RPC URL for gateway node startup/migration reads and historical settlement-proof authentication (if unset, falls back to `L1_RPC_URL`) |
| `GATEWAY_WRAPPED_BASE_TOKEN_ADDRESS` | Independently verified Gateway wrapped base-token proxy used by settlement-fee provisioning. The launcher never derives this native-value recipient from the RPC that receives the signed transaction. A fresh staged launch may omit it while `MIGRATE_EDGE=false`; obtain and verify it from the deployment record, then supply the pin before rerunning with `--reuse-ecosystem --migrate-edge`. |
| `PROVER_API_BIND_HOST` | Prover API bind host for generated node configs; must remain `127.0.0.1` and remote access must use the generated buffering HTTPS proxy |
| `GATEWAY_PROVER_API_DOMAIN` / `EDGE_PROVER_API_DOMAIN` | Hostnames for generated nginx prover API vhosts; defaults are `prover-gw.dev11.top` and `prover-zk.dev11.top` |
| `PROVER_API_AUTH_USER` / `PROVER_API_AUTH_PASSWORD` | Basic Auth credentials for remote prover API access; generated configs require a password of at least 32 characters (use a URL-safe secret such as `openssl rand -hex 32`) |
| `PROVER_BATCH_WITH_PROOF_CAPACITY_BYTES` | Durable accepted-FRI overflow cap; defaults to 8 GiB and cannot be lower in GPU mode. Size it for the expected disk backlog independently of the `max_assigned_batch_range: 256` RAM-span bound. |
| `FUNDER_SIGNER` | Funder signer backend: `account` (default on Tanenbaum/Mainnet), `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `FUNDER_ACCOUNT_NAME` | Foundry keystore account name when `FUNDER_SIGNER=account` (default `funder`) |
| `FUNDER_KEYSTORE` | Keystore file path when `FUNDER_SIGNER=keystore` |
| `FUNDER_PASSWORD_FILE` | Optional keystore password file passed to Cast without exposing the password in argv |
| `FUNDER_PRIVATE_KEY` | Local/disposable-network fallback for `FUNDER_SIGNER=private-key`; rejected on Tanenbaum/Mainnet by default |
| `GATEWAY_FUND_WALLETS_PATHS` | Optional extra `wallets.yaml` paths to fund (colon-separated) |
| `PROVER_MODE` | `gpu` (default) or `no-proofs`; do not change it across a restart that retains chain and proof-recovery state |
| `SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER` | Explicit upgrade-script opt-in for retaining the zkOS type-3 mock wrapper; defaults to `false` and must never be set for production |
| `PROTOCOL_VERSION` | Default `v32.0` |
| `GATEWAY_CHAIN_ID` | Gateway / zkSYS chain id used by ecosystem and node config generation |
| `ZKSYS_L2_CREATE2_DEPLOYER` | Deterministic L2 CREATE2 deployer for canonical zkSYS; defaults to `0x4e59b44847b379578588920cA78FbF26c0B4956C` |
| `ZKSYS_L2_TOKEN_ADMIN_ADDRESS` | Required on mainnet and whenever `ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE=true`; initial token role admin and default owner of deterministic zkSYS `ProxyAdmin` contracts; also used during L1 launch to derive deterministic zkSYS L2 addresses and the v32 `zk_token_asset_id` for canonical L2 zkSYS |
| `ZKSYS_L2_PROXY_ADMIN_SALT` | Optional bytes32 salt for the deterministic zkSYS `ProxyAdmin` deployment |
| `ZKSYS_L2_TOKEN_IMPL_SALT` / `ZKSYS_L2_TOKEN_PROXY_SALT` | Optional bytes32 salts for deriving the canonical L2 zkSYS implementation/proxy addresses |
| `ZKSYS_L2_RPC_URL` | Required by `scripts/gateway-launch/zksys-l2-bootstrap.sh` to deploy the L2 zkSYS suite after the chain is live |
| `ZKSYS_L2_DEPLOYER_SIGNER` | L2 bootstrap signer backend: `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key`; defaults to `DEPLOYER_SIGNER`, then `FUNDER_SIGNER`, then `account` unless `ZKSYS_L2_DEPLOYER_PRIVATE_KEY` is set |
| `ZKSYS_L2_DEPLOYER_ACCOUNT_NAME` / `ZKSYS_L2_DEPLOYER_KEYSTORE` / `ZKSYS_L2_DEPLOYER_PASSWORD_FILE` | Optional L2 bootstrap signer inputs for account or keystore modes; fall back to the matching deployer/funder values when unset |
| `ZKSYS_L2_DEPLOYER_PRIVATE_KEY` | Local/disposable-network fallback for `ZKSYS_L2_DEPLOYER_SIGNER=private-key`; rejected on Tanenbaum/Mainnet unless `GATEWAY_ALLOW_INSECURE_PRIVATE_KEY_ARGV=true` |
| `ZKSYS_DEPLOY_L1_REGISTRY_BRIDGE` | Deploy the L1/NEVM registry bridge during `gateway-deploy-l1.sh`; defaults to `true` |
| `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_OWNER_ADDRESS` | Optional owner for the L1 registry bridge `ProxyAdmin`; defaults to `ZKSYS_L2_TOKEN_ADMIN_ADDRESS` |
| `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_ADMIN_SALT` / `ZKSYS_L1_REGISTRY_BRIDGE_IMPL_SALT` / `ZKSYS_L1_REGISTRY_BRIDGE_PROXY_SALT` | Optional bytes32 salts for deterministic L1 registry bridge proxy admin, implementation, and proxy deployments through the L1 CREATE2 factory |
| `ZKSYS_L1_REGISTRY_BRIDGE_NEVM_START_BLOCK` | Syscoin `nNEVMStartBlock` used to convert absolute UTXO collateral heights into NEVM-local seniority age; defaults to `1317500` |
| `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT1` / `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_HEIGHT2` | Effective post-NEVM seniority thresholds; default `210240` / `525600` |
| `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL1_BPS` / `ZKSYS_L1_REGISTRY_BRIDGE_SENIORITY_LEVEL2_BPS` | Seniority bonuses in basis points; default `0` / `0` for first-year launch without seniority multiplier |
| `ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS` | Optional L1 registry bridge address override for the L2 bootstrap. If unset/zero, `zksys-l2-bootstrap.sh` reads the address persisted by `gateway-deploy-l1.sh` in `configs/contracts.yaml` |
| `ZKSYS_L2_REGISTRY_IMPL_SALT` / `ZKSYS_L2_REGISTRY_PROXY_SALT` | Optional bytes32 salts for deterministic L2 membership fact registry implementation/proxy deployments; the proxy address is the operational registry address wired to the L1 bridge |
| `ZKSYS_L2_WEIGHT_REGISTRY_IMPL_SALT` / `ZKSYS_L2_WEIGHT_REGISTRY_PROXY_SALT` | Optional bytes32 salts for deterministic L2 reward weight registry implementation/proxy deployments; the proxy address is wired as the membership registry receiver |
| `ZKSYS_L2_ISSUER_IMPL_SALT` / `ZKSYS_L2_ISSUER_PROXY_SALT` | Optional bytes32 salts for deterministic L2 issuer implementation/proxy deployments; the proxy address receives the token minter role |
| `ZKSYS_L2_STAKING_VAULT_IMPL_SALT` / `ZKSYS_L2_STAKING_VAULT_PROXY_SALT` | Optional bytes32 salts for deterministic L2 native SYS staking vault implementation/proxy deployments; the proxy address receives the reward weight updater role |
| `ZKSYS_ISSUER_START_TIME` | Required by L2 bootstrap; UNIX timestamp when algorithmic zkSYS issuance periods begin |
| `ZKSYS_ISSUER_PERIOD_SECONDS` | Issuance period length; defaults to `86400`; must multiply with `ZKSYS_ISSUER_PERIODS_PER_YEAR` to exactly `365 days` |
| `ZKSYS_ISSUER_PERIODS_PER_YEAR` | Number of issuance periods in each schedule year; defaults to `365`; must multiply with `ZKSYS_ISSUER_PERIOD_SECONDS` to exactly `365 days` |
| `ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS` | Reward-weight activation delay for positive native stake and Sentry Node weight changes; defaults to `3` periods and must be `1..7` |
| `ZKSYS_L2_GAS_TANK_SALT` | Optional bytes32 salt for the deterministic L2 zkSYS gas tank deployment; the tank is granted the token burn role and its address is written to `l2.zksys_gas_tank_addr` |
| `SYSCOIN_REQUIRE_GAS_TANK` | Optional explicit gas-tank presence gate (`0` or `1`). Leave unset for the first canonical edge-chain boot so the node can start with the immutable published address before `zksys-l2-bootstrap.sh` deploys the tank. After bootstrap attests and persists a nonzero `l2.zksys_gas_tank_addr`, the canonical main-node runner forces this gate to `1` on every launch (even if an operator supplies `0`). Fresh external nodes retain the missing-code allowance while catching up, always reject a wrong nonempty runtime, and may set `1` once synced past deployment. |
| `ZKSYNC_ERA_PATH` | Optional custom era checkout; otherwise launcher manages pinned workspace |
| `ZKSYNC_OS_DEV_PATH` | Optional custom official final-v0.4 `zksync-os` checkout for the single canonical Syscoin patch. Otherwise the launcher creates an isolated checkout under `$GATEWAY_DIR/.gateway-launch/zksync-os/<workspace>/canonical/` |
| `ALLOW_SHARED_ZKSYNC_OS_DEV_PATH` | Set to `true` only for local development when `ZKSYNC_OS_DEV_PATH` is used. Deployment builds leave it unset and use an isolated disposable checkout |
| `ZKSYNC_OS_GIT_URL` | Optional transport override or mirror for materializing the official `matter-labs/zksync-os` commit. The exact `Cargo.lock`-pinned revision must be present; execution dependencies are rewritten only after their official source and locked object ID are verified |
| `GATEWAY_CREATE2_FACTORY_SALT` | Required explicit deterministic L1 deployment salt on Tanenbaum/Mainnet; fingerprint-bound and written over zkstack's generated placeholder before broadcast |
| `GATEWAY_WALLET_PATH` | Wallet file used for gateway ecosystem create (`in-file` if present, else random+persist) |
| `EDGE_WALLET_PATH` | Wallet file used for edge chain create (`in-file` if present, else random+persist) |
| `DEPLOYER_SIGNER` | Optional L1 deployer signer override for direct Forge deployment/retry broadcasts; defaults to `FUNDER_SIGNER` on Tanenbaum/Mainnet and supports `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `DEPLOYER_ACCOUNT_NAME` | Foundry keystore account name when `DEPLOYER_SIGNER=account` (default: `FUNDER_ACCOUNT_NAME`, then `funder`) |
| `DEPLOYER_KEYSTORE` | Keystore file path when `DEPLOYER_SIGNER=keystore` (default: `FUNDER_KEYSTORE`) |
| `DEPLOYER_PASSWORD_FILE` | Optional keystore password file passed to Forge without exposing the password in argv (default: `FUNDER_PASSWORD_FILE`) |
| `EDGE_GATEWAY_GOVERNOR_SIGNER` | Optional governor signer override for Gateway migration repairs; defaults to the generated Gateway governor and supports `generated`, `account`, `keystore`, `ledger`, `trezor`, `aws`, `gcp`, or local-only `private-key` |
| `EDGE_GATEWAY_GOVERNOR_ACCOUNT_NAME` | Foundry keystore account name when `EDGE_GATEWAY_GOVERNOR_SIGNER=account` (default: `FUNDER_ACCOUNT_NAME`, then `funder`) |
| `EDGE_GATEWAY_GOVERNOR_KEYSTORE` | Keystore file path when `EDGE_GATEWAY_GOVERNOR_SIGNER=keystore` (default: `FUNDER_KEYSTORE`) |
| `EDGE_GATEWAY_GOVERNOR_PASSWORD_FILE` | Optional password file used for the short-lived generated-governor or external keystore (default: `FUNDER_PASSWORD_FILE`; when neither is set for the generated governor, the launcher creates a random process-local password) |
| `BITCOIN_DA_RPC_URL` / `BITCOIN_DA_RPC_USER` / `BITCOIN_DA_RPC_PASSWORD` | DA connectivity for gateway blobs mode |

## Notes

- Default `FOUNDRY_EVM_VERSION` is `cancun`.
- The reviewed L2 genesis is reproduced with the pinned Era Contracts `prague`
  target in an isolated temporary artifact tree. Real Syscoin L1 deployment
  artifacts use `FOUNDRY_EVM_VERSION` (default `cancun`); the fixed `prague`
  build is temporary and only reproduces and attests the reviewed L2 genesis.
<!-- SYSCOIN: bind operator prover mode to the explicit on-chain verifier mode. -->
- `PROVER_MODE=no-proofs` is valid only when the active settlement-layer diamond points to the explicit `ZKsyncOSTestnetVerifier`. Its constructor is bound to both the execution chain and the ultimate root-L1 chain ID carried through direct and Gateway deployment: root chain ID 1 or 57 is rejected even when the constructor executes on Gateway chain ID 57001, while Tanenbaum root 5700 remains available for a fresh mock-prover testnet. The node also reads the wrapper's testnet marker before starting fake pools. Conversely, `PROVER_MODE=gpu` requires the production wrapper, a cleared regeneration sentinel, and an exact match between its on-chain PLONK VK hash and the compiled V8 VK. Converting the testnet to real proving therefore requires an atomic verifier/VK/config transition, not merely changing this environment variable.
- `run-gateway-launch.sh` still enforces L1 chain-id preflight before broadcast steps.
- Generated zkstack deployer/governor keys are passed to Forge through a private,
  short-lived encrypted keystore and password file; they are never rendered in
  Forge argv or launcher logs.
- Migration safety guards remain in `edge-chain-migrate-to-gateway.sh` (DA bytecode checks, idempotent pause/unpause behavior).
- For Tanenbaum/Mainnet launches, keep `L1_RPC_URL` on local Syscoin RPC and set `GATEWAY_ARCHIVE_L1_RPC_URL` to the archive/public endpoint.
- On Tanenbaum and mainnet, `gateway-deploy-l1.sh` derives `ZKSYS_ZK_TOKEN_ASSET_ID` from the zkSYS edge-chain ID, deterministic L2 zkSYS proxy address, and the v32 L2 native token vault address `0x0000000000000000000000000000000000010004`, then exports it for zkstack CTM deployment. Operator-supplied overrides are rejected. v32 uses this asset id only for InteropCenter's optional fixed zkSYS fee path; the default interop fee path remains base-token `msg.value` in SYS.
- Canonical zkSYS CREATE2 bytecode derivation uses `forge inspect --no-metadata` so Solidity metadata and local remapping paths do not affect deterministic addresses. Discard any zkSYS CREATE2 addresses or `ZKSYS_ZK_TOKEN_ASSET_ID` values calculated from metadata-bearing bytecode.
- Canonical Pali ERC-4337 infrastructure bytecodes are also generated without Solidity metadata; keep `pali-wallet` deployment constants and the committed Blockscout/Sourcify standard JSON inputs in sync when regenerating them. The SLH-DSA validator constructor pins the verifier runtime code hash.
- The prover API is plain HTTP and loopback-only in the node process. Internet-reachable provers must use the generated buffering HTTPS vhost, which forwards Basic Auth while draining each complete bounded response independently of client pace.
- `GATEWAY_CREATE2_FACTORY_SALT` is fingerprint-bound. Changing it requires a fresh deployment directory or an explicit operator reset of both launcher state and the corresponding deployment artifacts; clearing checkpoint JSON alone is unsafe.
- After the chain is live, run `scripts/gateway-launch/zksys-l2-bootstrap.sh` to deploy the canonical L2 zkSYS `ProxyAdmin`, transparent proxy, implementation, membership fact registry, reward weight registry, algorithmic issuer, and zkSYS gas tank with deterministic CREATE2 salts, then wire issuer minting, membership-to-weight callbacks, weight-to-issuer callbacks, optional L1 registry bridge authority, and burn rights for the gas tank (`burnSurplus()`). The script verifies the final role and receiver wiring before exiting and records the gas tank address as `l2.zksys_gas_tank_addr`. That persisted, attested value is also the durable launch-policy transition: the next canonical edge-chain main-node start requires the exact gas-tank runtime to exist in the latest local state, and no operator-supplied `SYSCOIN_REQUIRE_GAS_TANK=0` can keep the first-boot exception active. That address must equal the immutable address already bound to the canonical application and VK; changing it requires rebuilding the app and verifier artifacts. The token admin receives role-admin authority for recovery and later governance transfer, but not direct `MINTER_ROLE` / `BURNER_ROLE`.
- The membership registry mirrors NEVM facts from the L1 `0x62` precompile and exposes the active Sentry Node address set for offchain diffing. The L1 registry bridge derives each Sentry Node's seniority-weighted reward weight from raw Syscoin collateral age (`nNEVMStartBlock + block.number - collateralHeight`) and sends that final weight to L2. For mainnet, use effective post-NEVM seniority thresholds `210240` and `525600` blocks with levels `3500` and `10000` bps. Native SYS staking is handled by the L2 staking vault.
- Reward weight increases are not active immediately: native SYS deposits, Sentry Node additions, and Sentry Node seniority increases are queued for `ZKSYS_WEIGHT_ACTIVATION_DELAY_PERIODS` periods and require the account to call `activatePendingWeight()` after the delay. Weight decreases and removals apply immediately. This prevents a stake or Sentry weight increase submitted just before a period boundary from earning the completed period.
- The issuer uses a fixed remaining-cap curve: 20% in schedule year 1, 12% in year 2, 8% in year 3, then 5% per year afterward. Each annual amount is released pro-rata over `ZKSYS_ISSUER_PERIODS_PER_YEAR` periods, so scheduled issuance approaches but never exceeds the 210M zkSYS cap.
<!-- SYSCOIN: Fake/real mode is part of durable prover recovery authority, not launcher cache state. -->
- Do not switch prover mode (`PROVER_MODE` / effective `GATEWAY_PROVER_MODE`) while retaining a chain's proof storage or journal. Clearing `$GATEWAY_DIR/.gateway-launch` is not sufficient; use an explicitly planned verifier/config/chain-state transition instead of treating a production restart as a fake/real mode change.
- The launcher and repair command never delete runtime databases. If an explicitly
  diagnosed incompatible replay requires a reset, first stop the node, then back
  up and move the complete `os-server-configs/<chain>/db` directory before rerunning.
- For `v32.x`, launcher build/run commands copy the current `zksync-os-server` tree into `$GATEWAY_DIR/.gateway-launch/zksync-os-server/`, rewrite only the `*_dev` `zksync-os` deps to the patched upstream checkout, and use that isolated workspace for Cargo.
- The zkSYS external-node deployment invalidates the prior build stamp, builds each isolated patched workspace with `build-node.sh`, then publishes a stamp bound to the binary, compiled Syscoin address context, and current server build/release sources before restarting its service. The generated `start-node.sh` only refreshes rotating Syscoin RPC cookie credentials, validates that stamp, and executes the prebuilt binary, so ordinary systemd or package-maintenance restarts do not rebuild Rust dependencies while the public RPC is offline. A failed or partial redeployment leaves the stamp invalid and later restarts fail closed rather than running a stale binary against new deployment state.
- High-TPS runs can exhaust low default `nofile` limits; use at least `65536`, with `131072+` recommended.
