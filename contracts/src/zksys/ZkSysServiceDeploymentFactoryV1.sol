// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// @dev The receiver and registry refer to one another. Atomic CREATE deployment
/// fixes both addresses before either constructor executes, without EOA nonce races.
contract ZkSysServiceDeploymentFactoryV1 {
    address public immutable owner;
    bytes32 public deploymentCommitment;

    error Unauthorized();
    error AlreadyDeployed();
    error DeploymentFailed();

    constructor(address owner_) {
        if (owner_ == address(0)) revert Unauthorized();
        owner = owner_;
    }

    function deploy(bytes memory receiverCode, bytes memory registryCode) external {
        if (msg.sender != owner) revert Unauthorized();
        if (deploymentCommitment != bytes32(0)) revert AlreadyDeployed();
        deploymentCommitment = keccak256(abi.encode(receiverCode, registryCode));
        address receiver;
        address registry;
        assembly ("memory-safe") {
            receiver := create(0, add(receiverCode, 32), mload(receiverCode))
            registry := create(0, add(registryCode, 32), mload(registryCode))
        }
        if (receiver == address(0) || registry == address(0)) revert DeploymentFailed();
    }
}
