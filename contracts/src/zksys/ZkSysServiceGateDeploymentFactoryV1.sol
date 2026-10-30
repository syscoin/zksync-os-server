// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

interface IZkSysFactoryGateV1 {
    function deploymentAuthority() external view returns (address);
    function priorityGuard() external view returns (address);
    function installCoordinator(address coordinator) external;
}

interface IZkSysFactoryGuardV1 {
    function acceptanceGate() external view returns (address);
}

/// @dev The guard pins a not-yet-deployed gate; the gate must retain a way to
/// install its coordinator after bootstrap has established the accepted parent.
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

    function deploy(bytes memory guardCode, bytes memory gateCode) external {
        if (msg.sender != owner) revert Unauthorized();
        if (deploymentCommitment != bytes32(0)) revert AlreadyDeployed();
        deploymentCommitment = keccak256(abi.encode(guardCode, gateCode));
        address guard;
        address gate_;
        assembly ("memory-safe") {
            guard := create(0, add(guardCode, 32), mload(guardCode))
            gate_ := create(0, add(gateCode, 32), mload(gateCode))
        }
        if (guard == address(0) || gate_ == address(0)) revert DeploymentFailed();
        if (
            IZkSysFactoryGuardV1(guard).acceptanceGate() != gate_ || IZkSysFactoryGateV1(gate_).priorityGuard() != guard
                || IZkSysFactoryGateV1(gate_).deploymentAuthority() != address(this)
        ) revert InvalidBinding();
        gate = gate_;
    }

    function installCoordinator(address coordinator) external {
        if (msg.sender != owner) revert Unauthorized();
        if (gate == address(0) || coordinator.code.length == 0) revert InvalidBinding();
        IZkSysFactoryGateV1(gate).installCoordinator(coordinator);
    }
}
