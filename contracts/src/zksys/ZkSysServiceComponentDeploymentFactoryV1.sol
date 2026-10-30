// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// @dev A stable factory address allows independent chains to pin each other's
/// future component addresses before either constructor runs.
contract ZkSysServiceComponentDeploymentFactoryV1 {
    address public immutable owner;
    address public component;
    bytes32 public deploymentCommitment;

    error Unauthorized();
    error AlreadyDeployed();
    error DeploymentFailed();

    constructor(address owner_) {
        if (owner_ == address(0)) revert Unauthorized();
        owner = owner_;
    }

    function deploy(bytes memory componentCode) external {
        if (msg.sender != owner) revert Unauthorized();
        if (deploymentCommitment != bytes32(0)) revert AlreadyDeployed();
        deploymentCommitment = keccak256(componentCode);
        address component_;
        assembly ("memory-safe") {
            component_ := create(0, add(componentCode, 32), mload(componentCode))
        }
        if (component_ == address(0) || component_.code.length == 0) revert DeploymentFailed();
        component = component_;
    }
}
