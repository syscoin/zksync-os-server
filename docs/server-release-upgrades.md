# Server release inventory and in-place upgrades

A successful Gateway/Edge sequencer restart does not upgrade public or debug
external nodes (ENs). Release completion must name the full deployed fleet:
main nodes, public ENs and internal/debug ENs. Review this inventory before an
upgrade; do not derive the expected fleet from whichever snapshots were returned.
An intentionally absent role needs an explicit `not_deployed` reason.

`scripts/gateway-launch/check-server-release-inventory.py` is a read-only check.
It does not connect by SSH, make RPC calls, install a binary, restart services,
change configuration, read wallet files, or print config/environment contents.
It compares running ELF hashes with on-disk binaries, the stock source closure
and workspace-specific prebuilt stamps. A pass is **not** chain health, proof
qualification, finality, contract acceptance or successful user transactions.

## Freeze a release and the complete fleet

Use a clean merged release checkout. The expected source digest is the stock
`prebuilt_source_digest` from `run-os-server-with-patched-zksync-os.sh`, not a
commit label alone. Keep the reviewed commit/tree, build receipt and ELF hashes
with the manifest. Binaries may differ by build workspace, so pin each instance's
actual accepted ELF; do not assume all role hashes are identical.

The manifest schema is `syscoin-server-release-inventory-v1`. It has a
`source_sha256`, `role_classes` for all three classes (`main_node`,
`public_external_node`, `debug_external_node`), and `instances` with `host`,
`unit`, `role_class`, `binary_sha256` and ordered `config_sha256` hashes for its
explicit config files. Each role class is either
`{"status":"deployed"}` or `{"status":"not_deployed","reason":"..."}`.
Every deployed class needs at least one instance. Host labels and units are
operator-chosen; the tool contains no production/testnet host or chain defaults.

On each host, under its existing deployment user, capture all declared units:

```sh
python3 scripts/gateway-launch/check-server-release-inventory.py capture \
  --host rpc-host --unit public-en.service --unit debug-en.service
```

Save that non-secret JSON through the operator's normal evidence workflow, then
verify the complete fleet (including a main-node-host snapshot):

```sh
python3 scripts/gateway-launch/check-server-release-inventory.py verify \
  --manifest reviewed-release-inventory.json \
  --snapshot main-node-host.json --snapshot rpc-host.json
```

Snapshots expire after five minutes by default (bounded to one hour if explicitly
configured). Missing, undeclared, duplicate, inactive, stale-source,
mismatched-running-ELF, changed-config and invalid-stock-stamp instances fail
closed. The capture assumes Linux `/proc`, systemd and an explicit `--config`.
The process environment must export `ZKSYNC_OS_SERVER_PATH`, `GATEWAY_DIR`,
`PROTOCOL_VERSION`, `SYSCOIN_EDGE_DA_COMMIT_TARGET` and `SYSCOIN_GAS_TANK_ADDRESS`.
These are the stock source/workspace/protocol and compiled-consensus bindings,
not operator credentials. The generated main-node and EN launchers export their
source/workspace/protocol bindings. The shared runner also exports its validated
canonical source root and resolved protocol, including its stock default, before
executing the server. Custom launchers must preserve these actual startup
bindings; shell-only variables are not visible in `/proc`. Qualify them through
the normal launcher process before capture. Do not invent missing values or
relabel a running process to make the inventory pass.
Capture failure is not an acceptable substitute for a role's snapshot. Read-only
filesystem/process changes detected during capture also reject the observation.
Config hashes describe observed files, not a guarantee they were the bytes loaded
at process startup; check effective configuration and namespace/bind policy
separately.

## Update existing ENs without rebuilding the chain

1. Record each EN's current config hash, database directory identity, network-key
   inode/permissions, stock binary/stamp, launcher/unit and a canonical old block.
   Preserve private rollback copies through the existing operator workflow.
   Freeze the intended clean source and independently qualified replacement ELF.
2. Preserve the network, database paths, role, existing network keys, config
   values, public/debug bindings, private Gateway access policy and shared service
   units. An EN update is not a new network or database initialization. Do not
   restart sequencers, bridge actors or shared monitors as an EN-update shortcut.
3. Use the existing deployment/build and `exec-prebuilt` mechanisms. The
   deployer has `START_PUBLIC_SERVICE` and `START_DEBUG_SERVICE` controls; qualify
   and restart one role, verify it, then restart the other. The deployment script
   generates both configs and builds both role targets: these flags do **not**
   promise byte-preserving config adoption or isolate shared-source changes. For
   an existing customized configuration, review a separately bounded in-place
   adoption before running the installer. Avoid changing a shared source tree
   while the other role still depends on its old stamp for restarts.
4. A separately reviewed prebuilt import may reuse an already qualified ELF only
   after matching the full clean source/build inputs, compiled consensus inputs,
   architecture/runtime dependencies and preserved runtime configuration. Retain
   the accepted build receipt. The stock stamp includes the **destination
   workspace name**, protocol, compiled inputs and source digest; never copy a
   sequencer's stamp to an EN or relabel a stale binary as current.
5. After each normal start, verify the running ELF/PID, source/stamp, expected
   chain ID, retained canonical old block/genesis, catch-up/head consistency with
   the intended sequencer, public/debug namespaces and continued private Gateway
   policy. HTTP 200 or an active PID is insufficient. Ordinary restart invalidates
   in-memory RPC filter IDs; clients must recreate filters, not replay financial
   transactions. Durable replay/database cursors and shared monitor state remain
   untouched.
6. Capture and verify the entire release inventory again. Keep raw observations,
   semantic checks and the operator's final acceptance separate. Do not declare
   the release complete while a public/debug EN remains on a previous source.

This process applies to future mainnet releases too. It neither changes proving
mode nor treats a mock/no-proofs lane as production proof qualification.
