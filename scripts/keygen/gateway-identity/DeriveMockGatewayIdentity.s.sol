// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {VmSafe} from "forge-std/Vm.sol";
import {GatewayVotePreparation} from "deploy-scripts/gateway/GatewayVotePreparation.s.sol";
import {
    GatewayCTMDeployerHelper,
    DeployerAddresses,
    DeployerCreate2Calldata,
    DirectCreate2Calldata
} from "deploy-scripts/gateway/GatewayCTMDeployerHelper.sol";
import {DeployedContracts} from "contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployer.sol";

/// @notice SYSCOIN: RPC-backed, read-only mock-testnet graph calculation.
/// @dev This is separate from the real-verifier offline attestation harness.
/// Uses the canonical initializer and complete calculateAddresses helper; no
/// broadcast/deploy entry point is called and no deployment artifact is written.
contract DeriveMockGatewayIdentity is GatewayVotePreparation {
    string private constant OBJECT = "syscoin-mock-gateway-identity";

    function inspect(string calldata inputPath, string calldata outputPath) external {
        require(vm.isContext(VmSafe.ForgeContext.ScriptDryRun), "dry-run only");
        require(block.chainid == 5700, "Tanenbaum root required");
        require(vm.envOr("SYSCOIN_ZKSYNC_OS_MOCK_VERIFIER", false), "explicit mock mode required");
        require(vm.envOr("GW_IS_EVM_EQUIVALENT", true), "EVM CREATE2 required");
        require(!vm.exists(outputPath), "refusing to overwrite evidence");

        string memory input = vm.readFile(inputPath);
        require(
            keccak256(bytes(vm.parseJsonString(input, ".schema"))) ==
                keccak256("syscoin-v32-mock-gateway-inspection-input-v1"),
            "wrong inspection schema"
        );
        address bridgehub = vm.parseJsonAddress(input, ".bridgehub");
        address governance = vm.parseJsonAddress(input, ".expected_governance");
        require(bridgehub != address(0) && governance != address(0), "missing root addresses");
        initializeConfig(
            vm.parseJsonString(input, ".preparation_config_path"),
            bridgehub,
            vm.parseJsonUint(input, ".representative_chain_id")
        );
        require(config.isZKsyncOS && config.testnetVerifier, "mock zkOS config required");
        require(config.l1ChainId == 5700 && gatewayCTMDeployerConfig.l1ChainId == 5700, "wrong root chain");
        require(config.ownerAddress == governance, "root governance mismatch");
        require(gatewayCTMDeployerConfig.salt == vm.parseJsonBytes32(input, ".expected_salt"), "salt mismatch");
        require(gatewayCTMDeployerConfig.eraChainId == vm.parseJsonUint(input, ".expected_era_chain_id"), "Era ID mismatch");
        require(gatewayCTMDeployerConfig.protocolVersion == (uint256(32) << 32), "V32 required");
        require(gatewayCTMDeployerConfig.forceDeploymentsData.length != 0, "missing force deployments");

        (
            DeployedContracts memory contracts,
            DeployerCreate2Calldata memory payloads,
            DeployerAddresses memory deployers,
            DirectCreate2Calldata memory directPayloads,
            address factory
        ) = GatewayCTMDeployerHelper.calculateAddresses(gatewayCTMDeployerConfig.salt, gatewayCTMDeployerConfig);

        require(factory == vm.parseJsonAddress(input, ".expected_factory"), "factory mismatch");
        require(contracts.stateTransition.proxies.validatorTimelock == vm.parseJsonAddress(input, ".expected_validator_timelock"), "guest DA target mismatch");
        require(contracts.daContracts.rollupSLDAValidator == vm.parseJsonAddress(input, ".expected_relay"), "guest relay mismatch");
        require(contracts.stateTransition.chainTypeManagerProxyAdmin == vm.parseJsonAddress(input, ".expected_proxy_admin"), "proxy admin mismatch");
        require(deployers.proxyAdminDeployer == vm.parseJsonAddress(input, ".expected_proxy_admin_deployer"), "proxy admin deployer mismatch");
        require(deployers.validatorTimelockDeployer == vm.parseJsonAddress(input, ".expected_validator_timelock_deployer"), "timelock deployer mismatch");
        require(contracts.stateTransition.implementations.validatorTimelock == vm.parseJsonAddress(input, ".expected_validator_timelock_implementation"), "timelock implementation mismatch");

        vm.serializeString(OBJECT, "schema", "syscoin-v32-mock-gateway-inspection-output-v1");
        vm.serializeString(OBJECT, "scope", "full-ctm-rpc-backed-mock-dry-run");
        vm.serializeBool(OBJECT, "broadcast", false);
        vm.serializeBool(OBJECT, "full_ctm_calculate_addresses_executed", true);
        vm.serializeBool(OBJECT, "production_proof_attestation", false);
        vm.serializeBool(OBJECT, "independent_host_reproduction", false);
        vm.serializeUint(OBJECT, "root_block", block.number);
        vm.serializeAddress(OBJECT, "bridgehub", bridgehub);
        vm.serializeAddress(OBJECT, "governance", governance);
        vm.serializeBytes32(OBJECT, "input_keccak256", keccak256(bytes(input)));
        vm.serializeBytes(OBJECT, "complete_config_abi", abi.encode(gatewayCTMDeployerConfig));
        vm.serializeBytes(OBJECT, "complete_contracts_abi", abi.encode(contracts));
        vm.serializeBytes(OBJECT, "complete_deployers_abi", abi.encode(deployers));
        vm.serializeBytes32(OBJECT, "deployment_payloads_keccak256", keccak256(abi.encode(payloads, directPayloads)));
        vm.serializeAddress(OBJECT, "factory", factory);
        vm.serializeAddress(OBJECT, "validator_timelock", contracts.stateTransition.proxies.validatorTimelock);
        vm.serializeAddress(OBJECT, "relay", contracts.daContracts.rollupSLDAValidator);
        vm.serializeAddress(OBJECT, "proxy_admin", contracts.stateTransition.chainTypeManagerProxyAdmin);
        vm.serializeAddress(OBJECT, "proxy_admin_deployer", deployers.proxyAdminDeployer);
        vm.serializeAddress(OBJECT, "validator_timelock_deployer", deployers.validatorTimelockDeployer);
        vm.serializeAddress(OBJECT, "validator_timelock_implementation", contracts.stateTransition.implementations.validatorTimelock);
        vm.serializeAddress(OBJECT, "mock_verifier", contracts.stateTransition.verifiers.verifier);
        string memory output = vm.serializeAddress(OBJECT, "ctm", contracts.stateTransition.proxies.chainTypeManager);
        vm.writeJson(output, outputPath);
    }
}
