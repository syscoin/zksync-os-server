#!/usr/bin/env bash
# Start local Anvil for gateway-launch (chain id 9).
# --mixed-mining only (no --block-time): with --block-time 1, forge DeployL1CoreContracts --broadcast
# reproducibly stalls (run-latest: many txs, 0 receipts; txpool pending=queued=0). Mixed-mining
# still mitigates foundry#10122-style issues; run-gateway-launch watch also mines every tick.
# See docs/src/guides/gateway_launch.md
set -euo pipefail
exec anvil --chain-id 9 --host 0.0.0.0 --mixed-mining "$@"
