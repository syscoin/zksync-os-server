#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /absolute/path/to/era-contracts" >&2
  exit 1
fi

case "$1" in
  /*) ;;
  *)
    echo "error: Era-contracts path must be absolute: $1" >&2
    exit 1
    ;;
esac

if [[ ! -d "$1" ]]; then
  echo "error: Era-contracts directory does not exist: $1" >&2
  exit 1
fi

CONTRACTS_PATH="$(cd "$1" && pwd -P)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PATCH_FILE="${ERA_CONTRACTS_SYSCOIN_PATCH_FILE:-${SCRIPT_DIR}/patches/era-contracts-syscoin.patch}"

EXPECTED_BASE_COMMIT="8fb7c29a4e3174335c6480b23f57822e054f9d5f"
EXPECTED_BASE_TREE="acdd11e5bb7787d9df2306f6a1dc96bf92e67f53"
EXPECTED_NESTED_SHA="e554ae64ec150c47d6f17786e7f4aacebc7bf945"
NESTED_PATH="lib/@matterlabs/zksync-contracts"

EXPECTED_PATCH_SIZE="658434"
EXPECTED_PATCH_SHA256="1814e1ba5c0605df6e1338670d7c39d4d60e94503a2e836ed280cbd7207f4bcd"
EXPECTED_PATCH_PATH_COUNT="65"
EXPECTED_PATCH_PATHS_SHA256="8649c1aea0b303e6284d9ab26aff4641260aff9f6ce6ce3e2f5556331af3b3b0"

STOCK_APP_VK_HASH="0x9f7576b911e7d3f528d49f894208682c81800814db9e3beac7fc3b1c4d626e7a"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

die() {
  echo "error: $*" >&2
  exit 1
}

[[ -f "${PATCH_FILE}" && ! -L "${PATCH_FILE}" ]] ||
  die "canonical patch is missing, not regular, or a symlink: ${PATCH_FILE}"

git -C "${CONTRACTS_PATH}" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
  die "${CONTRACTS_PATH} is not a git worktree"
ACTUAL_TOPLEVEL="$(git -C "${CONTRACTS_PATH}" rev-parse --show-toplevel)"
ACTUAL_TOPLEVEL="$(cd "${ACTUAL_TOPLEVEL}" && pwd -P)"
[[ "${ACTUAL_TOPLEVEL}" == "${CONTRACTS_PATH}" ]] ||
  die "argument must be the Era-contracts repository root: expected=${ACTUAL_TOPLEVEL} actual=${CONTRACTS_PATH}"

ACTUAL_HEAD="$(git -C "${CONTRACTS_PATH}" rev-parse HEAD)"
ACTUAL_TREE="$(git -C "${CONTRACTS_PATH}" rev-parse HEAD^{tree})"
[[ "${ACTUAL_HEAD}" == "${EXPECTED_BASE_COMMIT}" ]] ||
  die "wrong Era-contracts commit: expected=${EXPECTED_BASE_COMMIT} actual=${ACTUAL_HEAD}"
[[ "${ACTUAL_TREE}" == "${EXPECTED_BASE_TREE}" ]] ||
  die "wrong Era-contracts tree: expected=${EXPECTED_BASE_TREE} actual=${ACTUAL_TREE}"

ACTUAL_PATCH_SIZE="$(wc -c < "${PATCH_FILE}" | tr -d '[:space:]')"
ACTUAL_PATCH_SHA256="$(sha256_file "${PATCH_FILE}")"
[[ "${ACTUAL_PATCH_SIZE}" == "${EXPECTED_PATCH_SIZE}" ]] ||
  die "canonical patch size mismatch: expected=${EXPECTED_PATCH_SIZE} actual=${ACTUAL_PATCH_SIZE}"
[[ "${ACTUAL_PATCH_SHA256}" == "${EXPECTED_PATCH_SHA256}" ]] ||
  die "canonical patch SHA-256 mismatch: expected=${EXPECTED_PATCH_SHA256} actual=${ACTUAL_PATCH_SHA256}"

# The canonical patch is itself tracked by the server repository. Keep its text envelope
# whitespace-clean; the one whitespace-sensitive upstream Rust delta is encoded losslessly.
if LC_ALL=C grep -n '[[:blank:]]$' "${PATCH_FILE}" >/dev/null; then
  die "canonical patch contains trailing whitespace"
fi
[[ "$(grep -c '^GIT binary patch$' "${PATCH_FILE}")" == "1" ]] ||
  die "canonical patch must contain exactly one binary delta for the whitespace-sensitive verifier generator"

PATCH_PATHS="$(
  git -C "${CONTRACTS_PATH}" apply --numstat --recount "${PATCH_FILE}" |
    awk -F '\t' '{print $3}' |
    LC_ALL=C sort
)"
PATCH_PATH_COUNT="$(printf '%s\n' "${PATCH_PATHS}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
PATCH_PATHS_SHA256="$(printf '%s\n' "${PATCH_PATHS}" | sha256_stdin)"
[[ "${PATCH_PATH_COUNT}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] ||
  die "canonical patch path count mismatch: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${PATCH_PATH_COUNT}"
[[ "${PATCH_PATHS_SHA256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] ||
  die "canonical patch path manifest mismatch"

# The app-bound PLONK contract and key JSON remain outside this source patch and must be
# generated and attested separately. The patch deliberately deletes the verification-dead
# zkOS FFLONK source/key; generic Era FFLONK artifacts remain untouched.
for forbidden_path in \
  "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol" \
  "tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json"
do
  if printf '%s\n' "${PATCH_PATHS}" | grep -Fqx "${forbidden_path}"; then
    die "canonical source patch unexpectedly changes verifier artifact: ${forbidden_path}"
  fi
done

for required_deleted_path in \
  "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol" \
  "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json"
do
  if ! printf '%s\n' "${PATCH_PATHS}" | grep -Fqx "${required_deleted_path}"; then
    die "canonical source patch does not delete disabled zkOS FFLONK artifact: ${required_deleted_path}"
  fi
done

patch_forward_applicable() {
  git -C "${CONTRACTS_PATH}" apply --check --recount --whitespace=error-all "${PATCH_FILE}" >/dev/null 2>&1
}

patch_reverse_applicable() {
  # The upstream Rust whitespace deletion is a binary delta, so both directions can enforce
  # strict text whitespace without losing the exact base or postimage bytes.
  git -C "${CONTRACTS_PATH}" apply --reverse --check --recount --whitespace=error-all "${PATCH_FILE}" >/dev/null 2>&1
}

verify_exact_file() {
  local relative_path="$1" expected_size="$2" expected_sha256="$3"
  local path="${CONTRACTS_PATH}/${relative_path}" actual_size actual_sha256
  [[ -f "${path}" && ! -L "${path}" ]] ||
    die "postimage is missing, not regular, or a symlink: ${relative_path}"
  actual_size="$(wc -c < "${path}" | tr -d '[:space:]')"
  actual_sha256="$(sha256_file "${path}")"
  [[ "${actual_size}" == "${expected_size}" ]] ||
    die "postimage size mismatch for ${relative_path}: expected=${expected_size} actual=${actual_size}"
  [[ "${actual_sha256}" == "${expected_sha256}" ]] ||
    die "postimage SHA-256 mismatch for ${relative_path}: expected=${expected_sha256} actual=${actual_sha256}"
}

verify_postimage_manifest() {
  while read -r expected_size expected_sha256 relative_path; do
    [[ -n "${relative_path}" ]] || continue
    verify_exact_file "${relative_path}" "${expected_size}" "${expected_sha256}"
  done <<'SYSCOIN_POSTIMAGE_MANIFEST'
18173 679a977b41d3f78ec4901eca64fb70f22e5a9ecf458a15ed9b302f5bab013ad5 .github/workflows/l1-contracts-ci.yaml
2560 9b05619a1f4903fe24053955cbe652626b036127d3492ddb74efea4b27b2bd9e .github/workflows/slither.yaml
1433 bd10dcd322c0f23805c556d31cecbdfdb562adb1fb6deb10b14f41019d5b5a21 .prettierignore
159930 05a58477ab36d2b020c7bc94392888705fc261eec6cb1c38b9d2a905ede0d7c3 AllContractsHashes.json
1615 b9492bb3d1cbb976fbc2bd960707c194750202b9569f6c60e8bcdefa7353384e da-contracts/contracts/DAContractsErrors.sol
601 9201889972a107b91caec471ad95bb7c912fa1b2c0822004bb06f3629b1d2fd2 da-contracts/contracts/SyscoinDAUtils.sol
2811 1397e31377f382e311f9582deb25a7899cd9bf605c9bf1971093e0a814c20b45 da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol
5020 2edd28b26d393c601e85d6de5bbe23096fd60332338e66ca36e999edca0b1697 da-contracts/test/foundry/SyscoinL1DAValidatorZKsyncOS.t.sol
2214 21f230d3d1fe830ce140d2c18275c66d96f66e335d08da021d55148ed0df0747 l1-contracts/contracts/bridge/BridgeHelper.sol
2741 f56a1acc456774feeedd4e49f00af92ec27258518aa9ebe7a86e8d91c7046028 l1-contracts/contracts/common/StateTransitionTypes.sol
833 63a9033b60dd77f0c166c4f6f2177693717163e526bded1e6724196b5ad7422a l1-contracts/contracts/common/SyscoinConfig.sol
3945 6c64f59cf560d21a8c7223d86475df69c28a5348edc523f16f891a55b572fbd5 l1-contracts/contracts/script-interfaces/IDeployL2Contracts.sol
2673 a1beabc87a05602ff4c5dc1be2feead39949385549e7d19dc7f51db25f68e236 l1-contracts/contracts/script-interfaces/IRegisterZKChain.sol
4350 46879b879bee93b99f2d1c549e64b304da215fe2da1281f54a658fb97d0ea98e l1-contracts/contracts/state-transition/L1StateTransitionErrors.sol
25886 14497f9b115ef308207a7a8f745694d3a10746d7692abfa0ac8a0fb41d25b155 l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol
47451 85294c11b0f49ce52a33487a836835ed156cc79a40e707656ffd69cfb26aedca l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol
2039 d016c5f44e58b3f7c1a9f528074d003df3a456cbc172ce32743bdb870cc7f8d4 l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol
2615 f889b9306db4bc5eff5fb0bcdfc00c2e912350a426542b670fec7a363ecdc751 l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol
8280 fc0dd7d98d372330d55ba9d4a8d397dac768be390f230f9d8ab2d6e23bb93c3a l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol
9982 6d78dd90d9ea85ce9c30211fa8dbfbac53fd84e40f1328c0b72c091929803f46 l1-contracts/contracts/state-transition/chain-interfaces/IExecutor.sol
541 6ca90d2112debc99221b048a6a1a665ef8ea83b6042157e7c7c179803d57730e l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol
4273 51f404a6ac45d3d3b45100cee2a1ce824e9d61cb199d03d65434c51f973a25cc l1-contracts/contracts/state-transition/data-availability/RollupDAManager.sol
2719 626e72c2c39e07e943f87fcb4523576b0b701f5c7988cc8227481d97315e9909 l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol
1872 32d75e5fe32c3dd3d85459c03b7dc1dfa667a350c1cc3ebbc862ecc3cfe37582 l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol
8893 4dcff298c0a4df26751568bc6d78ba43931825a67f16783833c15906e9e54136 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol
2038 539e405865803e70ad68c2081299f681648b8319460a49c7400c2ac03d4e32e1 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol
6707 2428a3ae1112ab7014cc332f0f087027d474055790e80d2c6d8957d8ce13ec05 l1-contracts/contracts/upgrades/L1FixedForceDeploymentsHelper.sol
10570 610487a7e3503b2860cccf81d6c28b06f0bd04f57d5da06b23a3aad39d098546 l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol
30611 3563eb0faa4aef96ab249d541a37f65ba3b9d211153f8017ad68e044b6ab560e l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol
13653 5d738e2bebe6831edc731fe87a84d63032e37f21b6ebd70b1ecf6fe04e0ed1bb l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol
21065 7fe7b3d63220de44c069d39a086b82e13925abc99a67d59161ea3cfe20541744 l1-contracts/deploy-scripts/ctm/DeployCTMUtils.s.sol
26012 0d35573f3436f83523143dc5f36c8064706d63bb38a45c828c7bb5c86836844b l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol
38525 7452bdf0d2d5c9fda45bd5c08f70f21bf5945e4a8d69c4cbe0309a58a33e4f46 l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol
20390 fc11828bdb368afa549902e11de120e8bbb1e2c443462d10385813f297f5f553 l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol
45246 30bdc5d87a4eeff24d06d69be56221b05753c9317f8577b2534513630c1c88e4 l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultCTMUpgrade.s.sol
31141 cd28c4f95efef2e6d82cd525e58467c0ea5f58cab83f000b20c6e01d4c2626ee l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultGatewayUpgrade.s.sol
23010 cb187e7f389dfbb0901d14c2e9b25ec887d279c139e83c3293ff08ed5de3edfc l1-contracts/deploy-scripts/utils/AddressIntrospector.sol
11788 ea6f79c67102e48874d3d2e1015c682452cd8ceba0ad8ebea532c46ebe9d624e l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol
5127 ee6d66efe63dc4b97540362d8ad9a693f4fb3c6ba7d7725e7a446c32d03c20c2 l1-contracts/foundry.toml
2305049 7094cba7745c399407df2a9287a35ec3f5ac94767a5c79c0b2a9af5bb6698d69 l1-contracts/selectors
16687 7bdf8ffdaa910ce9c148474897110db70ca559c0606f23ec3528cde5e8cc1a64 l1-contracts/test/foundry/l1/integration/GatewayVotePreparationTests.t.sol
17852 8430598e45f49d8d33f7cd4ec200eda8109a461d0df64306615476d7e81163a1 l1-contracts/test/foundry/l1/integration/UpgradeTestShared.t.sol
8296 b18605c0bc27bdb37e37ed58bc5c3484b92dfb2b2e2f56e3a9d5b0804e9fe752 l1-contracts/test/foundry/l1/integration/_SharedL1ContractDeployer.t.sol
10489 9af1774b88371c8b4e7469d54e4cf2d7ce84fccb8745de37a302a197729cce11 l1-contracts/test/foundry/l1/integration/_SharedZKChainDeployer.t.sol
9299 cd0ede252e156ad72d759c36a80a4832ecc5d139db708103fef0e4ee958f40a8 l1-contracts/test/foundry/l1/integration/deploy-scripts/script-config/config-deploy-ctm.toml
14206 d31debecd5d6dd48515fb3f4ffdfa5620a31402b79d22dbf7c7d01750ebbc052 l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/CommittingZKsyncOS.t.sol
6410 4a980bff555e45892aeb109bf0bd22b789c11e53988e0d5c5ec496c8c42dcec6 l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/ZKsyncOSPublicInput.t.sol
22429 7d74824b07446ac3dcc644070c4e73de276e209ec7069a1b31bb1eb25ad9dcbf l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/_Executor_Shared.t.sol
18595 5f035db0bba8065d1f19203fc9258f35393fde26f17a43d57cf9b4adb3d877e4 l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol
4513 a384c8bf0476364c8ad73f916c4739689f7eaee02abe63ecb4a2c935cb7c1f18 l1-contracts/test/foundry/l1/unit/concrete/Utils/DeployCTML1OrGateway.t.sol
37169 4528a744bd384ffb77614bc59bb8f9d7b4f85d1b6b6b37547907098ee0155d3a l1-contracts/test/foundry/l1/unit/concrete/Utils/Utils.sol
7991 f95e44019436e4ddc543611a2b3ae890fc5c5d68ab4a64e56bfd79859e55644a l1-contracts/test/foundry/l1/unit/concrete/state-transition/chain-deps/facets/Admin/MakePermanentRollup.t.sol
4253 fa76c9948e6714a48165c6916d02ff4d6922dd9a2a0224e0e0b11e007d360742 l1-contracts/test/foundry/l1/unit/concrete/state-transition/chain-deps/facets/Admin/SetZKsyncOSChainConfig.t.sol
4041 3bf4db1e7f53fe628c3a42d4f24ecb33c813e22ec74aa6926ba3e785fbae88ff l1-contracts/test/foundry/l1/unit/concrete/state-transition/data-availability/SyscoinRelayedSLDAValidator.t.sol
2757 5fae24f2799106a7608b3d295f911cb3a21db39405f985441321478263489b58 l1-contracts/test/foundry/l1/unit/concrete/state-transition/data-availability/SyscoinRollupDAManager.t.sol
14654 021fc712d1513822a74292cfae17b121bf1faf16e265c1fb2192dd14eb928d3a l1-contracts/test/foundry/l1/unit/concrete/state-transition/verifiers/ZKsyncOSDualVerifier.t.sol
17221 abd546566d56ac84ba78764c69beb84c373e8467b048371f54cd3d6ede61f598 l1-contracts/test/foundry/l2/unit/GatewayCTMDeployer/GatewayCTMDeployer.t.sol
2142 87c5bb6506f9762d2a5526df1a8d588f9ebf1a9726949fd1a482119a096b280b l1-contracts/test/foundry/l2/unit/GatewayCTMDeployer/SyscoinGatewayCTMDeployerDA.t.sol
69053 91f7111b6388441773b1dbaec13e98d4631b54f972cb806a537a4af6c0b473e7 protocol-ops/src/upgrade_verification/versions/v31/elements/deployed_addresses.rs
10751 608343dc8b3439bd91c2a54db26294161c1386b743e53c12ceeee44c867f4b94 system-contracts/contracts/Constants.sol
2110 5e4f3154e8d5541fa955deb8108ace7f454ebccd0180429e1115fba81f143b4f tools/verifier-gen/README.md
7504 a3139ed4dc14bf66978047bb4882e34952f43a9f7e8182c48ab01b775f5bd3e6 tools/verifier-gen/src/main.rs
SYSCOIN_POSTIMAGE_MANIFEST
}

require_text() {
  local relative_path="$1" expected="$2"
  grep -Fq -- "${expected}" "${CONTRACTS_PATH}/${relative_path}" ||
    die "missing canonical Syscoin V8 sentinel in ${relative_path}: ${expected}"
}

forbid_text() {
  local relative_path="$1" forbidden="$2"
  if grep -Fq -- "${forbidden}" "${CONTRACTS_PATH}/${relative_path}"; then
    die "forbidden legacy/provisional sentinel in ${relative_path}: ${forbidden}"
  fi
}

verify_absent_path() {
  local relative_path="$1"
  if [[ -e "${CONTRACTS_PATH}/${relative_path}" || -L "${CONTRACTS_PATH}/${relative_path}" ]]; then
    die "disabled legacy path unexpectedly exists: ${relative_path}"
  fi
}

verify_semantics() {
  # Intentional downstream source/config deviations remain visibly attributable after rebases.
  # The slither and prettier entries are deletion-only FFLONK cleanup; explicit absence checks
  # below attest those two non-taggable exceptions without inventing replacement configuration.
  local tagged_path
  for tagged_path in \
    ".github/workflows/l1-contracts-ci.yaml" \
    "da-contracts/contracts/DAContractsErrors.sol" \
    "da-contracts/contracts/SyscoinDAUtils.sol" \
    "da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol" \
    "l1-contracts/contracts/bridge/BridgeHelper.sol" \
    "l1-contracts/contracts/common/StateTransitionTypes.sol" \
    "l1-contracts/contracts/common/SyscoinConfig.sol" \
    "l1-contracts/contracts/script-interfaces/IDeployL2Contracts.sol" \
    "l1-contracts/contracts/script-interfaces/IRegisterZKChain.sol" \
    "l1-contracts/contracts/state-transition/L1StateTransitionErrors.sol" \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol" \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol" \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol" \
    "l1-contracts/contracts/state-transition/chain-interfaces/IExecutor.sol" \
    "l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol" \
    "l1-contracts/contracts/state-transition/data-availability/RollupDAManager.sol" \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol" \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "l1-contracts/contracts/upgrades/L1FixedForceDeploymentsHelper.sol" \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "l1-contracts/deploy-scripts/ctm/DeployCTMUtils.s.sol" \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol" \
    "l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultCTMUpgrade.s.sol" \
    "l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultGatewayUpgrade.s.sol" \
    "l1-contracts/deploy-scripts/utils/AddressIntrospector.sol" \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    "l1-contracts/foundry.toml" \
    "protocol-ops/src/upgrade_verification/versions/v31/elements/deployed_addresses.rs" \
    "system-contracts/contracts/Constants.sol" \
    "tools/verifier-gen/README.md" \
    "tools/verifier-gen/src/main.rs"
  do
    require_text "${tagged_path}" "SYSCOIN:"
  done

  # The most security-sensitive application-bound constants and ABI restrictions are tagged in place.
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/IExecutor.sol" \
    "SYSCOIN: Compact Bitcoin DA permits up to thirty-two 2 MiB references per batch."
  require_text \
    "system-contracts/contracts/Constants.sol" \
    "SYSCOIN: A compact Bitcoin-DA reference represents one 2 MiB availability object."
  require_text \
    "system-contracts/contracts/Constants.sol" \
    "SYSCOIN: Match the bounded thirty-two-reference compact DA envelope."
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol" \
    "SYSCOIN: Carry the opening and root separately so final settlement can revalidate Gateway relay data."
  require_text \
    "da-contracts/contracts/DAContractsErrors.sol" \
    "SYSCOIN: Distinguish a failed raw Bitcoin-DA precompile call from an unavailable reference."
  require_text \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    "SYSCOIN: Keep the public ABI stable while rejecting legacy NoDA/Avail selector values."
  require_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    "SYSCOIN: Compact Bitcoin DA has no Validium registration path."

  # A fresh production deployment has one cryptographic verifier route: final v0.4/V8 PLONK in slot 8.
  # The separately named testnet subclass retains type-3 fake proofs only when explicitly selected.
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "uint32 internal constant CANONICAL_ZKSYNC_OS_VERIFIER_VERSION = 8;"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "plonkVerifiers[CANONICAL_ZKSYNC_OS_VERIFIER_VERSION] = _plonkVerifier;"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "constructor(IVerifier _plonkVerifier, address _initialOwner)"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "_transferOwnership(_initialOwner);"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "if (version != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "if (verifierVersion != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {"
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "fflonkVerifiers"
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "FFLONK_VERIFIER"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function addVerifier(uint32 version, IVerifier _plonkVerifier) external onlyOwner"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function removeVerifier(uint32 version) external onlyOwner"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "if (verifierType == ZKSYNC_OS_PLONK_VERIFICATION_TYPE) {"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "else if (verifierType == ZKSYNC_OS_MOCK_VERIFICATION_TYPE) {"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "revert MockVerifierNotSupported();"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function IS_TESTNET_VERIFIER() external pure virtual override returns (bool) {"
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol" \
    "function IS_TESTNET_VERIFIER() external view returns (bool);"
  forbid_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol" \
    "function fflonkVerifiers"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    ": uint256(keccak256(abi.encodePacked(_publicInputs)));"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "keccak256(abi.encodePacked(initialHash, _publicInputs))"

  # The fake-prover lane is explicit and cannot be deployed for either supported production
  # root L1, including when its constructor executes on the distinct Gateway chain ID.
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "uint256 _l1ChainId"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "_l1ChainId != 0 &&"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "_l1ChainId != MAINNET_CHAIN_ID &&"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "_l1ChainId != SYSCOIN_MAINNET_CHAIN_ID &&"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "block.chainid != MAINNET_CHAIN_ID &&"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "block.chainid != SYSCOIN_MAINNET_CHAIN_ID"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol" \
    "function IS_TESTNET_VERIFIER() external pure override returns (bool) {"
  require_text \
    "l1-contracts/contracts/common/SyscoinConfig.sol" \
    "uint256 constant SYSCOIN_MAINNET_CHAIN_ID = 57;"

  # Direct and Gateway fresh paths retain generic Era FFLONK, but zkOS always reports zero FFLONK.
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "result.verifier = _config.testnetVerifier"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "constructor(GatewayVerifiersDeployerConfig memory _config, uint256 _l1ChainId)"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "_config.aliasedGovernanceAddress,"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "_l1ChainId"
  forbid_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "result.verifierFflonk ="
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    "if (!config.isZKsyncOS) {"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    "ctmAddresses.admin.governance,"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "return abi.encode(_plonk, _owner);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "return abi.encode(_plonk, _owner, _l1ChainId);"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "? abi.encode(verifiersConfig, config.l1ChainId)"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    ": abi.encode(verifiersConfig);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "function initializeVerifier(address _verifier, address _plonk, address _owner, bool _isZKsyncOS) internal view"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "function transferVerifierOwnership(address _verifier, address _newOwner, bool _isZKsyncOS) internal view"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "require(_verifier.owner() == _expectedOwner, \"ZKsyncOS verifier owner is not governance\");"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "require(_verifier.pendingOwner() == address(0), \"ZKsyncOS verifier has a pending ownership handoff\");"
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    ".addVerifier("
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    ".transferOwnership("

  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "uint32 internal constant DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 8;"
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "DEFAULT_ZKSYNC_OS_VERIFIER_VERSION = 6"
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "BlobsL1DAValidatorZKsyncOS"

  # Upgrade scripts default zkOS to production and require explicit environment opt-in for mock proofs.
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    '`SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER=true` only for an explicitly fake-prover-backed testnet.'
  require_text \
    "l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultCTMUpgrade.s.sol" \
    'vm.envOr("SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER", false)'
  require_text \
    "l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultGatewayUpgrade.s.sol" \
    'vm.envOr("SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER", false)'
  require_text \
    "l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultGatewayUpgrade.s.sol" \
    '? address(0)'

  # Focused Gateway regressions cover both the production route and explicit fake-prover route.
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    "function testGatewayVerifierDeployerZKsyncOSProductionRoute() external"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    'assertFalse(wrapper.IS_TESTNET_VERIFIER(), "production wrapper must reject fake proofs");'
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    "vm.expectRevert(MockVerifierNotSupported.selector);"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    "function testGatewayVerifierDeployerZKsyncOSExplicitTestnetRoute() external"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    'assertTrue(wrapper.IS_TESTNET_VERIFIER(), "explicit testnet wrapper must report fake-proof mode");'
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    "function testGatewayVerifierDeployerZKsyncOSRejectsSyscoinMainnetRootForTestnetRoute() external"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol" \
    "function testGatewayVerifierDeployerZKsyncOSRejectsEthereumMainnetRootForTestnetRoute() external"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/state-transition/verifiers/ZKsyncOSDualVerifier.t.sol" \
    "function test_testnetVerifierConstructor_revertsForZeroRootChain() public"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/state-transition/verifiers/ZKsyncOSDualVerifier.t.sol" \
    "function test_testnetVerifierConstructor_succeedsOnTestnetGateway() public"
  require_text \
    "l1-contracts/test/foundry/l1/unit/concrete/Utils/DeployCTML1OrGateway.t.sol" \
    "function test_genericEraVerifierCreationArgsRemainUpstreamEncoding() public"

  # Current generated inventory contains production, explicit testnet, and PLONK only.
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSDualVerifier"'
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSTestnetVerifier"'
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSVerifierPlonk"'
  forbid_text "AllContractsHashes.json" "ZKsyncOSVerifierFflonk"

  # Current resolver rejects the deleted identifier; the sole operational-looking reference elsewhere is V31 history.
  require_text \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    'revert("ContractsBytecodesLib: ZKsyncOS FFLONK verifier disabled");'
  require_text \
    "protocol-ops/src/upgrade_verification/versions/v31/elements/deployed_addresses.rs" \
    "SYSCOIN: historical V31 verification only"

  # The zkOS generator is PLONK-only; generic Era and custom generation retain FFLONK.
  require_text "tools/verifier-gen/src/main.rs" "fn zksync_os_variant_is_plonk_only()"
  require_text "tools/verifier-gen/src/main.rs" "assert!(fflonk_paths.is_none());"
  require_text "tools/verifier-gen/src/main.rs" "fn era_and_custom_variants_keep_fflonk()"
  forbid_text "tools/verifier-gen/src/main.rs" "ZKsyncOS_fflonk_scheduler_key.json"
  forbid_text "tools/verifier-gen/src/main.rs" "ZKsyncOSVerifierFflonk.sol"
  require_text "tools/verifier-gen/README.md" "### 2. ZKsyncOS Variant (PLONK only)"
  forbid_text "tools/verifier-gen/README.md" "ZKsyncOS_fflonk_scheduler_key.json"

  # CommitBatchInfo binds the compact edge input/root and the final V8 chain config.
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol" \
    "bytes edgeDARefsInput;"
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol" \
    "bytes32 edgeDARefsRoot;"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "_verifySyscoinEdgeDARefs(_newBatch.edgeDARefsInput, _newBatch.edgeDARefsRoot);"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "_newBatch.edgeDARefsRoot"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "if (_newBatch.chainId != s.chainId) {"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "if (daOutput.blobsLinearHashes.length != 0 || daOutput.blobsOpeningCommitments.length != 0) {"

  # The chain config hash is consensus data; governance cannot drift its max-tx-gas word away
  # from the value baked into the canonical application and server.
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol" \
    "if (_newMaxTxGasLimit > ZKSYNC_OS_DEFAULT_MAX_TX_GAS_LIMIT) {"
  forbid_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol" \
    "ZKSYNC_OS_MAX_BLOCK_GAS_LIMIT"

  # Direct-L1 and Gateway deployment entrypoints are hardwired to the compact Syscoin pair.
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'require(config.isZKsyncOS, "Only the canonical Syscoin ZKsync OS deployment is supported");'
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    '"SyscoinL1DAValidatorZKsyncOS",'
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    '"SyscoinRollupDAManager",'
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'deploySimpleContract("BlobsL1DAValidatorZKsyncOS"'
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'deploySimpleContract("ValidiumL1DAValidator"'
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'deploySimpleContract("AvailL1DAValidator"'
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol" \
    "new SyscoinRelayedSLDAValidator"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol" \
    "new SyscoinRollupDAManager"
  forbid_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol" \
    "new ValidiumL1DAValidator"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'require(config.isZKsyncOS, "Syscoin compact Gateway DA requires ZKsync OS");'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'return new bytes[](0);'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol" \
    'require(config.isZKsyncOS, "Only the canonical Syscoin ZKsync OS Gateway deployment is supported");'

  # The canonical manager remains governance-toggleable, but only for its immutable pair.
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    "address public immutable SYSCOIN_L1_DA_VALIDATOR;"
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    'require(_l1DAValidator == SYSCOIN_L1_DA_VALIDATOR, "Only the Syscoin DA validator is allowed");'
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    '"Only the Syscoin compact DA scheme is allowed"'

  require_text \
    "l1-contracts/contracts/common/SyscoinConfig.sol" \
    "L2DACommitmentScheme.BLOBS_ZKSYNC_OS;"
  require_text \
    "da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol" \
    "abi.encodePacked(_dataHash)"
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol" \
    "bytes32 actualCommitment = keccak256(_operatorDAInput);"

  # Old deployment selectors are not reachable through the canonical manifests/resolvers.
  require_text \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    'revert("ContractsBytecodesLib: legacy DA implementation disabled");'
  require_text \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    'require(daValidatorType == 0, "DA validator selector is reserved and must be zero");'
  forbid_text \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    "enum DAValidatorType"
  require_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    'require(!toml.readBool("$.chain.validium_mode"), "Syscoin compact DA does not support Validium");'
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    "if (config.validiumMode)"
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTMUtils.s.sol" \
    "getRollupL2DACommitmentScheme"
  require_text \
    "l1-contracts/test/foundry/l1/integration/deploy-scripts/script-config/config-deploy-ctm.toml" \
    "is_zk_sync_os = true"
  verify_absent_path \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol"
  verify_absent_path \
    "l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol"
  verify_absent_path \
    "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json"
  forbid_text ".github/workflows/slither.yaml" "ZKsyncOSVerifierFflonk.sol"
  forbid_text ".prettierignore" "ZKsyncOSVerifierFflonk.sol"
}

verify_verifier_artifacts_pending() {
  local stock_hits=0
  local relative_path expected_sha256 actual_sha256

  while read -r expected_sha256 relative_path; do
    [[ -n "${relative_path}" ]] || continue
    [[ -f "${CONTRACTS_PATH}/${relative_path}" && ! -L "${CONTRACTS_PATH}/${relative_path}" ]] ||
      die "verifier artifact is missing, not regular, or a symlink: ${relative_path}"
    actual_sha256="$(sha256_file "${CONTRACTS_PATH}/${relative_path}")"
    if [[ "${actual_sha256}" == "${expected_sha256}" ]]; then
      echo "stock verifier artifact rejected: ${relative_path} (${actual_sha256})" >&2
      stock_hits=$((stock_hits + 1))
    fi
  done <<'STOCK_VERIFIER_ARTIFACTS'
9926cf03b65cd404dfb1e5b2d6d9b487c60d2ead2d5b7998fcf0e3dd2249bb15 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol
368a53c438267cde0a21f49cb8fda8cc460e301462e205dfc87f5a01d7676bb9 tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json
STOCK_VERIFIER_ARTIFACTS

  if ((stock_hits > 0)); then
    die "canonical V8 VK regeneration required: stock app VK ${STOCK_APP_VK_HASH} is not bound to the patched Syscoin application"
  fi

  die "canonical V8 VK regeneration and attestation are still required; no app-bound security100 verifier hashes are approved"
}

verify_worktree_postimage_scope() {
  local status_paths status_count status_sha256
  git -C "${CONTRACTS_PATH}" diff --cached --quiet ||
    die "staged changes are not allowed in the patched Era-contracts worktree"
  status_paths="$(
    git -C "${CONTRACTS_PATH}" status --porcelain --untracked-files=all |
      sed -E 's/^.. //' |
      LC_ALL=C sort
  )"
  status_count="$(printf '%s\n' "${status_paths}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
  status_sha256="$(printf '%s\n' "${status_paths}" | sha256_stdin)"
  [[ "${status_count}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] ||
    die "patched worktree has unexpected path count: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${status_count}"
  [[ "${status_sha256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] ||
    die "patched worktree contains partial or unrelated changes"
}

BASE_FORWARD=false
BASE_REVERSE=false
patch_forward_applicable && BASE_FORWARD=true
patch_reverse_applicable && BASE_REVERSE=true

if [[ "${BASE_FORWARD}" == true && "${BASE_REVERSE}" == true ]]; then
  die "canonical patch state is ambiguous"
fi
if [[ "${BASE_FORWARD}" != true && "${BASE_REVERSE}" != true ]]; then
  die "canonical patch state is partial or diverged"
fi

if [[ "${BASE_FORWARD}" == true ]]; then
  if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain --untracked-files=all)" ]]; then
    git -C "${CONTRACTS_PATH}" status --porcelain --untracked-files=all >&2
    die "Era-contracts worktree must be clean before canonical patch application"
  fi
fi

# This pending-keygen gate is intentionally before every mutating operation,
# including submodule initialization and source-patch application.
verify_verifier_artifacts_pending

# Only top-level dependencies are part of this source attestation. Their own
# optional developer/test submodules are not needed for the canonical build.
git -C "${CONTRACTS_PATH}" submodule sync
git -C "${CONTRACTS_PATH}" submodule update --init

EXPECTED_GITLINK="$(git -C "${CONTRACTS_PATH}" ls-tree HEAD "${NESTED_PATH}" | awk '{print $3}')"
[[ "${EXPECTED_GITLINK}" == "${EXPECTED_NESTED_SHA}" ]] ||
  die "unexpected nested zksync-contracts gitlink: expected=${EXPECTED_NESTED_SHA} actual=${EXPECTED_GITLINK}"
ACTUAL_NESTED_SHA="$(git -C "${CONTRACTS_PATH}/${NESTED_PATH}" rev-parse HEAD)"
[[ "${ACTUAL_NESTED_SHA}" == "${EXPECTED_NESTED_SHA}" ]] ||
  die "nested zksync-contracts checkout mismatch: expected=${EXPECTED_NESTED_SHA} actual=${ACTUAL_NESTED_SHA}"

if [[ "${BASE_FORWARD}" == true ]]; then
  echo "Applying canonical Syscoin protocol-V32/execution-V7/proving-V8 Era-contracts patch..."
  git -C "${CONTRACTS_PATH}" apply --recount --whitespace=error-all "${PATCH_FILE}"
else
  echo "Canonical Syscoin protocol-V32/execution-V7/proving-V8 Era-contracts patch is already applied."
fi

patch_reverse_applicable ||
  die "canonical patch postimage failed reverse applicability"
verify_worktree_postimage_scope
verify_postimage_manifest
verify_semantics
git -C "${CONTRACTS_PATH}" diff --check

echo "Canonical Syscoin Era source patch is exact: protocol V32, execution V7, proving V8, verifier slot 8, compact DA only."
