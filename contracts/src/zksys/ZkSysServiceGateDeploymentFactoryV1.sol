// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

interface IZkSysFactoryGateV1 {
    function deploymentAuthority() external view returns (address);
    function installCoordinator(address coordinator) external;
}

/// @dev The gate retains this factory's authority to install its coordinator
/// after bootstrap has established the accepted parent.
contract ZkSysServiceGateDeploymentFactoryV1 {
    address public immutable owner;
    address public gate;
    bytes32 public deploymentCommitment;

    error Unauthorized();
    error AlreadyDeployed();
    error DeploymentFailed();
    error InvalidBinding();

    constructor(address owner_) {
        if (owner_ == address(0)) revert Unauthorized();
        owner = owner_;
    }

    function deploy(bytes memory gateCode) external {
        if (msg.sender != owner) revert Unauthorized();
        if (deploymentCommitment != bytes32(0)) revert AlreadyDeployed();
        deploymentCommitment = keccak256(gateCode);
        address gate_;
        assembly ("memory-safe") {
            gate_ := create(0, add(gateCode, 32), mload(gateCode))
        }
        if (gate_.code.length == 0) revert DeploymentFailed();
        if (IZkSysFactoryGateV1(gate_).deploymentAuthority() != address(this)) revert InvalidBinding();
        gate = gate_;
    }

    function installCoordinator(address coordinator) external {
        if (msg.sender != owner) revert Unauthorized();
        if (gate == address(0) || coordinator.code.length == 0) revert InvalidBinding();
        IZkSysFactoryGateV1(gate).installCoordinator(coordinator);
    }
}
