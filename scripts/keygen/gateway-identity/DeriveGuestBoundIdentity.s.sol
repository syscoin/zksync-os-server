// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {AddressAliasHelper} from "contracts/vendor/AddressAliasHelper.sol";
import {
    GatewayCTMDeployerHelper,
    DeployerAddresses,
    DeployerCreate2Calldata
} from "deploy-scripts/gateway/GatewayCTMDeployerHelper.sol";
import {
    GatewayCTMDeployerConfig,
    DeployedContracts,
    GatewayProxyAdminDeployerResult,
    GatewayValidatorTimelockDeployerResult
} from "contracts/state-transition/chain-deps/gateway-ctm-deployer/GatewayCTMDeployer.sol";
import {
    SYSCOIN_EDGE_DA_RELAY_FACTORY,
    SYSCOIN_EDGE_DA_RELAY_SALT,
    SYSCOIN_EDGE_DA_RELAY_ADDRESS,
    SYSCOIN_EDGE_DA_RELAY_INIT_CODE_HASH,
    SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH
} from "contracts/common/SyscoinEdgeDARelayDeployment.sol";

/// @notice Task-local calculation only: no broadcast, fork, signer, or deployment calls.
/// @dev Full explicit config is required so provisional downstream values are never invented.
contract DeriveGuestBoundIdentity is Script {
    string private constant OBJECT = "syscoin-offline-identity";
    struct Computed {
        DeployedContracts contracts;
        DeployerCreate2Calldata payloads;
        DeployerAddresses deployers;
    }

    function run(string calldata inputPath, string calldata outputPath) external {
        require(vm.isContext(VmSafe.ForgeContext.ScriptDryRun), "offline dry-run only");
        require(!vm.exists(outputPath), "refusing to overwrite existing derivation");
        string memory input = vm.readFile(inputPath);
        require(
            keccak256(bytes(vm.parseJsonString(input, ".schema"))) ==
                keccak256("syscoin-v32-offline-identity-input-v1"),
            "wrong input schema"
        );
        require(!vm.parseJsonBool(input, ".deployed"), "input must be an offline plan");
        require(vm.envOr("GW_IS_EVM_EQUIVALENT", true), "EVM CREATE2 required");
        address governance = vm.parseJsonAddress(input, ".l1_governance");
        require(governance != address(0), "governance is missing");
        require(
            vm.parseJsonAddress(input, ".factory") == SYSCOIN_EDGE_DA_RELAY_FACTORY,
            "factory differs from reviewed source"
        );
        require(
            vm.parseJsonBytes32(input, ".relay_salt") == SYSCOIN_EDGE_DA_RELAY_SALT,
            "relay salt differs from reviewed source"
        );
        GatewayCTMDeployerConfig memory config = _config(input);
        address aliasedGovernance = AddressAliasHelper.applyL1ToL2Alias(governance);
        require(config.aliasedGovernanceAddress == aliasedGovernance, "wrong governance alias");
        bytes32 outerSalt = vm.parseJsonBytes32(input, ".outer_salt");
        _checkGovernance(input, governance, outerSalt);
        // The actual GatewayVotePreparation entry point supplies the same salt twice.
        require(outerSalt == config.salt, "outer/inner salt differs from canonical caller");
        Computed memory result;
        if (_isCritical(input)) {
            // Only these exact helper functions consume this deliberately partial config.
            // Unspecified full-CTM fields are never passed to calculateAddresses.
            (result.contracts, result.payloads, result.deployers) = _critical(outerSalt, config);
        } else {
            address factory;
            (result.contracts, result.payloads, result.deployers, , factory) = GatewayCTMDeployerHelper.calculateAddresses(outerSalt, config);
            require(factory == SYSCOIN_EDGE_DA_RELAY_FACTORY, "helper factory mismatch");
            require(result.contracts.daContracts.rollupSLDAValidator == SYSCOIN_EDGE_DA_RELAY_ADDRESS, "relay mismatch");
        }
        _export(input, config, result, outputPath);
    }

    function _checkGovernance(string memory _input, address _governance, bytes32 _salt) private view {
        string memory artifact = vm.readFile(string.concat(vm.projectRoot(), "/out/Governance.sol/Governance.json"));
        bytes memory bytecode = vm.parseJsonBytes(artifact, ".bytecode.object");
        bytes memory constructorArgs = abi.encode(
            vm.parseJsonAddress(_input, ".root_governance_constructor.admin"),
            vm.parseJsonAddress(_input, ".root_governance_constructor.security_council"),
            vm.parseJsonUint(_input, ".root_governance_constructor.min_delay_seconds")
        );
        address expected = address(uint160(uint256(keccak256(abi.encodePacked(
            bytes1(0xff), SYSCOIN_EDGE_DA_RELAY_FACTORY, _salt, keccak256(bytes.concat(bytecode, constructorArgs))
        )))));
        require(_governance == expected, "root governance derivation mismatch");
    }

    function _isCritical(string memory _input) private pure returns (bool) {
        bytes32 scope = keccak256(bytes(vm.parseJsonString(_input, ".scope")));
        require(scope == keccak256("critical-subtree") || scope == keccak256("full-ctm"), "unknown calculation scope");
        return scope == keccak256("critical-subtree");
    }

    function _critical(bytes32 _salt, GatewayCTMDeployerConfig memory _config)
        private returns (DeployedContracts memory contracts, DeployerCreate2Calldata memory payloads, DeployerAddresses memory deployers)
    {
        GatewayProxyAdminDeployerResult memory proxyAdmin;
        GatewayValidatorTimelockDeployerResult memory timelock;
        (deployers.proxyAdminDeployer, payloads.proxyAdminCalldata, proxyAdmin) =
            GatewayCTMDeployerHelper._calculateProxyAdminDeployer(_salt, _config);
        (deployers.validatorTimelockDeployer, payloads.validatorTimelockCalldata, timelock) =
            GatewayCTMDeployerHelper._calculateValidatorTimelockDeployer(_salt, _config, proxyAdmin);
        contracts.stateTransition.chainTypeManagerProxyAdmin = proxyAdmin.chainTypeManagerProxyAdmin;
        contracts.stateTransition.implementations.validatorTimelock = timelock.validatorTimelockImplementation;
        contracts.stateTransition.proxies.validatorTimelock = timelock.validatorTimelockProxy;
        // This check is repeated independently from the Prague artifact by check_identity.py.
        string memory relayArtifact = vm.readFile(vm.envString("SYSCOIN_EDGE_DA_RELAY_ARTIFACT"));
        bytes32 initHash = keccak256(vm.parseJsonBytes(relayArtifact, ".bytecode.object"));
        require(initHash == SYSCOIN_EDGE_DA_RELAY_INIT_CODE_HASH, "relay init-code drift");
        require(keccak256(vm.parseJsonBytes(relayArtifact, ".deployedBytecode.object")) == SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH, "relay runtime drift");
        require(address(uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), SYSCOIN_EDGE_DA_RELAY_FACTORY, SYSCOIN_EDGE_DA_RELAY_SALT, initHash))))) == SYSCOIN_EDGE_DA_RELAY_ADDRESS, "relay address drift");
    }

    function _config(string memory input) private pure returns (GatewayCTMDeployerConfig memory config) {
        config.aliasedGovernanceAddress = vm.parseJsonAddress(input, ".config.aliasedGovernanceAddress");
        config.salt = vm.parseJsonBytes32(input, ".config.salt");
        config.testnetVerifier = vm.parseJsonBool(input, ".config.testnetVerifier");
        config.isZKsyncOS = vm.parseJsonBool(input, ".config.isZKsyncOS");
        require(config.isZKsyncOS && !config.testnetVerifier, "real zkOS verifier required");
        if (_isCritical(input)) return config;
        config.eraChainId = vm.parseJsonUint(input, ".config.eraChainId");
        config.l1ChainId = vm.parseJsonUint(input, ".config.l1ChainId");
        config.testnetVerifier = vm.parseJsonBool(input, ".config.testnetVerifier");
        config.isZKsyncOS = vm.parseJsonBool(input, ".config.isZKsyncOS");
        require(config.eraChainId != 0 && config.l1ChainId != 0, "chain IDs must be explicit");
        require(config.isZKsyncOS && !config.testnetVerifier, "real zkOS verifier required");
        config.adminSelectors = _selectors(input, ".config.adminSelectors");
        config.executorSelectors = _selectors(input, ".config.executorSelectors");
        config.mailboxSelectors = _selectors(input, ".config.mailboxSelectors");
        config.gettersSelectors = _selectors(input, ".config.gettersSelectors");
        config.migratorSelectors = _selectors(input, ".config.migratorSelectors");
        config.committerSelectors = _selectors(input, ".config.committerSelectors");
        config.bootloaderHash = vm.parseJsonBytes32(input, ".config.bootloaderHash");
        config.defaultAccountHash = vm.parseJsonBytes32(input, ".config.defaultAccountHash");
        config.evmEmulatorHash = vm.parseJsonBytes32(input, ".config.evmEmulatorHash");
        config.genesisRoot = vm.parseJsonBytes32(input, ".config.genesisRoot");
        config.genesisRollupLeafIndex = vm.parseJsonUint(input, ".config.genesisRollupLeafIndex");
        config.genesisBatchCommitment = vm.parseJsonBytes32(input, ".config.genesisBatchCommitment");
        config.forceDeploymentsData = vm.parseJsonBytes(input, ".config.forceDeploymentsData");
        config.protocolVersion = vm.parseJsonUint(input, ".config.protocolVersion");
        require(config.protocolVersion == (uint256(32) << 32), "protocol must be 0.32.0");
    }

    function _selectors(string memory input, string memory key) private pure returns (bytes4[] memory result) {
        bytes[] memory raw = vm.parseJsonBytesArray(input, key);
        require(raw.length != 0, "facet selectors must be explicit");
        result = new bytes4[](raw.length);
        for (uint256 i; i < raw.length; ++i) {
            require(raw[i].length == 4, "selector must contain four bytes");
            result[i] = bytes4(raw[i]);
        }
    }

    function _export(
        string memory input,
        GatewayCTMDeployerConfig memory config,
        Computed memory result,
        string memory outputPath
    ) private {
        vm.serializeString(OBJECT, "schema", "syscoin-v32-solidity-identity-output-v1");
        vm.serializeString(OBJECT, "status", "offline-derived-not-deployed");
        vm.serializeString(OBJECT, "scope", vm.parseJsonString(input, ".scope"));
        vm.serializeBool(OBJECT, "deployed", false);
        vm.serializeBool(OBJECT, "independent_host_reproduction", false);
        vm.serializeBytes32(OBJECT, "input_keccak256", keccak256(bytes(input)));
        vm.serializeAddress(OBJECT, "l1_governance", vm.parseJsonAddress(input, ".l1_governance"));
        vm.serializeAddress(OBJECT, "aliased_governance", config.aliasedGovernanceAddress);
        vm.serializeAddress(OBJECT, "factory", SYSCOIN_EDGE_DA_RELAY_FACTORY);
        vm.serializeBytes32(OBJECT, "outer_salt", config.salt);
        vm.serializeBytes32(OBJECT, "inner_salt", config.salt);
        vm.serializeAddress(OBJECT, "proxy_admin_deployer", result.deployers.proxyAdminDeployer);
        vm.serializeAddress(OBJECT, "proxy_admin", result.contracts.stateTransition.chainTypeManagerProxyAdmin);
        vm.serializeAddress(OBJECT, "validator_timelock_deployer", result.deployers.validatorTimelockDeployer);
        vm.serializeAddress(OBJECT, "validator_timelock_implementation", result.contracts.stateTransition.implementations.validatorTimelock);
        vm.serializeAddress(OBJECT, "validator_timelock", result.contracts.stateTransition.proxies.validatorTimelock);
        vm.serializeAddress(OBJECT, "relay", SYSCOIN_EDGE_DA_RELAY_ADDRESS);
        vm.serializeBytes32(OBJECT, "relay_salt", SYSCOIN_EDGE_DA_RELAY_SALT);
        vm.serializeBytes32(OBJECT, "relay_init_code_hash", SYSCOIN_EDGE_DA_RELAY_INIT_CODE_HASH);
        vm.serializeBytes32(OBJECT, "relay_runtime_hash", SYSCOIN_EDGE_DA_RELAY_RUNTIME_HASH);
        // These values change after VK generation and are excluded from guest-bound identity.
        if (!_isCritical(input)) {
            vm.serializeAddress(OBJECT, "provisional_verifier", result.contracts.stateTransition.verifiers.verifier);
            vm.serializeAddress(OBJECT, "provisional_plonk_verifier", result.contracts.stateTransition.verifiers.verifierPlonk);
            vm.serializeAddress(OBJECT, "provisional_ctm", result.contracts.stateTransition.proxies.chainTypeManager);
        }
        vm.serializeBytes(OBJECT, "proxy_admin_factory_calldata", result.payloads.proxyAdminCalldata);
        string memory output = vm.serializeBytes(OBJECT, "validator_timelock_factory_calldata", result.payloads.validatorTimelockCalldata);
        vm.writeJson(output, outputPath);
    }
}
