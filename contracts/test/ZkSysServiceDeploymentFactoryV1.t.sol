// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ZkSysServiceDeploymentFactoryV1} from "../src/zksys/ZkSysServiceDeploymentFactoryV1.sol";
import {ZkSysProverServiceRegistryV1} from "../src/zksys/ZkSysProverServiceRegistryV1.sol";
import {ZkSysAcceptedServiceReceiverV1} from "../src/zksys/ZkSysServiceMessageReceiversV1.sol";
import {ZkSysMembershipRegistry} from "../src/zksys/ZkSysMembershipRegistry.sol";

contract FactoryClockStub {
    uint256 public immutable startTime;
    uint256 public constant periodSeconds = 1 days;

    constructor() {
        startTime = block.timestamp + 1 days;
    }
}

contract FactoryReceiverStub {
    address public immutable registry;

    constructor(address registry_) {
        registry = registry_;
    }
}

contract FactoryRegistryStub {
    address public immutable receiver;

    constructor(address receiver_) {
        require(receiver_.code.length != 0);
        receiver = receiver_;
    }
}

contract ZkSysServiceDeploymentFactoryV1Test is Test {
    function testRealReceiverAndRegistryConstructors() public {
        ZkSysServiceDeploymentFactoryV1 factory = new ZkSysServiceDeploymentFactoryV1(address(this));
        FactoryClockStub clock = new FactoryClockStub();
        FactoryReceiverStub membership = new FactoryReceiverStub(address(1));
        address receiver = vm.computeCreateAddress(address(factory), 1);
        address registry = vm.computeCreateAddress(address(factory), 2);
        bytes memory receiverCode = abi.encodePacked(
            type(ZkSysAcceptedServiceReceiverV1).creationCode,
            abi.encode(uint256(5050), address(11), registry, address(12))
        );
        ZkSysProverServiceRegistryV1.Configuration memory config = ZkSysProverServiceRegistryV1.Configuration({
            membershipRegistry: ZkSysMembershipRegistry(address(membership)),
            issuer: address(clock),
            acceptanceSource: receiver,
            settlementChainAddress: address(13),
            policyHash: bytes32(uint256(1)),
            firstServicePeriod: 0,
            receiptGraceSeconds: 100,
            rosterPublicationLeadSeconds: 100,
            dutiesPerRound: 2,
            gatewayChainId: 5050,
            gatewayChainAddress: address(14),
            gatewayVkHash: bytes32(uint256(2)),
            sharedSequencer: address(15)
        });
        bytes memory registryCode =
            abi.encodePacked(type(ZkSysProverServiceRegistryV1).creationCode, abi.encode(config));
        factory.deploy(receiverCode, registryCode);
        assertEq(address(ZkSysAcceptedServiceReceiverV1(receiver).registry()), registry);
        assertEq(ZkSysProverServiceRegistryV1(registry).acceptanceSource(), receiver);
        assertEq(ZkSysProverServiceRegistryV1(registry).firstServicePeriod(), 0);
    }

    function testAtomicDeploymentAndAuthorization() public {
        ZkSysServiceDeploymentFactoryV1 factory = new ZkSysServiceDeploymentFactoryV1(address(this));
        address receiver = vm.computeCreateAddress(address(factory), 1);
        address registry = vm.computeCreateAddress(address(factory), 2);
        bytes memory receiverCode = abi.encodePacked(type(FactoryReceiverStub).creationCode, abi.encode(registry));
        bytes memory registryCode = abi.encodePacked(type(FactoryRegistryStub).creationCode, abi.encode(receiver));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceDeploymentFactoryV1.Unauthorized.selector);
        factory.deploy(receiverCode, registryCode);
        factory.deploy(receiverCode, registryCode);
        assertEq(FactoryReceiverStub(receiver).registry(), registry);
        assertEq(FactoryRegistryStub(registry).receiver(), receiver);
        assertEq(factory.deploymentCommitment(), keccak256(abi.encode(receiverCode, registryCode)));
        vm.expectRevert(ZkSysServiceDeploymentFactoryV1.AlreadyDeployed.selector);
        factory.deploy(receiverCode, registryCode);
    }

    function testDeploymentFailureRollsBackBothCreations() public {
        ZkSysServiceDeploymentFactoryV1 factory = new ZkSysServiceDeploymentFactoryV1(address(this));
        bytes memory receiverCode = abi.encodePacked(type(FactoryReceiverStub).creationCode, abi.encode(address(1)));
        bytes memory registryCode = abi.encodePacked(type(FactoryRegistryStub).creationCode, abi.encode(address(1)));
        vm.expectRevert(ZkSysServiceDeploymentFactoryV1.DeploymentFailed.selector);
        factory.deploy(receiverCode, registryCode);
        assertEq(vm.computeCreateAddress(address(factory), 1).code.length, 0);
        assertEq(factory.deploymentCommitment(), bytes32(0));
    }
}
