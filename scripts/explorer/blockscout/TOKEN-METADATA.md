# zkSYS explorer token metadata

Network branding and wallet/bridge token lists do not populate Blockscout's ERC-20
`icon_url`. `token-metadata.zksys.json` records the two reviewed chain-57057 token
icons; `publish-token-metadata.py` reapplies them through Blockscout's supported
admin API after initial indexing or a deliberate database rebuild.

The helper is pinned to the deployed **Blockscout v9.0.2** source revision
`ed93c12dc1835ec0e7fb3e4be3504ceb7061610e`. Review its compatibility again before
changing the backend version or chain addresses:

- [Import controller and authentication](https://github.com/blockscout/blockscout/blob/ed93c12dc1835ec0e7fb3e4be3504ceb7061610e/apps/block_scout_web/lib/block_scout_web/controllers/api/v2/import_controller.ex)
- [API routes](https://github.com/blockscout/blockscout/blob/ed93c12dc1835ec0e7fb3e4be3504ceb7061610e/apps/block_scout_web/lib/block_scout_web/routers/api_router.ex)

## Operator procedure

1. Deploy/index the correct chain first. Confirm the actual backend image/revision
   and review both token contract addresses. The helper checks the reported
   backend version, chain ID, token address, symbol and decimals; it does not
   replace operator verification of deployed bytecode/source provenance.
2. From the repository root, run the default read-only check:

   ```sh
   timeout 45s python3 scripts/explorer/blockscout/publish-token-metadata.py
   ```

3. `deploy-remote.sh` copies the helper and its adjacent manifest into
   `REMOTE_DIR` (default `/home/ubuntu/gateway/ui/blockscout`); it does not run the
   helper or publish metadata. On the zkSYS explorer host, change to that deployed
   directory and pipe the **existing** `API_SENSITIVE_ENDPOINTS_KEY` from
   an approved private secret reader into the helper with
   `--base-url http://127.0.0.1:5200 --apply`. The public HTTPS base is also allowed.
   Do not place the key in command-line arguments, shell history, logs or a new
   environment file. Do not enable shell tracing. An absent existing key requires
   a separately reviewed configuration change, not an automatic restart.

   ```text
   cd /home/ubuntu/gateway/ui/blockscout  # use the actual REMOTE_DIR if overridden
   <private secret reader> | timeout 45s python3 ./publish-token-metadata.py --base-url http://127.0.0.1:5200 --apply
   ```

   The helper resolves the manifest relative to its own file, not the working
   directory. It remains specific to the zkSYS chain even though both deployment
   instances receive the files.

4. Retain the helper's public-only result and rerun the read-only check (omit
   `--apply`, retaining the host-local `--base-url` if used). Both tokens
   must report `already-current`; then confirm the browser renders each icon.
   An API success alone is not visual acceptance. If a POST fails or times out,
   inspect the read-only result before authorizing another run. There is no
   automatic retry.

   These commands use the Linux host's `timeout` for a hard wall-clock bound;
   retain it even though individual socket operations and body reads are bounded.
   A timeout during import is an ambiguous result, not permission to resend.

The exact API is `POST /api/v2/import/token-info`. Contrary to a stale header
comment in that upstream controller, this version reads the secret from JSON
`api_key`. The other fields are `token_address`, `icon_url`, `token_symbol: null`
and `token_name: null`. Those null values preserve the existing name/symbol.
The supported endpoint also sets `is_verified_via_admin_panel: true`; this is
an **administrative metadata flag**, not bytecode/source verification. No token
contract, balance, protocol state or transaction is changed.

The helper preflights both tokens before any import, skips already-correct icons,
does at most one POST per remaining token, and verifies each resulting icon and
unchanged name/symbol/decimals. It permits only the public zkSYS explorer or its
host-local port 5200, refuses redirects/proxy environment settings, caps responses
at 256 KiB each/2 MiB total and makes at most eight requests. Socket operations
time out at ten seconds with an 80-second overall read-loop budget. It never
contacts a database, replaces secret files, rebuilds containers or restarts
shared services.
