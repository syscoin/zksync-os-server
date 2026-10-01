#!/usr/bin/env bash
# Intentionally accepts no arbitrary Forge flags, endpoint, signer, or broadcast option.
set -euo pipefail
if [ "$#" -ne 2 ]; then
  printf '%s\n' 'usage: bash run_offline.sh INPUT_BASENAME.json OUTPUT_BASENAME.json' >&2
  exit 2
fi
input_name="$1"
output_name="$2"
[[ "$input_name" =~ ^[A-Za-z0-9_-]+\.json$ ]] || exit 2
[[ "$output_name" =~ ^[A-Za-z0-9_-]+\.json$ ]] || exit 2
identity_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
task_dir="$(cd -- "$identity_dir/.." && pwd -P)"
[ -f "$identity_dir/$input_name" ]
[ ! -e "$identity_dir/results/$output_name" ] || {
  printf '%s\n' 'refusing to overwrite an existing evidence result' >&2
  exit 1
}
mkdir -p "$identity_dir/results" "$task_dir/era-contracts/l1-contracts/out"
exec docker run --rm --platform linux/amd64 --network none \
  --mount "type=bind,source=$task_dir,target=/work" \
  --mount "type=bind,source=$task_dir/era-contracts,target=/work/era-contracts,readonly" \
  --mount "type=bind,source=$identity_dir/compiler-cache,target=/root/.svm" \
  --mount "type=bind,source=$identity_dir/foundry.toml,target=/work/era-contracts/l1-contracts/foundry.toml,readonly" \
  --mount "type=bind,source=$identity_dir/cancun-out,target=/work/era-contracts/l1-contracts/out,readonly" \
  --env GW_IS_EVM_EQUIVALENT=true \
  --env SYSCOIN_EDGE_DA_RELAY_ARTIFACT=/work/identity/prague-out/SyscoinRelayedSLDAValidator.sol/SyscoinRelayedSLDAValidator.json \
  --workdir /work/era-contracts/l1-contracts \
  airbender-build:nightly-2026-02-10 /work/foundry-zksync-v0.1.5/forge script \
  /work/identity/DeriveGuestBoundIdentity.s.sol:DeriveGuestBoundIdentity \
  --root /work/era-contracts/l1-contracts \
  --sig 'run(string,string)' \
  "/work/identity/$input_name" "/work/identity/results/$output_name" --offline
