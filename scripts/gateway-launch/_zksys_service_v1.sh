#!/usr/bin/env bash
# Sourced by zksys-l2-bootstrap after the canonical issuer is wired.

zksys_assert_service_mode_matches_bridge() {
  local mode="${1:?mode}" level1="${2:?level1}" level2="${3:?level2}"
  if [ "${mode}" = true ]; then
    [ "${level1}:${level2}" = 3500:10000 ] || gl_die "service V1 requires bridge bps 3500/10000"
  elif [ "${level1}:${level2}" != 0:0 ]; then
    gl_die "bridge has nonzero seniority bonuses but period-zero prover service is not selected"
  fi
}

zksys_assert_shared_bridgehub() {
  [ "$(gl_to_lower "${1:?root sink Bridgehub}")" = "$(gl_to_lower "${2:?registry Bridgehub}")" ] ||
    gl_die "root sink Bridgehub does not match the L1 registry bridge"
}

zksys_assert_staking_deferred() {
  local current
  current="$(call_l2 "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)')"
  if [ "${current}" = 0 ]; then
    assert_l2_bool_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'hasRole(bytes32,address)(bool)' false \
      "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}"
  fi
}

zksys_enable_staking() {
  local enabled
  enabled="$(call_l2 "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'hasRole(bytes32,address)(bool)' \
    "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}")"
  if [ "${enabled}" = false ]; then
    send_l2 "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'grantRole(bytes32,address)' \
      "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}"
  fi
  assert_l2_bool_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'hasRole(bytes32,address)(bool)' true \
    "${STAKE_WEIGHT_UPDATER_ROLE}" "${ZKSYS_L2_STAKING_VAULT_ADDRESS}"
}

zksys_assert_connected_service_v1() {
  local manifest service
  manifest="$(gl_checkpoint_state_dir)/zksys-service-v1.json"
  service="$(python3 - "${manifest}" <<'PY'
import json, os, re, stat, sys
path = sys.argv[1]
st = os.lstat(path)
if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or stat.S_IMODE(st.st_mode) & 0o077:
    raise SystemExit('unsafe service V1 manifest')
value = json.load(open(path)).get('service', '')
if not re.fullmatch(r'0x[0-9a-f]{40}', value):
    raise SystemExit('invalid service V1 manifest address')
print(value)
PY
)"
  assert_l2_uint_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)' 1
  assert_l2_uint_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)' 1
  assert_l2_address_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceSource()(address)' "${service}"
  assert_l2_bool_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceLaunchAborted()(bool)' false
}

zksys_service_v1_addresses() {
  local salt code init factory receiver registry
  salt="$(normalize_bytes32_env ZKSYS_SERVICE_FACTORY_SALT 0x7a6b7379732d736572766963652d76312d666163746f72790000000000000000)"
  code="$(forge_inspect_bytecode ZkSysServiceDeploymentFactoryV1)"
  init="${code}$(cast abi-encode 'constructor(address)' "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" | cut -c 3-)"
  factory="$(cast create2 --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" --salt "${salt}" --init-code "${init}")"
  receiver="$(cast compute-address "${factory}" --nonce 1 | awk '{print $NF}')"
  registry="$(cast compute-address "${factory}" --nonce 2 | awk '{print $NF}')"
  printf '%s %s %s\n' "${factory}" "${receiver}" "${registry}"
}

zksys_setup_service_v1() {
  local name gateway_rpc gateway_id edge_id factory_salt factory_code factory_init factory_address
  local receiver_address service_address receiver_init service_init config commitment current factory_runtime
  local root_gate root_code gate_code sink_code root_bridgehub actual value
  for name in ZKSYS_SERVICE_GATEWAY_GATE ZKSYS_SERVICE_ROOT_SINK ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS \
    ZKSYS_SERVICE_SETTLEMENT_CHAIN_ADDRESS ZKSYS_SERVICE_SEQUENCER ZKSYS_SERVICE_POLICY_HASH \
    ZKSYS_SERVICE_GATEWAY_VK_HASH ZKSYS_SERVICE_GATEWAY_GATE_CODE_HASH ZKSYS_SERVICE_ROOT_SINK_CODE_HASH \
    ZKSYS_SERVICE_ROOT_GATE_CODE_HASH ZKSYS_SERVICE_RECEIPT_GRACE_SECONDS \
    ZKSYS_SERVICE_ROSTER_LEAD_SECONDS ZKSYS_SERVICE_DUTIES_PER_ROUND; do
    gl_require "${name}"
  done
  for name in ZKSYS_SERVICE_GATEWAY_GATE ZKSYS_SERVICE_ROOT_SINK ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS \
    ZKSYS_SERVICE_SETTLEMENT_CHAIN_ADDRESS ZKSYS_SERVICE_SEQUENCER; do
    printf -v "${name}" '%s' "$(normalize_nonzero_address_env "${name}")"
  done
  for name in ZKSYS_SERVICE_POLICY_HASH ZKSYS_SERVICE_GATEWAY_VK_HASH ZKSYS_SERVICE_GATEWAY_GATE_CODE_HASH \
    ZKSYS_SERVICE_ROOT_SINK_CODE_HASH ZKSYS_SERVICE_ROOT_GATE_CODE_HASH; do
    printf -v "${name}" '%s' "$(normalize_bytes32_env "${name}" 0)"
    [ "${!name}" != "0x$(printf '%064d' 0)" ] || gl_die "${name} must not be zero"
  done
  gateway_rpc="$(gl_gateway_runtime_rpc_url)"
  gateway_id="$(gl_gateway_chain_id_from_config)"
  edge_id="$(gl_non_l1_cast chain-id --rpc-url "${ZKSYS_L2_RPC_URL}")"
  [ "$(gl_non_l1_cast chain-id --rpc-url "${gateway_rpc}")" = "${gateway_id}" ] || gl_die "wrong Gateway RPC chain id"

  factory_salt="$(normalize_bytes32_env ZKSYS_SERVICE_FACTORY_SALT 0x7a6b7379732d736572766963652d76312d666163746f72790000000000000000)"
  factory_code="$(forge_inspect_bytecode ZkSysServiceDeploymentFactoryV1)"
  factory_init="${factory_code}$(cast abi-encode 'constructor(address)' "${ZKSYS_L2_TOKEN_ADMIN_ADDRESS}" | cut -c 3-)"
  factory_address="$(cast create2 --deployer "${ZKSYS_L2_CREATE2_DEPLOYER}" --salt "${factory_salt}" --init-code "${factory_init}")"
  receiver_address="$(cast compute-address "${factory_address}" --nonce 1 | awk '{print $NF}')"
  service_address="$(cast compute-address "${factory_address}" --nonce 2 | awk '{print $NF}')"
  receiver_init="$(forge_inspect_bytecode ZkSysAcceptedServiceReceiverV1)$(cast abi-encode 'constructor(uint256,address,address,address)' "${gateway_id}" "${ZKSYS_SERVICE_GATEWAY_GATE}" "${service_address}" "${ZKSYS_SERVICE_ROOT_SINK}" | cut -c 3-)"
  config="(${ZKSYS_L2_REGISTRY_ADDRESS},${ZKSYS_L2_ISSUER_ADDRESS},${receiver_address},${ZKSYS_SERVICE_SETTLEMENT_CHAIN_ADDRESS},${ZKSYS_SERVICE_POLICY_HASH},0,${ZKSYS_SERVICE_RECEIPT_GRACE_SECONDS},${ZKSYS_SERVICE_ROSTER_LEAD_SECONDS},${ZKSYS_SERVICE_DUTIES_PER_ROUND},${gateway_id},${ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS},${ZKSYS_SERVICE_GATEWAY_VK_HASH},${ZKSYS_SERVICE_SEQUENCER})"
  service_init="$(forge_inspect_bytecode ZkSysProverServiceRegistryV1)$(cast abi-encode 'constructor((address,address,address,address,bytes32,uint64,uint64,uint64,uint16,uint256,address,bytes32,address))' "${config}" | cut -c 3-)"
  commitment="$(cast keccak "$(cast abi-encode 'f(bytes,bytes)' "${receiver_init}" "${service_init}")")"

  # The external contracts must already be deployed and pinned before issuer configuration.
  gate_code="$(gl_non_l1_cast code --rpc-url "${gateway_rpc}" "${ZKSYS_SERVICE_GATEWAY_GATE}")"
  sink_code="$(cast code --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}")"
  [ "${gate_code}" != 0x ] && [ "$(gl_to_lower "$(cast keccak "${gate_code}")")" = "${ZKSYS_SERVICE_GATEWAY_GATE_CODE_HASH}" ] || gl_die "Gateway gate code hash mismatch"
  [ "${sink_code}" != 0x ] && [ "$(gl_to_lower "$(cast keccak "${sink_code}")")" = "${ZKSYS_SERVICE_ROOT_SINK_CODE_HASH}" ] || gl_die "root sink code hash mismatch"
  root_gate="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}" 'publisher()(address)')"
  root_code="$(cast code --rpc-url "${L1_RPC_URL}" "${root_gate}")"
  [ "${root_code}" != 0x ] && [ "$(gl_to_lower "$(cast keccak "${root_code}")")" = "${ZKSYS_SERVICE_ROOT_GATE_CODE_HASH}" ] || gl_die "root gate code hash mismatch"
  for value in 'seniorityHeight1()(uint32):210240' 'seniorityHeight2()(uint32):525600' 'seniorityLevel1Bps()(uint16):3500' 'seniorityLevel2Bps()(uint16):10000'; do
    actual="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" "${value%:*}")"
    actual="${actual%% *}"
    [ "${actual}" = "${value#*:}" ] || gl_die "L1 bridge ${value%:*} is ${actual}, expected ${value#*:}"
  done
  for value in "registryReceiver()(address):${receiver_address}" "gatewayChainAddress()(address):${ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS}" "gatewayGate()(address):${ZKSYS_SERVICE_GATEWAY_GATE}"; do
    actual="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}" "${value%:*}")"
    [ "$(gl_to_lower "${actual}")" = "$(gl_to_lower "${value#*:}")" ] || gl_die "root sink ${value%:*} mismatch"
  done
  root_bridgehub="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}" 'bridgehub()(address)')"
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_L1_REGISTRY_BRIDGE_ADDRESS}" 'bridgehub()(address)')"
  zksys_assert_shared_bridgehub "${root_bridgehub}" "${actual}"
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${root_bridgehub}" 'getZKChain(uint256)(address)' "${gateway_id}")"
  [ "$(gl_to_lower "${actual}")" = "${ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS}" ] || gl_die "root sink Gateway diamond is not registered with Bridgehub"
  for value in "chain()(address):${ZKSYS_SERVICE_SETTLEMENT_CHAIN_ADDRESS}" "sequencer()(address):${ZKSYS_SERVICE_SEQUENCER}"; do
    actual="$(gl_non_l1_cast call --rpc-url "${gateway_rpc}" "${ZKSYS_SERVICE_GATEWAY_GATE}" "${value%:*}")"
    [ "$(gl_to_lower "${actual}")" = "$(gl_to_lower "${value#*:}")" ] || gl_die "Gateway gate ${value%:*} mismatch"
  done
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}" 'registryChainId()(uint256)')"
  actual="${actual%% *}"
  [ "${actual}" = "${edge_id}" ] || gl_die "root sink registry chain mismatch"
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${ZKSYS_SERVICE_ROOT_SINK}" 'gatewayChainId()(uint256)')"
  actual="${actual%% *}"
  [ "${actual}" = "${gateway_id}" ] || gl_die "root sink Gateway chain mismatch"
  for name in ZKSYS_SERVICE_ROOT_SINK; do
    actual="$(cast call --rpc-url "${L1_RPC_URL}" "${!name}" 'policyHash()(bytes32)')"
    [ "$(gl_to_lower "${actual}")" = "${ZKSYS_SERVICE_POLICY_HASH}" ] || gl_die "root sink policy mismatch"
  done
  actual="$(gl_non_l1_cast call --rpc-url "${gateway_rpc}" "${ZKSYS_SERVICE_GATEWAY_GATE}" 'policyHash()(bytes32)')"
  [ "$(gl_to_lower "${actual}")" = "${ZKSYS_SERVICE_POLICY_HASH}" ] || gl_die "Gateway gate policy mismatch"
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${root_gate}" 'productionVkHash()(bytes32)')"
  [ "$(gl_to_lower "${actual}")" = "${ZKSYS_SERVICE_GATEWAY_VK_HASH}" ] || gl_die "root gate VK mismatch"
  for value in "chain()(address):${ZKSYS_SERVICE_GATEWAY_CHAIN_ADDRESS}" "messageSink()(address):${ZKSYS_SERVICE_ROOT_SINK}" "sequencer()(address):${ZKSYS_SERVICE_SEQUENCER}"; do
    actual="$(cast call --rpc-url "${L1_RPC_URL}" "${root_gate}" "${value%:*}")"
    [ "$(gl_to_lower "${actual}")" = "$(gl_to_lower "${value#*:}")" ] || gl_die "root gate ${value%:*} mismatch"
  done
  actual="$(cast call --rpc-url "${L1_RPC_URL}" "${root_gate}" 'policyHash()(bytes32)')"
  [ "$(gl_to_lower "${actual}")" = "${ZKSYS_SERVICE_POLICY_HASH}" ] || gl_die "root gate policy mismatch"
  actual="$(gl_non_l1_cast call --rpc-url "${gateway_rpc}" "${ZKSYS_SERVICE_GATEWAY_GATE}" 'childChainId()(uint256)')"
  [ "${actual%% *}" = "${edge_id}" ] || gl_die "Gateway gate child chain mismatch"

  current="$(call_l2 "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)')"
  if [ "${current}" = 0 ]; then
    assert_l2_uint_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'totalWeight()(uint256)' 0
    local timestamp
    timestamp="$(gl_non_l1_cast block --rpc-url "${ZKSYS_L2_RPC_URL}" latest --json | python3 -c 'import json,sys; print(int(json.load(sys.stdin)["timestamp"], 0))')"
    python3 - "${timestamp}" "${ZKSYS_ISSUER_START_TIME}" "${ZKSYS_SERVICE_ROSTER_LEAD_SECONDS}" <<'PY'
import sys
now, start, lead = map(int, sys.argv[1:])
if lead <= 0 or now + lead >= start:
    raise SystemExit('service deployment requires time before period zero and roster publication')
PY
  elif [ "${current}" != 1 ]; then
    gl_die "issuer already configured for a different service period"
  else
    assert_l2_address_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceSource()(address)' "${service_address}"
  fi
  # Bind every deployment byte and cross-chain code identity before broadcasting.
  python3 - "$(gl_checkpoint_state_dir)/zksys-service-v1.json" "${GL_DIR}" "${factory_address}" "${receiver_address}" "${service_address}" "${commitment}" "${ZKSYS_SERVICE_GATEWAY_GATE_CODE_HASH}" "${ZKSYS_SERVICE_ROOT_SINK_CODE_HASH}" "${ZKSYS_SERVICE_ROOT_GATE_CODE_HASH}" <<'PY'
import json, os, stat, sys
from pathlib import Path
path = Path(sys.argv[1]); sys.path.insert(0, sys.argv[2])
from _checkpoint_state_io import atomic_write_json
payload = dict(zip(('factory', 'receiver', 'service', 'commitment', 'gateway_gate_hash', 'root_sink_hash', 'root_gate_hash'), (v.lower() for v in sys.argv[3:])))
if path.exists() or path.is_symlink():
    st = os.lstat(path)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or stat.S_IMODE(st.st_mode) & 0o077:
        raise SystemExit('unsafe service V1 manifest')
    if json.loads(path.read_text()) != payload:
        raise SystemExit('service V1 deployment identity changed')
else:
    atomic_write_json(path, payload)
PY
  factory_runtime="$(gl_non_l1_cast call --rpc-url "${ZKSYS_L2_RPC_URL}" --create "${factory_init}")"
  deploy_create2 'service V1 deployment factory' "${factory_address}" "${factory_salt}" "${factory_init}"
  assert_exact_runtime 'service V1 deployment factory' "${factory_address}" "${factory_runtime}" "$(cast keccak "${factory_runtime}")"
  actual="$(call_l2 "${factory_address}" 'deploymentCommitment()(bytes32)')"
  if [ "${actual}" = "0x$(printf '%064d' 0)" ]; then
    send_l2 "${factory_address}" 'deploy(bytes,bytes)' "${receiver_init}" "${service_init}"
  elif [ "$(gl_to_lower "${actual}")" != "$(gl_to_lower "${commitment}")" ]; then
    gl_die "service factory deployed a different configuration"
  fi
  [ "$(gl_to_lower "$(call_l2 "${factory_address}" 'deploymentCommitment()(bytes32)')")" = "$(gl_to_lower "${commitment}")" ] || gl_die "service deployment commitment mismatch"
  [ "$(rpc_code "${receiver_address}")" != 0x ] && [ "$(rpc_code "${service_address}")" != 0x ] || gl_die "service deployment missing code"
  assert_l2_address_call "${receiver_address}" 'registry()(address)' "${service_address}"
  assert_l2_address_call "${service_address}" 'acceptanceSource()(address)' "${receiver_address}"
  assert_l2_address_call "${service_address}" 'issuer()(address)' "${ZKSYS_L2_ISSUER_ADDRESS}"
  assert_l2_uint_call "${service_address}" 'firstServicePeriod()(uint64)' 0
  if [ "${current}" = 0 ]; then
    send_l2 "${ZKSYS_L2_ISSUER_ADDRESS}" 'configureServiceAccounting(address,uint256)' "${service_address}" 0
  fi
  assert_l2_address_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceSource()(address)' "${service_address}"
  assert_l2_uint_call "${ZKSYS_L2_ISSUER_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)' 1
  assert_l2_uint_call "${ZKSYS_L2_WEIGHT_REGISTRY_ADDRESS}" 'serviceStartPeriodPlusOne()(uint256)' 1
  printf 'service V1 registry=%s receiver=%s; accounting configured for period zero\n' "${service_address}" "${receiver_address}"
  echo 'Service activation still requires real bootstrap proofs, a qualified roster and both activation messages.'
}
