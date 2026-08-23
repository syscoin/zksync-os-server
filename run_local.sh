#!/usr/bin/env bash

set -e

# Get the directory where this script is located
REPO_ROOT="$(dirname "$(realpath "$0")")"

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${YELLOW}⚠️  This script is a temporary solution. Do not depend on it in production.${NC}"
echo ""

# Array to store PIDs of background processes
declare -a PIDS=()

TEMP_DIR=$(mktemp -d)

# Check if tmp dir was created
if [[ ! "$TEMP_DIR" || ! -d "$TEMP_DIR" ]]; then
  echo "Could not create temporary directory via 'mkdir'"
  exit 1
fi

# Cleanup function to stop all started services
cleanup() {
    local exit_status=$?

    # Prevent re-entry when exit triggers the trap again
    trap - SIGINT SIGTERM EXIT

    echo -e "\n${YELLOW}Shutting down all services...${NC}"
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            echo -e "${YELLOW}Stopping process $pid${NC}"
            kill -TERM "$pid" 2>/dev/null || true
        fi
    done

    # Wait for processes to terminate gracefully
    sleep 2

    # Force kill any remaining processes
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            echo -e "${RED}Force killing process $pid${NC}"
            kill -9 "$pid" 2>/dev/null || true
        fi
    done

    echo -e "${GREEN}All services stopped${NC}"
    rm -rf "$TEMP_DIR"
    exit "$exit_status"
}

# Preserve the conventional signal exit status, then let the EXIT trap clean up.
trap 'exit 130' SIGINT
trap 'exit 143' SIGTERM
trap cleanup EXIT

# Parse command line arguments
CONFIG_DIR=""
LOGS_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --logs-dir)
            if [ -z "$2" ] || [[ "$2" == --* ]]; then
                echo -e "${RED}Error: --logs-dir requires a path argument${NC}"
                exit 1
            fi
            LOGS_DIR="$2"
            shift 2
            ;;
        -*)
            echo -e "${RED}Error: Unknown option $1${NC}"
            echo -e "Usage: $0 <folder-path> [--logs-dir <path>]"
            exit 1
            ;;
        *)
            if [ -z "$CONFIG_DIR" ]; then
                CONFIG_DIR="$1"
            else
                echo -e "${RED}Error: Unexpected argument $1${NC}"
                exit 1
            fi
            shift
            ;;
    esac
done

# Check if folder path is provided
if [ -z "$CONFIG_DIR" ]; then
    echo -e "${RED}Usage: $0 <folder-path> [--logs-dir <path>]${NC}"
    echo -e "Canonical v32.0/V8 examples are unavailable until the local-chain fixture is regenerated."
    exit 1
fi

# Resolve to absolute path
CONFIG_DIR="$(realpath "$CONFIG_DIR")"

# Verify the directory exists
if [ ! -d "$CONFIG_DIR" ]; then
    echo -e "${RED}Error: Directory '$CONFIG_DIR' does not exist${NC}"
    exit 1
fi

# SYSCOIN: Never launch a stale fixture under the canonical V32/V8 identity.
FIXTURE_PENDING_MARKER="$CONFIG_DIR/../CANONICAL_V8_REGENERATION_REQUIRED"
if [ -f "$FIXTURE_PENDING_MARKER" ]; then
    echo -e "${RED}Error: this local-chain fixture is blocked pending canonical v32.0/V8 regeneration.${NC}" >&2
    echo -e "${RED}See: $FIXTURE_PENDING_MARKER${NC}" >&2
    exit 1
fi

PATCHED_OS_RUNNER="$REPO_ROOT/scripts/gateway-launch/run-os-server-with-patched-zksync-os.sh"
PATCHED_OS_WORKSPACE="run-local"
export GATEWAY_DIR="${ZKSYNC_OS_LOCAL_BUILD_ROOT:-$REPO_ROOT/target/run-local-patched-os}"
export ZKSYNC_OS_SERVER_PATH="$REPO_ROOT"
export PROTOCOL_VERSION=run-local
# SYSCOIN: These values are part of the canonical guest. Local native execution
# must use them exactly; otherwise sequencing and proof execution would disagree.
export SYSCOIN_EDGE_DA_COMMIT_TARGET=0x64ef2f0c4168eb76fe95993f2a7c7b35dcf3fe19
export SYSCOIN_GAS_TANK_ADDRESS=0xb9feff70ec42b6b5af5a690b4dbc332a2d1f3beb
# Every server binary uses the locally patched final zksync-os v0.4.0 source.
export ZKSYNC_OS_FORCE_PATCHED_WORKSPACE=true

# Check for compressed L1 state file
L1_STATE_FILE_GZ="$CONFIG_DIR/../l1-state.json.gz"
if [ ! -f "$L1_STATE_FILE_GZ" ]; then
    echo -e "${RED}Error: L1 state file '$L1_STATE_FILE_GZ' not found${NC}"
    exit 1
fi

# Decompress L1 state file into temporary directory
gzip -d < "$L1_STATE_FILE_GZ" > "$TEMP_DIR/l1-state.json"

# Check for L1 state file
L1_STATE_FILE="$TEMP_DIR/l1-state.json"
if [ ! -f "$L1_STATE_FILE" ]; then
    echo -e "${RED}Error: decompressed L1 state file '$L1_STATE_FILE' not found${NC}"
    exit 1
fi

# Generate timestamp for log files (same timestamp for all logs in this session)
LOG_TIMESTAMP=$(date +"%Y-%m-%dT%H-%M-%S")

# Setup logs directory if specified
if [ -n "$LOGS_DIR" ]; then
    mkdir -p "$LOGS_DIR"
    LOGS_DIR="$(realpath "$LOGS_DIR")"
    echo -e "${BLUE}Logs will be written to: $LOGS_DIR${NC}"
fi

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}Starting Local Development Environment${NC}"
echo -e "${BLUE}Config directory: $CONFIG_DIR${NC}"
echo -e "${BLUE}========================================${NC}"

# Build first
echo -e "\n${GREEN}Building zksync-os-server...${NC}"
# Native execution must use the same patched final-v0.4 source as the proving app.
# The launcher anchors the official tag to Cargo.lock, applies the local patch,
# rewrites a disposable server workspace, and publishes a context-stamped binary.
if ! bash "$PATCHED_OS_RUNNER" "$PATCHED_OS_WORKSPACE" -- build-prebuilt; then
    echo -e "${RED}Build failed${NC}"
    exit 1
fi
echo -e "${GREEN}Build completed${NC}"

run_chain() {
    local config_file="$1"
    bash "$PATCHED_OS_RUNNER" "$PATCHED_OS_WORKSPACE" -- exec-prebuilt -- \
        --config "$REPO_ROOT/local-chains/local_dev.yaml" --config "$config_file"
}

# Start Anvil
echo -e "\n${GREEN}Starting Anvil...${NC}"
if [ -n "$LOGS_DIR" ]; then
    ANVIL_LOG_FILE="$LOGS_DIR/anvil-$LOG_TIMESTAMP.log"
    anvil --load-state "$L1_STATE_FILE" --port 8545 --block-time 0.25 --mixed-mining --slots-in-an-epoch 10 > "$ANVIL_LOG_FILE" 2>&1 &
    echo -e "${GREEN}Anvil logs: $ANVIL_LOG_FILE${NC}"
else
    anvil --load-state "$L1_STATE_FILE" --port 8545 --block-time 0.25 --mixed-mining --slots-in-an-epoch 10 > /dev/null 2>&1 &
fi
ANVIL_PID=$!
PIDS+=($ANVIL_PID)
echo -e "${GREEN}Anvil started with PID $ANVIL_PID${NC}"

# Wait for Anvil to be ready
echo -e "${YELLOW}Waiting for Anvil to be ready...${NC}"
for i in {1..30}; do
    if curl -s http://localhost:8545 -X POST -H "Content-Type: application/json" \
        --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' > /dev/null 2>&1; then
        echo -e "${GREEN}Anvil is ready${NC}"
        break
    fi
    if [ $i -eq 30 ]; then
        echo -e "${RED}Anvil failed to start${NC}"
        exit 1
    fi
    sleep 1
done

# Determine which chain configs to use
SINGLE_CONFIG="$CONFIG_DIR/config.yaml"

if [ -f "$SINGLE_CONFIG" ]; then
    # Single chain mode

    # Prompt to clean up db folder (only for single chain mode)
    if [ -d "$REPO_ROOT/db" ] && [ "$(ls -A "$REPO_ROOT/db" 2>/dev/null)" ]; then
        echo -e "${YELLOW}The db/ folder contains existing data.${NC}"
        read -p "Do you want to clean it up? (y/N): " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            echo -e "${YELLOW}Cleaning up db/* ...${NC}"
            rm -rf "$REPO_ROOT/db"/*
            echo -e "${GREEN}db/ folder cleaned${NC}"
        fi
    fi

    echo -e "\n${GREEN}Starting single chain with config: $SINGLE_CONFIG${NC}"
    if [ -n "$LOGS_DIR" ]; then
        CHAIN_LOG_FILE="$LOGS_DIR/chain-$LOG_TIMESTAMP.log"
        run_chain "$SINGLE_CONFIG" > "$CHAIN_LOG_FILE" 2>&1 &
        echo -e "${GREEN}Chain logs: $CHAIN_LOG_FILE${NC}"
    else
        run_chain "$SINGLE_CONFIG" &
    fi
    CHAIN_PID=$!
    PIDS+=($CHAIN_PID)
    echo -e "${GREEN}Chain started with PID $CHAIN_PID${NC}"
else
    # Multiple chains mode - look for chain_<chainid>.yaml files
    CHAIN_CONFIGS=($(ls "$CONFIG_DIR"/chain_*.yaml 2>/dev/null | sort -V))

    if [ ${#CHAIN_CONFIGS[@]} -eq 0 ]; then
        echo -e "${RED}Error: No config.yaml or chain_*.yaml files found in '$CONFIG_DIR'${NC}"
        exit 1
    fi

    echo -e "\n${GREEN}Starting ${#CHAIN_CONFIGS[@]} chain(s)...${NC}"

    for config_file in "${CHAIN_CONFIGS[@]}"; do
        echo -e "${GREEN}Starting chain with config: $config_file${NC}"
        if [ -n "$LOGS_DIR" ]; then
            # Extract config file name without extension for log file naming
            CONFIG_NAME=$(basename "$config_file" .yaml)
            CHAIN_LOG_FILE="$LOGS_DIR/${CONFIG_NAME}-$LOG_TIMESTAMP.log"
            run_chain "$config_file" > "$CHAIN_LOG_FILE" 2>&1 &
            echo -e "${GREEN}Chain logs: $CHAIN_LOG_FILE${NC}"
        else
            run_chain "$config_file" &
        fi
        CHAIN_PID=$!
        PIDS+=($CHAIN_PID)
        echo -e "${GREEN}Chain started with PID $CHAIN_PID${NC}"

        # Small delay between starting chains (to make sure file locks are awaited properly)
        sleep 2
    done
fi

echo -e "\n${BLUE}========================================${NC}"
echo -e "${BLUE}All services started successfully${NC}"
echo -e "${BLUE}Press Ctrl+C to stop all services${NC}"
echo -e "${BLUE}========================================${NC}"

# Wait for all background processes
wait
