#!/usr/bin/env bash
# Start local Anvil for gateway-launch (chain id 9).
# Newer Anvil builds reject --mixed-mining without --block-time, while --block-time 1 caused
# forge DeployL1CoreContracts --broadcast to stall in practice. Use a long block time so the
# gateway-launch watch remains the primary miner via explicit `anvil_mine`, while still satisfying
# the newer CLI requirement.
# See docs/src/guides/gateway_launch.md
set -euo pipefail
: "${GATEWAY_ANVIL_BLOCK_TIME:=3600}"
exec anvil --chain-id 9 --host 0.0.0.0 --block-time "${GATEWAY_ANVIL_BLOCK_TIME}" --mixed-mining "$@"
