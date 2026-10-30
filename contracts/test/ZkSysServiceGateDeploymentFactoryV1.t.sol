// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ZkSysServiceGateDeploymentFactoryV1} from "../src/zksys/ZkSysServiceGateDeploymentFactoryV1.sol";
import {ZkSysPriorityGuardV1, IZkSysPriorityMailboxV1} from "../src/zksys/ZkSysPriorityGuardV1.sol";
import {ZkSysProofGateV1} from "../src/zksys/ZkSysProofGateV1.sol";
import {ZkSysWrapperCoordinatorV1} from "../src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {IZkSysAuthenticatedRootSourceV1, IZkSysWrapperRosterSourceV1} from "../src/zksys/ZkSysServiceTypesV1.sol";
import {
    GateVerifier,
    GateSettlement,
    GatePriorityCheckpoint,
    GateDrawSource,
    GateRosterSource
} from "./ZkSysProofGate.t.sol";

contract GateFactoryGuardStub {
    address public immutable acceptanceGate;

    constructor(address expectedGate) {
        acceptanceGate = expectedGate;
    }
}

contract GateFactoryGateStub {
    address public immutable deploymentAuthority;
    address public immutable priorityGuard;
    address public coordinator;

    constructor(address expectedGuard) {
        require(expectedGuard.code.length != 0);
        deploymentAuthority = msg.sender;
        priorityGuard = expectedGuard;
    }

    function installCoordinator(address candidate) external {
        require(msg.sender == deploymentAuthority && coordinator == address(0));
        coordinator = candidate;
    }
}

contract ZkSysServiceGateDeploymentFactoryV1Test is Test {
    function testAtomicBindingAndCoordinatorInstallation() public {
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        address guard = vm.computeCreateAddress(address(factory), 1);
        address gate = vm.computeCreateAddress(address(factory), 2);
        bytes memory guardCode = abi.encodePacked(type(GateFactoryGuardStub).creationCode, abi.encode(gate));
        bytes memory gateCode = abi.encodePacked(type(GateFactoryGateStub).creationCode, abi.encode(guard));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.Unauthorized.selector);
        factory.deploy(guardCode, gateCode);
        factory.deploy(guardCode, gateCode);
        assertEq(factory.gate(), gate);
        assertEq(GateFactoryGuardStub(guard).acceptanceGate(), gate);
        assertEq(GateFactoryGateStub(gate).priorityGuard(), guard);
        assertEq(factory.deploymentCommitment(), keccak256(abi.encode(guardCode, gateCode)));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.Unauthorized.selector);
        factory.installCoordinator(address(this));
        factory.installCoordinator(address(this));
        assertEq(GateFactoryGateStub(gate).coordinator(), address(this));
        vm.expectRevert();
        factory.installCoordinator(address(this));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.AlreadyDeployed.selector);
        factory.deploy(guardCode, gateCode);
    }

    function testRealGuardAndGateBindAndInstallCoordinator() public {
        bytes32 policy = keccak256("policy");
        GateVerifier verifier = new GateVerifier();
        GateSettlement settlement = new GateSettlement(verifier);
        GatePriorityCheckpoint checkpoint = new GatePriorityCheckpoint(policy);
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        address guardAddress = vm.computeCreateAddress(address(factory), 1);
        address gateAddress = vm.computeCreateAddress(address(factory), 2);
        bytes memory guardCode = abi.encodePacked(
            type(ZkSysPriorityGuardV1).creationCode,
            abi.encode(gateAddress, IZkSysPriorityMailboxV1(address(settlement)), checkpoint, policy, 100, 600, 64)
        );
        bytes memory gateCode = abi.encodePacked(
            type(ZkSysProofGateV1).creationCode,
            abi.encode(settlement, settlement, address(123), policy, verifier.vkHash(), guardAddress, address(0))
        );
        factory.deploy(guardCode, gateCode);
        ZkSysPriorityGuardV1 guard = ZkSysPriorityGuardV1(guardAddress);
        ZkSysProofGateV1 gate = ZkSysProofGateV1(gateAddress);
        assertEq(guard.acceptanceGate(), gateAddress);
        assertEq(address(gate.priorityGuard()), guardAddress);
        assertEq(gate.deploymentAuthority(), address(factory));
        GateDrawSource draw = new GateDrawSource();
        GateRosterSource roster = new GateRosterSource(keccak256("roster"));
        ZkSysWrapperCoordinatorV1 coordinator = new ZkSysWrapperCoordinatorV1(
            ZkSysWrapperCoordinatorV1.Configuration({
                acceptanceGate: gateAddress,
                rootSource: IZkSysAuthenticatedRootSourceV1(address(draw)),
                rosterSource: IZkSysWrapperRosterSourceV1(address(roster)),
                sequencer: address(123),
                childChainId: settlement.getChainId(),
                childChainAddress: address(settlement),
                policyHash: policy,
                productionVkHash: verifier.vkHash(),
                initialParent: gate.lastAcceptedPackage(),
                firstBatch: 1,
                firstServicePeriod: 0,
                turnSeconds: 100,
                nativeRootDraw: false
            })
        );
        factory.installCoordinator(address(coordinator));
        assertEq(address(gate.coordinator()), address(coordinator));
    }

    function testWrongPredictedGateRollsBackBothCreations() public {
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        address guard = vm.computeCreateAddress(address(factory), 1);
        bytes memory guardCode = abi.encodePacked(type(GateFactoryGuardStub).creationCode, abi.encode(address(123)));
        bytes memory gateCode = abi.encodePacked(type(GateFactoryGateStub).creationCode, abi.encode(guard));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.InvalidBinding.selector);
        factory.deploy(guardCode, gateCode);
        assertEq(guard.code.length, 0);
        assertEq(factory.deploymentCommitment(), bytes32(0));
    }
}
