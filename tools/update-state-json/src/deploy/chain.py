from __future__ import annotations

import os

from inputs import DeploymentInputs
from utils import cast_calldata, forge_script, read_from_file, update_toml_key, derive_address_from_private_key

def prepare_config(inputs: DeploymentInputs) -> None:
    cfg_path = os.path.join(inputs.l1_contracts_dir, "script-config/register-zk-chain.toml")
    # anvil default
    update_toml_key(cfg_path, "create2_factory_address", "0x4e59b44847b379578588920ca78fbf26c0b4956c")

    # for some reason we put the governor as the owner in register-zk-chain.toml
    governor_address = derive_address_from_private_key(inputs.governor_key)
    update_toml_key(cfg_path, "owner_address", governor_address)

    # Update chain id
    update_toml_key(cfg_path, "chain_chain_id", inputs.chain_id)

    update_toml_key(cfg_path, "validator_sender_operator_commit_eth", inputs.validator_sender_operator_commit_eth)
    update_toml_key(cfg_path, "validator_sender_operator_prove", inputs.validator_sender_operator_prove)
    update_toml_key(cfg_path, "validator_sender_operator_execute", inputs.validator_sender_operator_execute)

    # These keys are not present in the template, but we need to set them anyway
    deploy_contracts_cfg_path = os.path.join(inputs.l1_contracts_dir, "script-config/config-deploy-l2-contracts.toml")
    update_toml_key(deploy_contracts_cfg_path, "da_validator_type", "0x0")
    update_toml_key(deploy_contracts_cfg_path, "consensus_registry_owner", "0x0000000000000000000000000000000000000001") # TODO: should be set? probably not used

    # mapping from keys (from output) to overrides for register-zk-chain.toml
    mapping = {
        "bridgehub_proxy_addr": "bridgehub_proxy_addr",
        "shared_bridge_proxy_addr": "shared_bridge_proxy_addr",
        "l1_nullifier_proxy_addr": "l1_nullifier_proxy_addr",
        "erc20_bridge_proxy_addr": "erc20_bridge_proxy_addr",
        "native_token_vault_addr": "native_token_vault_addr",
        "state_transition_proxy_addr": "chain_type_manager_proxy_addr",
        "diamond_cut_data": "diamond_cut_data",
        "force_deployments_data": "force_deployments_data",
        "validator_timelock_addr": "validator_timelock_addr",
        "server_notifier_proxy_addr": "server_notifier_proxy_addr"
    }
    for key, toml_key in mapping.items():
        value = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-deploy-l1.toml"), key)
        if value:
            update_toml_key(cfg_path, toml_key, value)

def bootstrap_chain(inputs: DeploymentInputs) -> None:
    prepare_config(inputs)

    forge_script(
        "deploy-scripts/RegisterZKChain.s.sol",
        f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key}",
        inputs,
        "Register ZK chain on Bridgehub",
    )

    bridgehub_proxy = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-deploy-l1.toml"), "bridgehub_proxy_addr")
    diamond_proxy = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-register-zk-chain.toml"), "diamond_proxy_addr")
    chain_admin_addr = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-register-zk-chain.toml"), "chain_admin_addr")

    calldata = cast_calldata("chainAdminAcceptAdmin(address,address)", chain_admin_addr, diamond_proxy)
    forge_script(
        "deploy-scripts/AdminFunctions.s.sol",
        f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key} --sig={calldata}",
        inputs,
        "Accept diamond admin role",
    )

    if inputs.enable_token_multiplier_update:
        if not inputs.token_multiplier_setter:
            raise RuntimeError("Token multiplier setter address required when ENABLE_TOKEN_MULTIPLIER_SETTER is true")
        if not inputs.access_control_restriction:
            raise RuntimeError("Access control restriction address required when ENABLE_TOKEN_MULTIPLIER_SETTER is true")
        calldata = cast_calldata(
            "chainSetTokenMultiplierSetter(address,address,address,address)",
            chain_admin_addr,
            inputs.access_control_restriction,
            inputs.diamond_proxy,
            inputs.token_multiplier_setter,
        )
        forge_script(
            "deploy-scripts/AdminFunctions.s.sol",
            f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key} --sig={calldata}",
            inputs,
            "Update token multiplier setter",
        )
    
    # mapping from keys (from output) to overrides for config-deploy-l2-contracts.toml
    mapping = {
        "bridgehub_proxy_addr": "bridgehub",
        "shared_bridge_proxy_addr": "l1_shared_bridge",
        "era_chain_id": "era_chain_id",
        "governance_addr": "governance",
        "erc20_bridge_proxy_addr": "erc20_bridge",
    }
    update_toml_key(os.path.join(inputs.l1_contracts_dir, "script-config/config-deploy-l2-contracts.toml"), "chain_id", inputs.chain_id)
    for key, toml_key in mapping.items():
        value = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-deploy-l1.toml"), key)
        if value:
            update_toml_key(os.path.join(inputs.l1_contracts_dir, "script-config/config-deploy-l2-contracts.toml"), toml_key, value)

    forge_script(
        "deploy-scripts/DeployL2Contracts.sol",
        f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key}",
        inputs,
        "Deploy L2 contracts bundle",
    )

    rollup_l1_da_validator_addr = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-deploy-l1.toml"), "rollup_l1_da_validator_addr")
    l2_da_validator_addr = read_from_file(os.path.join(inputs.l1_contracts_dir, "script-out/output-deploy-l2-contracts.toml"), "l2_da_validator_address")

    calldata = cast_calldata(
        "setDAValidatorPair(address,uint256,address,address,bool)",
        bridgehub_proxy,
        str(inputs.chain_id),
        rollup_l1_da_validator_addr,  # TODO: I guess we also want to support other types of DA
        l2_da_validator_addr,
        "true",
    )
    forge_script(
        "deploy-scripts/AdminFunctions.s.sol",
        f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key} --sig={calldata}",
        inputs,
        "Set DA validator pair",
    )

    # if inputs.run_make_permanent_rollup:
    #     calldata = cast_calldata(
    #         "makePermanentRollup(address,address)",
    #         inputs.chain_admin_addr,
    #         inputs.diamond_proxy,
    #     )
    #     forge_script(
    #         "deploy-scripts/AdminFunctions.s.sol",
    #         f"--ffi --rpc-url={inputs.l1_rpc_url} --broadcast --private-key={inputs.governor_key} --sig={calldata}",
    #         inputs,
    #         "Make rollup permanent",
    #     )
