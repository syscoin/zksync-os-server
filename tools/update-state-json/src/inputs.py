from __future__ import annotations

from typing import Optional, Dict
import os
from pathlib import Path
import json
from dataclasses import dataclass


@dataclass
class DeploymentInputs:
    # Mandatory configs
    l1_rpc_url: str
    deployer_key: str
    governor_key: str
    validator_sender_operator_commit_eth: str
    validator_sender_operator_prove: str
    validator_sender_operator_execute: str
    chain_id: int

    # Optional configs
    enable_token_multiplier_update: bool = False
    run_make_permanent_rollup: bool = False
    reuse_ctm_governance: bool = True  # matches CLI default behaviour

    # To be filled from within app
    l1_contracts_dir: Optional[str] = None
    genesis_commitment: Optional[str] = None

    @classmethod
    def from_env(cls) -> "DeploymentInputs":
        def require(name: str) -> str:
            value = os.getenv(name)
            if not value:
                raise RuntimeError(f"Missing required environment variable: {name}")
            return value.strip()

        def parse_bool(name: str, default: bool = False) -> bool:
            value = os.getenv(name)
            if value is None:
                return default
            return value.strip().lower() in {"1", "true", "yes", "on"}

        return cls(
            l1_contracts_dir=None,
            genesis_commitment=None,
            l1_rpc_url=require("L1_RPC_URL"),
            deployer_key=require("DEPLOYER_PRIVATE_KEY"),
            governor_key=require("GOVERNOR_PRIVATE_KEY"),
            validator_sender_operator_commit_eth=require("VALIDATOR_SENDER_OPERATOR_COMMIT_ETH"),
            validator_sender_operator_prove=require("VALIDATOR_SENDER_OPERATOR_PROVE"),
            validator_sender_operator_execute=require("VALIDATOR_SENDER_OPERATOR_EXECUTE"),
            chain_id=int(require("CHAIN_ID"), 0),
            enable_token_multiplier_update=parse_bool("ENABLE_TOKEN_MULTIPLIER_SETTER", False),
            run_make_permanent_rollup=parse_bool("RUN_MAKE_PERMANENT_ROLLUP", False),
            reuse_ctm_governance=parse_bool("REUSE_CTM_GOVERNANCE", True),
        )

    def base_env(self) -> dict:
        env = os.environ.copy()
        return env

def initial_contracts(inputs: DeploymentInputs) -> Dict[str, str]:
    wrapped_base_token_path = Path(f"{inputs.l1_contracts_dir}/out/L2WrappedBaseToken.sol/L2WrappedBaseToken.json")
    genesis_upgrade_path = Path(f"{inputs.l1_contracts_dir}/out/L2GenesisUpgrade.sol/L2GenesisUpgrade.json")
    complex_upgrader_path = Path(f"{inputs.l1_contracts_dir}/out/L2ComplexUpgrader.sol/L2ComplexUpgrader.json")
    
    wrapped_base_token = json.loads(wrapped_base_token_path.read_text())
    genesis_upgrade = json.loads(genesis_upgrade_path.read_text())
    complex_upgrader = json.loads(complex_upgrader_path.read_text())

    initial_contracts = {
        "0x000000000000000000000000000000000000800f": complex_upgrader["deployedBytecode"]["object"],
        "0x0000000000000000000000000000000000010001": genesis_upgrade["deployedBytecode"]["object"],
        "0x0000000000000000000000000000000000010007": wrapped_base_token["deployedBytecode"]["object"],
    }

    return initial_contracts

def additional_storage(inputs: DeploymentInputs) -> Dict[str, str]:
    return {}
