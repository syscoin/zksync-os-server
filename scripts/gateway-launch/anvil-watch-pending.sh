#!/usr/bin/env bash
# While large forge/zkstack deploys run, mine if the tx pool has pending OR queued txs.
# (foundry#10122: stuck with pending=0, queued>0 under pure automine; same fields matter here.)
# Requires: L1_RPC_URL, cast on PATH.
set -euo pipefail
: "${L1_RPC_URL:?set L1_RPC_URL}"
while pgrep -f 'DeployL1CoreContracts\.s\.sol|DeployCTM\.s\.sol|zkstack ecosystem init|zkstack chain init' >/dev/null 2>&1; do
  _tp="$(cast rpc txpool_status --rpc-url "${L1_RPC_URL}" 2>/dev/null || echo '{}')"
  read -r P Q <<EOF
$(python3 -c "import json,sys; j=json.loads(sys.argv[1]); print(int(j.get('pending','0x0'),16), int(j.get('queued','0x0'),16))" "${_tp}")
EOF
  if [ "${P}" != "0" ] || [ "${Q}" != "0" ]; then
    echo "[watch] pending=${P} queued=${Q}"
  fi
  cast rpc anvil_mine 1 --rpc-url "${L1_RPC_URL}"
  sleep 1
done
