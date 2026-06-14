#!/usr/bin/env bash
# Deploy and verify the Pali fixed-rate zkSYS ERC-4337 paymaster on zkTanenbaum.
#
# Required:
#   ZKSYS_TOKEN_ADDRESS      zkSYS ERC-20 token address on zkTanenbaum
#
# Optional:
#   ZKTANENBAUM_RPC_URL     default: https://rpc-zk.tanenbaum.io
#   EXPLORER_BASE           default: https://explorer-zk.tanenbaum.io
#   ENTRYPOINT_ADDRESS      default: 0x433709009B8330FDa32311DF1C2AFA402eD8D009
#   PAYMASTER_TREASURY      default: deployer address, if DEPLOYER_ADDRESS is set
#   PAYMASTER_OWNER         default: deployer address, if DEPLOYER_ADDRESS is set
#   DEPLOYER_ADDRESS        used as default owner/treasury for hardware or keystore signers
#   DEPLOYER_PRIVATE_KEY    raw private key signer
#   DEPLOYER_MNEMONIC       mnemonic signer, index 0 by default
#   DEPLOYER_MNEMONIC_INDEX mnemonic index, default: 0
#   DEPLOYER_ACCOUNT        Foundry keystore account name
#   DEPLOYER_KEYSTORE       Foundry keystore path
#   DEPLOYER_PASSWORD_FILE  password file for DEPLOYER_KEYSTORE / DEPLOYER_ACCOUNT
#   DEPLOYER_SIGNER         ledger | trezor | aws | gcp
#   PAYMASTER_INITIAL_DEPOSIT_NATIVE
#                           optional native amount to send to paymaster after deployment
#                           (for example: 1000ether)
#   VERIFY                  true by default; set false to skip Blockscout verification
#
# Example:
#   ZKSYS_TOKEN_ADDRESS=0x... DEPLOYER_PRIVATE_KEY=0x... \
#     ./scripts/explorer/blockscout/deploy-pali-paymaster-zktanenbaum.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
CONTRACTS_DIR="${REPO_ROOT}/integration-tests/test-contracts"

RPC_URL="${ZKTANENBAUM_RPC_URL:-https://rpc-zk.tanenbaum.io}"
EXPLORER_BASE="${EXPLORER_BASE:-https://explorer-zk.tanenbaum.io}"
CHAIN_ID="${CHAIN_ID:-57057}"
ENTRYPOINT_ADDRESS="${ENTRYPOINT_ADDRESS:-0x433709009B8330FDa32311DF1C2AFA402eD8D009}"
VERIFY="${VERIFY:-true}"
PAYMASTER_INITIAL_DEPOSIT_NATIVE="${PAYMASTER_INITIAL_DEPOSIT_NATIVE:-}"

if [[ -z "${ZKSYS_TOKEN_ADDRESS:-}" ]]; then
  echo "error: ZKSYS_TOKEN_ADDRESS is required" >&2
  exit 1
fi

PAYMASTER_OWNER="${PAYMASTER_OWNER:-${DEPLOYER_ADDRESS:-}}"
PAYMASTER_TREASURY="${PAYMASTER_TREASURY:-${DEPLOYER_ADDRESS:-}}"
if [[ -z "${PAYMASTER_OWNER}" || -z "${PAYMASTER_TREASURY}" ]]; then
  echo "error: set PAYMASTER_OWNER and PAYMASTER_TREASURY, or set DEPLOYER_ADDRESS as their default" >&2
  exit 1
fi

wallet_args=()
case "${DEPLOYER_SIGNER:-}" in
  "")
    if [[ -n "${DEPLOYER_PRIVATE_KEY:-}" ]]; then
      wallet_args+=(--private-key "${DEPLOYER_PRIVATE_KEY}")
    elif [[ -n "${DEPLOYER_MNEMONIC:-}" ]]; then
      wallet_args+=(--mnemonic "${DEPLOYER_MNEMONIC}" --mnemonic-index "${DEPLOYER_MNEMONIC_INDEX:-0}")
    elif [[ -n "${DEPLOYER_ACCOUNT:-}" ]]; then
      wallet_args+=(--account "${DEPLOYER_ACCOUNT}")
    elif [[ -n "${DEPLOYER_KEYSTORE:-}" ]]; then
      wallet_args+=(--keystore "${DEPLOYER_KEYSTORE}")
    else
      echo "error: set DEPLOYER_PRIVATE_KEY, DEPLOYER_ACCOUNT, DEPLOYER_KEYSTORE, or DEPLOYER_SIGNER" >&2
      exit 1
    fi
    ;;
  ledger) wallet_args+=(--ledger --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for ledger}") ;;
  trezor) wallet_args+=(--trezor --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for trezor}") ;;
  aws) wallet_args+=(--aws --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for aws}") ;;
  gcp) wallet_args+=(--gcp --from "${DEPLOYER_ADDRESS:?DEPLOYER_ADDRESS is required for gcp}") ;;
  *)
    echo "error: unsupported DEPLOYER_SIGNER=${DEPLOYER_SIGNER}" >&2
    exit 1
    ;;
esac

if [[ -n "${DEPLOYER_PASSWORD_FILE:-}" ]]; then
  wallet_args+=(--password-file "${DEPLOYER_PASSWORD_FILE}")
fi

verify_args=()
if [[ "${VERIFY}" == "true" ]]; then
  verify_args+=(--verify --verifier blockscout --verifier-url "${EXPLORER_BASE%/}/api/")
fi

echo "Deploying PaliFixedRateTokenPaymaster"
echo "  rpc:       ${RPC_URL}"
echo "  chain:     ${CHAIN_ID}"
echo "  entrypoint:${ENTRYPOINT_ADDRESS}"
echo "  token:     ${ZKSYS_TOKEN_ADDRESS}"
echo "  treasury:  ${PAYMASTER_TREASURY}"
echo "  owner:     ${PAYMASTER_OWNER}"
echo

output="$(
  cd "${CONTRACTS_DIR}"
  forge create src/passkey/PaliFixedRateTokenPaymaster.sol:PaliFixedRateTokenPaymaster \
    --rpc-url "${RPC_URL}" \
    --chain "${CHAIN_ID}" \
    --broadcast \
    --optimize \
    --optimizer-runs 200 \
    "${verify_args[@]}" \
    "${wallet_args[@]}" \
    --constructor-args "${ENTRYPOINT_ADDRESS}" "${ZKSYS_TOKEN_ADDRESS}" "${PAYMASTER_TREASURY}" "${PAYMASTER_OWNER}"
)"

printf '%s\n' "${output}"

paymaster_address="$(printf '%s\n' "${output}" | sed -n 's/^Deployed to: //p' | tail -n 1)"
if [[ -n "${paymaster_address}" ]]; then
  echo
  echo "PAYMASTER_ADDRESS=${paymaster_address}"

  if [[ -n "${PAYMASTER_INITIAL_DEPOSIT_NATIVE}" ]]; then
    echo
    echo "Depositing ${PAYMASTER_INITIAL_DEPOSIT_NATIVE} native into EntryPoint for paymaster ${paymaster_address}"
    cast send "${paymaster_address}" \
      --rpc-url "${RPC_URL}" \
      --chain "${CHAIN_ID}" \
      --value "${PAYMASTER_INITIAL_DEPOSIT_NATIVE}" \
      "${wallet_args[@]}"
  fi
fi
