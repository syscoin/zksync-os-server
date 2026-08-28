#!/usr/bin/env bash
set -euo pipefail

ASSERT_APPLIED=false
if [[ "${1:-}" == "--assert-applied" ]]; then
  ASSERT_APPLIED=true
  shift
fi

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 [--assert-applied] /absolute/path/to/era-contracts" >&2
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

EXPECTED_PATCH_SIZE="1419746"
EXPECTED_PATCH_SHA256="0e72ce962a53e928838205fba3efcd6e8140eaaf66e56c903a531e89252304c7"
EXPECTED_PATCH_PATH_COUNT="59"
EXPECTED_PATCH_PATHS_SHA256="d520d73b6b6b1001f4e8a845e2aa6e1fa04256c38d16cdb223b0643868fee5ff"
# SYSCOIN: Exact Git tree produced by applying the reviewed source-only patch to
# EXPECTED_BASE_TREE. Pending-VK mock launches must attest this postimage too.
EXPECTED_PATCHED_TREE="74b8ada2e8aa06701aa7496206dd72febf85346a"

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
# whitespace-clean and source-only; pinned upstream verifier generator/artifact bytes stay outside it.
if LC_ALL=C grep -n '^+.*[[:blank:]]$' "${PATCH_FILE}" >/dev/null; then
  die "canonical patch adds trailing whitespace"
fi
if grep -q '^deleted file mode ' "${PATCH_FILE}"; then
  die "canonical source patch unexpectedly deletes an upstream path"
fi
if grep -Eq '^(GIT binary patch$|Binary files )' "${PATCH_FILE}"; then
  die "canonical source patch unexpectedly contains a binary delta"
fi

PATCH_PATHS="$(
  git -C "${CONTRACTS_PATH}" apply --numstat --recount --unidiff-zero "${PATCH_FILE}" |
    awk -F '\t' '{print $3}' |
    LC_ALL=C sort
)"
PATCH_PATH_COUNT="$(printf '%s\n' "${PATCH_PATHS}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
PATCH_PATHS_SHA256="$(printf '%s\n' "${PATCH_PATHS}" | sha256_stdin)"
[[ "${PATCH_PATH_COUNT}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] ||
  die "canonical patch path count mismatch: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${PATCH_PATH_COUNT}"
[[ "${PATCH_PATHS_SHA256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] ||
  die "canonical patch path manifest mismatch"

# The app-bound PLONK contract/key are generated and attested separately. SYSCOIN: retain
# exact pinned-upstream FFLONK source/key/generator/deployment support outside this source patch;
# the proof router below attests that the compatibility verifier remains unreachable.
for forbidden_path in \
  "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierPlonk.sol" \
  "tools/verifier-gen/data/ZKsyncOS_plonk_scheduler_key.json" \
  "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol" \
  "tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json" \
  "tools/verifier-gen/data/fflonk_verifier_contract_template.txt" \
  "tools/verifier-gen/src/fflonk.rs" \
  "tools/verifier-gen/README.md" \
  "tools/verifier-gen/src/main.rs" \
  ".github/workflows/l1-contracts-ci.yaml" \
  ".github/workflows/slither.yaml" \
  ".prettierignore" \
  "l1-contracts/foundry.toml" \
  "l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol"
do
  if printf '%s\n' "${PATCH_PATHS}" | grep -Fqx "${forbidden_path}"; then
    die "canonical source patch unexpectedly changes verifier artifact: ${forbidden_path}"
  fi
done

patch_forward_applicable() {
  git -C "${CONTRACTS_PATH}" apply --check --recount --unidiff-zero --whitespace=error-all "${PATCH_FILE}" >/dev/null 2>&1
}

patch_reverse_applicable() {
  # Both directions enforce exact text whitespace for the source-only patch.
  git -C "${CONTRACTS_PATH}" apply --reverse --check --recount --unidiff-zero --whitespace=error-all "${PATCH_FILE}" >/dev/null 2>&1
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
  local manifest_paths="" manifest_count manifest_sha256
  while read -r expected_size expected_sha256 relative_path; do
    [[ -n "${relative_path}" ]] || continue
    verify_exact_file "${relative_path}" "${expected_size}" "${expected_sha256}"
    manifest_paths+="${relative_path}"$'\n'
  done <<'SYSCOIN_POSTIMAGE_MANIFEST'
3053 9e46ccc83139e8fb6d57a284631a2c1c90601525d5e44a05dc49d6a5988e216c .gitignore
160049 c1ad0208e77c01d3c2e91a7fe7e12624575dab9f231964e8a77223e787196725 AllContractsHashes.json
557518 5adf0dd1b618911d51c335e983c0c71cc1c74fc7db37161bf76a4b51e5055a95 configs/genesis/zksync-os/latest.json
1615 b9492bb3d1cbb976fbc2bd960707c194750202b9569f6c60e8bcdefa7353384e da-contracts/contracts/DAContractsErrors.sol
601 9201889972a107b91caec471ad95bb7c912fa1b2c0822004bb06f3629b1d2fd2 da-contracts/contracts/SyscoinDAUtils.sol
2999 24fcd082bee0ef29de5b4bd09b8e493a1bb1ef6759235ec71120668a19c417f4 da-contracts/contracts/SyscoinL1DAValidatorZKsyncOS.sol
5124 3f2388aba687e978b757ebc667bcc2639791a83404b7c205244c3c7429e03d89 da-contracts/test/foundry/SyscoinL1DAValidatorZKsyncOS.t.sol
2214 21f230d3d1fe830ce140d2c18275c66d96f66e335d08da021d55148ed0df0747 l1-contracts/contracts/bridge/BridgeHelper.sol
2792 36124022c94f3add992b220027d1e7a1f22ad9c813d3becf4d722f3f907f3d17 l1-contracts/contracts/common/StateTransitionTypes.sol
833 63a9033b60dd77f0c166c4f6f2177693717163e526bded1e6724196b5ad7422a l1-contracts/contracts/common/SyscoinConfig.sol
1311 c2138ea375da32973ecf228abd97adcf0c7099b48a38162bf9a223d81a7361b7 l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol
3945 6c64f59cf560d21a8c7223d86475df69c28a5348edc523f16f891a55b572fbd5 l1-contracts/contracts/script-interfaces/IDeployL2Contracts.sol
4350 46879b879bee93b99f2d1c549e64b304da215fe2da1281f54a658fb97d0ea98e l1-contracts/contracts/state-transition/L1StateTransitionErrors.sol
25974 b8afdf177f76cb229a5a98c3367775d3def34e7d7868b567bd08efb742d0698e l1-contracts/contracts/state-transition/chain-deps/facets/Admin.sol
49179 54751bc0aa7880c6c9182219aa418728986ae03eaca9abe98a14df1108316c8a l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol
2643 b9c43c8e79f715eb7b57eeaf17e3e9ae2f3157bc6f8496d9972b457c7afe8e97 l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol
3211 e5d289cbcb0bbd9f77b7a89f01fba3cc6bcb07f847754517c56d60b2f6c194bd l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol
8312 6825ecb5c046f7bcf1875c9b2045790dc96c67816b1fea2b0efaf6d20af1fec6 l1-contracts/contracts/state-transition/chain-interfaces/ICommitter.sol
855 186ecad8cf634eb5b3b03123468fb2239aa92fcad10a29e6b08175526ab194a3 l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol
4261 ca781b332ae1461d93fd89902f0e59c40f1db6f2846b511533a2e14aecba0c34 l1-contracts/contracts/state-transition/data-availability/RollupDAManager.sol
2727 c7f49220b06784bd67d73166fd9fb4e2329d7699d493b4342dfbcabfde683a10 l1-contracts/contracts/state-transition/data-availability/SyscoinRelayedSLDAValidator.sol
2303 a7a77cf790b20e91573ab5d5c30458b4aa1dc06f550e211d74e7f4afb448c04f l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol
10526 c9b04c90afedd8503fa3a27944b8b3446cd445213d0beac37410e857d8b63d77 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol
2210 99b2f630ccb303dc130e6010ae91ab2c462218a4cc813602ed307b9f05b95fe3 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSTestnetVerifier.sol
6707 2428a3ae1112ab7014cc332f0f087027d474055790e80d2c6d8957d8ce13ec05 l1-contracts/contracts/upgrades/L1FixedForceDeploymentsHelper.sol
10730 73b5cd2659d89e12ecdbefe2ed0e9753b1967eadaa4bf7ddfe5877ffe56c72dd l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol
31861 6af6435436f478f5723d0fe138771938b0f503ce9fdb7148cc98ae2742c8afc0 l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol
14452 db6f5326495f0e9926a15632ae8001d64887d9cb83fb64a1a8ffc3a0dbe35588 l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol
21890 285f15bd41c33ac64f19e20fb3853e867bd8491625b4d6968666166bb3a02260 l1-contracts/deploy-scripts/ctm/DeployCTMUtils.s.sol
26354 9fdb904b1613e219fa29f9e4dbaea017ba2311bec6e2ca358c41beb341bb2f36 l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol
41665 9bf5790131827c8671a648587e2f68794ff7bdc7b54b6a5539c6ee95aed91cca l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol
21027 6ebceffb5d2a5f6c083c8d78e79e0b97110e2653e5bd64c164f53d3cdc63f6de l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol
45338 212349849a53a7712cd68cb1065e00cc5a1abc451c2e4481df4c200c804ef3ed l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultCTMUpgrade.s.sol
31333 13eb6e18cd1806cf5154083c82299e4f1da1107d16812e125ee5040fd8832e5c l1-contracts/deploy-scripts/upgrade/default-upgrade/DefaultGatewayUpgrade.s.sol
23029 aff6be7d88b5426e117626bfbb717d292ca0531a7a5488620d5c4dfecd8ed27b l1-contracts/deploy-scripts/utils/AddressIntrospector.sol
11325 50276f9a9c4f059305b67471943159f0c195cc6c901968d6c2c1f9382db02754 l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol
858 9a6796cad5a4b8955ed797df04c19cdfc3d95494693f64bb818b1dc991635387 l1-contracts/script-config/syscoin-edge-da-relay-v1.json
2307622 ade53de26fa17c876045ae4817c932307b1a51b8edc6ae93e92e6bfa349238aa l1-contracts/selectors
18811 881846d3c06c9c660c8ee451ae5eb95d1fe324b65126a1a848ebe298cb93bf84 l1-contracts/test/foundry/l1/integration/GatewayVotePreparationTests.t.sol
17959 27e8c3a9f751b94e17a9c47b2303240cb5b375d0c09c1dbafda9b3039a1757a8 l1-contracts/test/foundry/l1/integration/UpgradeTestShared.t.sol
19299 1934c2776adb7c9c6b49fe5425cd0c19d4d27910d2fd3dbcadff3cd2240e4bab l1-contracts/test/foundry/l1/integration/UpgradeTestv31_Local.t.sol
8296 b18605c0bc27bdb37e37ed58bc5c3484b92dfb2b2e2f56e3a9d5b0804e9fe752 l1-contracts/test/foundry/l1/integration/_SharedL1ContractDeployer.t.sol
10900 6b9598f13155fc24026d0976332842a7ed6c10739a6fdb2c1cff567d4712ff49 l1-contracts/test/foundry/l1/integration/_SharedZKChainDeployer.t.sol
9378 0381dc84f5bb96727bfb06de2f9351d34ba5eae5b5423964f3a39e4aa731bf00 l1-contracts/test/foundry/l1/integration/deploy-scripts/script-config/config-deploy-ctm.toml
17139 7555ab6bee81f7c7133343d56e555a566b12d3ceabe6f61146d4bee53ccdad07 l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/CommittingZKsyncOS.t.sol
6691 88562d3c06e6339b4059966c0e3173eceb98a458205cb011bbb861ee7d5b7955 l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/ZKsyncOSPublicInput.t.sol
22740 dad85086f482c0d710834f2b6c8002946f2dd626c0cc44da0c0d0cdb0b45b045 l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/_Executor_Shared.t.sol
19467 e487d9ef43ae3d7bcbc941f035ec25e06b38ff8686629a7276a2fbaaf0e94943 l1-contracts/test/foundry/l1/unit/concrete/GatewayCTMDeployer/GatewayCTMDeployerZKsyncOS.t.sol
4828 3912fe8d417ec07ecb74a3a7e1e773907169bcafb56e4105cbdbfb0416f79da9 l1-contracts/test/foundry/l1/unit/concrete/Utils/DeployCTML1OrGateway.t.sol
37262 e6cb6d802f989a56ac8904a2b7d941a7203f664e8e2dbb8a3bf56f6dab74c069 l1-contracts/test/foundry/l1/unit/concrete/Utils/Utils.sol
7994 ee66867257ad8f7856ddcd660f44c36db9aea6c33c1c1ff8c75bde2221d03887 l1-contracts/test/foundry/l1/unit/concrete/state-transition/chain-deps/facets/Admin/MakePermanentRollup.t.sol
4368 cac4d80e69aed56f6732cb6024706e5286b01fb3d2868917716081da5f6d9c3b l1-contracts/test/foundry/l1/unit/concrete/state-transition/chain-deps/facets/Admin/SetZKsyncOSChainConfig.t.sol
4147 72dc6373a6093ea18a8d71522dda3836d66d9b3056660787c00ced1499a24967 l1-contracts/test/foundry/l1/unit/concrete/state-transition/data-availability/SyscoinRelayedSLDAValidator.t.sol
3454 9cc8e015664c0fe9b5bfca6e12a29120cf611775df6dbbd7d00d45e90dc74ea8 l1-contracts/test/foundry/l1/unit/concrete/state-transition/data-availability/SyscoinRollupDAManager.t.sol
18331 8600ed5a07fa68e2cb180674320406523a98660c0b6f78a538568ef935ec140e l1-contracts/test/foundry/l1/unit/concrete/state-transition/verifiers/ZKsyncOSDualVerifier.t.sol
17824 85171d93961f63606c4449f9cfbae0e8341daa942d659513d42ed0018850a758 l1-contracts/test/foundry/l2/unit/GatewayCTMDeployer/GatewayCTMDeployer.t.sol
5070 2bbab9425954d54c8dc0e0f963e4f9befb35eae6a34bcdb8ccb854409526487f l1-contracts/test/foundry/l2/unit/GatewayCTMDeployer/SyscoinGatewayCTMDeployerDA.t.sol
14102 8db8cf9b188baf96c2634fcab0c4e54512254c0b8737a066ad540ae7e5102a4e l1-contracts/zkstack-out/IDeployCTM.sol/IDeployCTM.json
11865 2d470cd020bad4178adc1cd12889693e235df86103be24bf25549ac411613b6d tools/zksync-os-genesis-gen/src/consts.rs
SYSCOIN_POSTIMAGE_MANIFEST

  manifest_paths="${manifest_paths%$'\n'}"
  manifest_count="$(printf '%s\n' "${manifest_paths}" | sed '/^$/d' | wc -l | tr -d '[:space:]')"
  manifest_sha256="$(printf '%s\n' "${manifest_paths}" | sha256_stdin)"
  [[ "${manifest_count}" == "${EXPECTED_PATCH_PATH_COUNT}" ]] ||
    die "postimage manifest path count mismatch: expected=${EXPECTED_PATCH_PATH_COUNT} actual=${manifest_count}"
  [[ "${manifest_sha256}" == "${EXPECTED_PATCH_PATHS_SHA256}" ]] ||
    die "postimage manifest path digest mismatch"
  [[ "${manifest_paths}" == "${PATCH_PATHS}" ]] ||
    die "postimage manifest does not exactly match the canonical patch path set"
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

verify_semantics() {
  # Intentional downstream source/config deviations remain visibly attributable after rebases.
  # Generated JSON artifacts and the selector inventory are exact-manifest checked above but
  # exempt from literal provenance comments.
  local tagged_path
  while IFS= read -r tagged_path; do
    case "${tagged_path}" in
      *.sol | *.rs | *.toml | *.gitignore)
        require_text "${tagged_path}" "SYSCOIN:"
        ;;
    esac
  done <<< "${PATCH_PATHS}"

  # The most security-sensitive application-bound ABI restrictions are tagged in place.
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
    "SYSCOIN: Retain the upstream config tuple shape, but reject its unsupported Validium branch."

  # A fresh production deployment retains the pinned-upstream verifier pair at slot 8, but has
  # exactly one cryptographic route: final v0.4/V8 PLONK/type 2. FFLONK is deployment-compatible
  # and introspectable only; the separately named testnet subclass owns the explicit type-3 route.
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "uint32 internal constant CANONICAL_ZKSYNC_OS_VERIFIER_VERSION = 8;"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "fflonkVerifiers[CANONICAL_ZKSYNC_OS_VERIFIER_VERSION] = _fflonkVerifier;"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "plonkVerifiers[CANONICAL_ZKSYNC_OS_VERIFIER_VERSION] = _plonkVerifier;"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "constructor(IVerifierV2 _fflonkVerifier, IVerifier _plonkVerifier, address _initialOwner)"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "_transferOwnership(_initialOwner);"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "if (version != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "if (verifierVersion != CANONICAL_ZKSYNC_OS_VERIFIER_VERSION) {"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "mapping(uint32 => IVerifierV2) public fflonkVerifiers;"
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "ZKSYNC_OS_FFLONK_VERIFICATION_TYPE"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function replaceVerifier(uint32 version, IVerifier newPlonkVerifier) external override onlyOwner"
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function addVerifier("
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "function removeVerifier("
  forbid_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "fflonkVerifiers[verifierVersion].verify"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "revert AddressHasNoCode(_verifier);"
  require_text \
    "l1-contracts/contracts/state-transition/verifiers/ZKsyncOSDualVerifier.sol" \
    "require(_initialOwner != address(0), ZeroAddress());"
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
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol" \
    "function fflonkVerifiers"
  require_text \
    "l1-contracts/contracts/state-transition/chain-interfaces/IZKsyncOSDualVerifier.sol" \
    "function replaceVerifier(uint32 version, IVerifier newPlonkVerifier) external;"
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

  # Direct and Gateway fresh paths retain the pinned-upstream FFLONK artifact and pass both
  # implementations to the wrapper at canonical slot 8; only PLONK is proof-routed.
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
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerVerifiersZKsyncOS.sol" \
    "result.verifierFflonk = address(new ZKsyncOSVerifierFflonk{salt: salt}());"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    "ctmAddresses.stateTransition.verifiers.verifierFflonk = deploySimpleContract(fflonkName, false);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    "ctmAddresses.admin.governance,"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "return abi.encode(_fflonk, _plonk, _owner);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "return abi.encode(_fflonk, _plonk, _owner, _l1ChainId);"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "? abi.encode(verifiersConfig, config.l1ChainId)"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    ": abi.encode(verifiersConfig);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "IVerifierV2 currentFflonk = verifier.fflonkVerifiers(DEFAULT_ZKSYNC_OS_VERIFIER_VERSION);"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "address(currentFflonk) == _fflonk && address(currentPlonk) == _plonk"
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    "fflonk = address(verifier.fflonkVerifiers(DEFAULT_ZKSYNC_OS_VERIFIER_VERSION));"
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
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTML1OrGateway.sol" \
    'return _isZKsyncOS ? "ZKsyncOSVerifierFflonk" : "EraVerifierFflonk";'

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
    'gatewayConfig.gatewayStateTransition.verifiers.verifierFflonk = deployGWContract("VerifierFflonk");'

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

  # Current generated inventory contains both pinned-upstream implementations and both wrappers.
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSDualVerifier"'
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSTestnetVerifier"'
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSVerifierPlonk"'
  require_text "AllContractsHashes.json" '"contractName": "l1-contracts/ZKsyncOSVerifierFflonk"'

  # SYSCOIN: exact pinned-upstream FFLONK generator and bytecode route remain present but dormant.
  require_text \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    'return Utils.readBytecodeL1(false, "ZKsyncOSVerifierFflonk.sol", "ZKsyncOSVerifierFflonk");'

  require_text "tools/verifier-gen/src/main.rs" '"data/ZKsyncOS_fflonk_scheduler_key.json".to_string()'
  require_text "tools/verifier-gen/src/main.rs" '"data/ZKsyncOSVerifierFflonk.sol".to_string()'
  require_text "tools/verifier-gen/README.md" '### 2. ZKsyncOS Variant'
  require_text "tools/verifier-gen/README.md" 'data/ZKsyncOS_fflonk_scheduler_key.json'

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
    "uint256 totalRefs;"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "if (totalRefs > SYSCOIN_DA_MAX_REFS_PER_BATCH) {"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/facets/Committer.sol" \
    "_maxBlobsSupported: SYSCOIN_DA_MAX_REFS_PER_BATCH"
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
  # SYSCOIN: the relay identity is frozen independently of the manager salt and authenticated
  # before settlement can bind it. Nested or alternate-factory deployment is forbidden.
  require_text \
    "l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol" \
    "address constant SYSCOIN_EDGE_DA_RELAY_ADDRESS = 0x758b06cDA80BDD016F79AFd0df1A984039067A21;"
  require_text \
    "l1-contracts/contracts/common/SyscoinEdgeDARelayDeployment.sol" \
    "bytes32 constant SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH"
  require_text \
    "l1-contracts/contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployerDA.sol" \
    "actualRelayCodeHash != SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH"
  forbid_text \
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
    "_validateSyscoinEdgeDARelayArtifact();"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'Utils.vm.envOr("SYSCOIN_EDGE_DA_RELAY_ARTIFACT", defaultArtifact)'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'Utils.vm.parseJsonBytes(artifact, ".bytecode.object")'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'Utils.vm.parseJsonBytes(artifact, ".deployedBytecode.object")'
  forbid_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "syscoinEdgeDARelayCalldata"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "actualInitCodeHash != SYSCOIN_EDGE_DA_RELAY_INIT_CODE_HASH"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    "actualRuntimeHash != SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH"
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayCTMDeployerHelper.sol" \
    'return new bytes[](0);'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol" \
    'require(config.isZKsyncOS, "Only the canonical Syscoin ZKsync OS Gateway deployment is supported");'
  forbid_text \
    "l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol" \
    "runGatewayL1L2Transaction(create2FactoryAddress, deployerCalldata.syscoinEdgeDARelayCalldata);"
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    'pub const SYSCOIN_EDGE_DA_RELAY_ADDRESS: Address = Address(FixedBytes::<20>(hex_literal::hex!('
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    '"758b06cDA80BDD016F79AFd0df1A984039067A21"'
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    'pub const INITIAL_CONTRACTS: [(Address, ContractDeployment); 23] = ['
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    'ContractDeployment::Direct(ContractSource::L1ContractName('
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    '"SyscoinRelayedSLDAValidator",'
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    'canonical zkOS genesis must contain only the pinned 41 contracts'
  require_text \
    "tools/zksync-os-genesis-gen/src/consts.rs" \
    '"ec4a6d11ed43e56364b38684633718eea0c3c270849ccef03dfcf2721a2b77fb"'
  require_text \
    "configs/genesis/zksync-os/latest.json" \
    '"0x758b06cda80bdd016f79afd0df1a984039067a21",'
  require_text \
    "configs/genesis/zksync-os/latest.json" \
    '"genesis_root": "0xec4a6d11ed43e56364b38684633718eea0c3c270849ccef03dfcf2721a2b77fb"'
  require_text \
    "l1-contracts/test/foundry/l1/integration/GatewayVotePreparationTests.t.sol" \
    '0xec4a6d11ed43e56364b38684633718eea0c3c270849ccef03dfcf2721a2b77fb;'
  require_text \
    "l1-contracts/test/foundry/l1/integration/GatewayVotePreparationTests.t.sol" \
    '0xf537449b2ae8774f0073e37e622c7b69744cfc985baf8236be2c82411a161191;'
  require_text \
    "l1-contracts/deploy-scripts/gateway/GatewayVotePreparation.s.sol" \
    'vm.serializeAddress("root", "validium_da_validator", output.validiumDAValidator);'
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'vm.serializeAddress("deployed_addresses", "no_da_validium_l1_validator_addr", address(0));'
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'vm.serializeAddress("deployed_addresses", "blobs_zksync_os_l1_da_validator_addr", address(0));'
  require_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTM.s.sol" \
    'vm.serializeAddress("deployed_addresses", "avail_l1_da_validator_addr", address(0));'

  # The canonical manager remains governance-toggleable, but only for its immutable pair.
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    "address public immutable SYSCOIN_L1_DA_VALIDATOR;"
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    "revert SyscoinL1DAValidatorMismatch(SYSCOIN_L1_DA_VALIDATOR, _l1DAValidator);"
  require_text \
    "l1-contracts/contracts/state-transition/data-availability/SyscoinRollupDAManager.sol" \
    "revert SyscoinL2DACommitmentSchemeMismatch(SYSCOIN_ROLLUP_L2_DA_COMMITMENT_SCHEME, _l2DACommitmentScheme);"

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
    '"SyscoinL1DAValidatorZKsyncOS",'
  require_text \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    '"SyscoinRelayedSLDAValidator",'
  require_text \
    "l1-contracts/deploy-scripts/utils/bytecode/ContractsBytecodesLib.sol" \
    '"SyscoinRollupDAManager",'
  require_text \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    'require(daValidatorType == 0, "DA validator selector is reserved and must be zero");'
  forbid_text \
    "l1-contracts/deploy-scripts/chain/DeployL2Contracts.sol" \
    "enum DAValidatorType"
  require_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    'config.validiumMode = toml.readBool("$.chain.validium_mode");'
  require_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    'require(!config.validiumMode, "Syscoin compact DA does not support Validium");'
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/RegisterZKChain.s.sol" \
    "if (config.validiumMode)"
  forbid_text \
    "l1-contracts/deploy-scripts/ctm/DeployCTMUtils.s.sol" \
    "getRollupL2DACommitmentScheme"
  require_text \
    "l1-contracts/test/foundry/l1/integration/deploy-scripts/script-config/config-deploy-ctm.toml" \
    "is_zk_sync_os = true"
  # SYSCOIN: these exact pinned-upstream compatibility, deployment, generator, and
  # review-tool inputs are intentionally retained byte-for-byte outside the patch.
  while read -r expected_size expected_sha256 relative_path; do
    [[ -n "${relative_path}" ]] || continue
    verify_exact_file "${relative_path}" "${expected_size}" "${expected_sha256}"
  done <<'SYSCOIN_RETAINED_UPSTREAM_MANIFEST'
4895 cfa792fc502364d12c855c02724ceef0843aa193b711630fb87326e16197e4bd l1-contracts/foundry.toml
58881 4e272ef47b1ba6fbbdd546e8da4b97a130463b54ad48eb03431dfb59e6e44b2e l1-contracts/test/foundry/l1/unit/concrete/BatchProcessing/Committing.t.sol
77746 9308b1850d4197bd7b6a59cc35029f51b94ffce76f5951848669fd9424a07d48 l1-contracts/contracts/state-transition/verifiers/ZKsyncOSVerifierFflonk.sol
1920 a1d093cf2bb0f5331c4a6bbf0e40d5f4888cc850324e8b9e406bde6686f07f77 tools/verifier-gen/data/ZKsyncOS_fflonk_scheduler_key.json
75842 b2b292b85a7f676d18bee0a0e98af3dbbd4bc05bcaccef1b8260e195652db647 tools/verifier-gen/data/fflonk_verifier_contract_template.txt
5122 7f015b5fbaebf4e21357c56db3282507256b0eb0bb44ed33f49b3d4be0c4c098 tools/verifier-gen/src/fflonk.rs
5962 f63ab6897dd986a6f5f36e1759d1d032f573c4aea9fcba42bc45a33c85df0e65 tools/verifier-gen/src/main.rs
1812 29736c7e0ad4a2e8e5b67e4f2de3064a05aef12db19783bb08635fbb2ed43cdc tools/verifier-gen/README.md
18272 315661d42cad03e6dbddf995f5f7f9fd3a5716518274d3c620e37922cae3490c .github/workflows/l1-contracts-ci.yaml
2656 e4c067ed467721e54b967fc57d1342f523c8690f7a5bb5726b348a449529c444 .github/workflows/slither.yaml
1510 18c4ad86772fc5e41d8241d1a7b2dc5f510610d99af44325ddb0ce98977b39bf .prettierignore
SYSCOIN_RETAINED_UPSTREAM_MANIFEST
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

pending_v8_mock_source_mode_enabled() {
  # SYSCOIN: This is the sole pre-keygen source-materialization exception. Both
  # the operator-facing mode and explicit on-chain mock-verifier opt-in must be
  # exact, and no chain-specific override may silently select GPU proving.
  [[ "${PROVER_MODE:-}" == "no-proofs" ]] || return 1
  [[ "${SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER:-}" == "true" ]] || return 1
  [[ "${GATEWAY_PROVER_MODE:-no-proofs}" == "no-proofs" ]] || return 1
  [[ "${EDGE_PROVER_MODE:-no-proofs}" == "no-proofs" ]] || return 1
  # SYSCOIN: Standalone invocation must bind the exception to one reviewed
  # non-production network/chain pair even if the outer launcher is skipped.
  case "${L1_NETWORK:-}:${L1_CHAIN_ID:-}" in
    localhost:31337 | tanenbaum:5700) ;;
    *) return 1 ;;
  esac
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

verify_worktree_postimage_tree() {
  # SYSCOIN: Reproduce the reviewed patched tree with an isolated index. This
  # binds the mock-only exception to every postimage byte without touching the
  # operator's real index or accepting an equivalent-but-unreviewed patch.
  local temporary_dir temporary_index temporary_objects canonical_objects
  local existing_alternates
  local actual_patched_tree relative_path
  temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/syscoin-era-patch-index.XXXXXX")"
  temporary_index="${temporary_dir}/index"
  temporary_objects="${temporary_dir}/objects"
  mkdir "${temporary_objects}"
  canonical_objects="$(git -C "${CONTRACTS_PATH}" rev-parse --git-path objects)"
  if [[ "${canonical_objects}" != /* ]]; then
    canonical_objects="${CONTRACTS_PATH}/${canonical_objects}"
  fi
  canonical_objects="$(cd "${canonical_objects}" && pwd -P)"
  existing_alternates="${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}"

  if ! (
    export GIT_INDEX_FILE="${temporary_index}"
    # SYSCOIN: Assertion-only attestation must not insert blobs or trees into
    # the reviewed checkout's object database. Read canonical objects through
    # an alternate and direct every write into the disposable directory.
    export GIT_OBJECT_DIRECTORY="${temporary_objects}"
    export GIT_ALTERNATE_OBJECT_DIRECTORIES="${canonical_objects}"
    if [[ -n "${existing_alternates}" ]]; then
      export GIT_ALTERNATE_OBJECT_DIRECTORIES="${canonical_objects}:${existing_alternates}"
    fi
    git -C "${CONTRACTS_PATH}" read-tree HEAD || exit 1
    while IFS= read -r relative_path; do
      [[ -n "${relative_path}" ]] || continue
      git -C "${CONTRACTS_PATH}" add -A -- "${relative_path}" || exit 1
    done <<< "${PATCH_PATHS}"
    git -C "${CONTRACTS_PATH}" write-tree >"${temporary_dir}/tree"
  ); then
    rm -rf "${temporary_dir}"
    die "failed to calculate canonical patched Era-contracts tree"
  fi
  actual_patched_tree="$(tr -d '\r\n' <"${temporary_dir}/tree")"
  rm -rf "${temporary_dir}"

  [[ "${actual_patched_tree}" == "${EXPECTED_PATCHED_TREE}" ]] ||
    die "patched Era-contracts tree mismatch: expected=${EXPECTED_PATCHED_TREE} actual=${actual_patched_tree}"
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
  [[ "${ASSERT_APPLIED}" != true ]] ||
    die "assert-applied mode refuses to materialize the canonical Era-contracts patch"
  if [[ -n "$(git -C "${CONTRACTS_PATH}" status --porcelain --untracked-files=all)" ]]; then
    git -C "${CONTRACTS_PATH}" status --porcelain --untracked-files=all >&2
    die "Era-contracts worktree must be clean before canonical patch application"
  fi
fi

# SYSCOIN: Production/GPU paths retain the original pre-mutation pending-keygen
# failure. Only an explicitly fake-prover-backed launch may materialize the
# reviewed source patch before VK generation; its exact postimage is attested
# below before this helper can report success.
PENDING_V8_MOCK_SOURCE_MODE=false
if pending_v8_mock_source_mode_enabled; then
  PENDING_V8_MOCK_SOURCE_MODE=true
  echo "SYSCOIN: allowing exact pending-V8 Era source materialization for no-proofs/mock-verifier launch only" >&2
else
  verify_verifier_artifacts_pending
fi

# SYSCOIN: Forge auto-discovers remappings from nested dependencies and commits them to IPFS
# metadata. Materialization mode populates the exact graph; assertion-only mode
# must never repair source after a build and instead requires it to be present.
if [[ "${ASSERT_APPLIED}" != true ]]; then
  git -C "${CONTRACTS_PATH}" submodule sync --recursive
  git -C "${CONTRACTS_PATH}" submodule update --init --recursive
fi

EXPECTED_GITLINK="$(git -C "${CONTRACTS_PATH}" ls-tree HEAD "${NESTED_PATH}" | awk '{print $3}')"
[[ "${EXPECTED_GITLINK}" == "${EXPECTED_NESTED_SHA}" ]] ||
  die "unexpected nested zksync-contracts gitlink: expected=${EXPECTED_NESTED_SHA} actual=${EXPECTED_GITLINK}"
ACTUAL_NESTED_SHA="$(git -C "${CONTRACTS_PATH}/${NESTED_PATH}" rev-parse HEAD)"
[[ "${ACTUAL_NESTED_SHA}" == "${EXPECTED_NESTED_SHA}" ]] ||
  die "nested zksync-contracts checkout mismatch: expected=${EXPECTED_NESTED_SHA} actual=${ACTUAL_NESTED_SHA}"

if [[ "${BASE_FORWARD}" == true ]]; then
  echo "Applying canonical Syscoin protocol-V32/execution-V7/proving-V8 Era-contracts patch..."
  git -C "${CONTRACTS_PATH}" apply --recount --unidiff-zero --whitespace=error-all "${PATCH_FILE}"
else
  echo "Canonical Syscoin protocol-V32/execution-V7/proving-V8 Era-contracts patch is already applied."
fi

patch_reverse_applicable ||
  die "canonical patch postimage failed reverse applicability"
verify_worktree_postimage_scope
verify_postimage_manifest
verify_semantics
git -C "${CONTRACTS_PATH}" diff --check
verify_worktree_postimage_tree

if [[ "${PENDING_V8_MOCK_SOURCE_MODE}" == true ]]; then
  echo "SYSCOIN: pending-V8 mock source postimage matches exact reviewed tree ${EXPECTED_PATCHED_TREE}" >&2
fi

echo "Canonical Syscoin Era source patch is exact: protocol V32, execution V7, proving V8, verifier slot 8, compact DA only."
