// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ZkSysServiceGateDeploymentFactoryV1} from "../src/zksys/ZkSysServiceGateDeploymentFactoryV1.sol";
import {ZkSysProofGateV1} from "../src/zksys/ZkSysProofGateV1.sol";
import {ZkSysWrapperCoordinatorV1} from "../src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {IZkSysAuthenticatedRootSourceV1, IZkSysWrapperRosterSourceV1} from "../src/zksys/ZkSysServiceTypesV1.sol";
import {GateVerifier, GateSettlement, GateDrawSource, GateRosterSource} from "./ZkSysProofGate.t.sol";

contract GateFactoryGateStub {
    address public immutable deploymentAuthority;
    address public coordinator;

    constructor(address authority) {
        deploymentAuthority = authority;
    }

    function installCoordinator(address candidate) external {
        require(msg.sender == deploymentAuthority && coordinator == address(0));
        coordinator = candidate;
    }
}

contract ZkSysServiceGateDeploymentFactoryV1Test is Test {
    function testAtomicBindingAndCoordinatorInstallation() public {
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        address gate = vm.computeCreateAddress(address(factory), 1);
        bytes memory gateCode = abi.encodePacked(type(GateFactoryGateStub).creationCode, abi.encode(address(factory)));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.Unauthorized.selector);
        factory.deploy(gateCode);
        factory.deploy(gateCode);
        assertEq(factory.gate(), gate);
        assertEq(factory.deploymentCommitment(), keccak256(gateCode));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.Unauthorized.selector);
        factory.installCoordinator(address(this));
        factory.installCoordinator(address(this));
        assertEq(GateFactoryGateStub(gate).coordinator(), address(this));
        vm.expectRevert();
        factory.installCoordinator(address(this));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.AlreadyDeployed.selector);
        factory.deploy(gateCode);
    }

    function testRealGateInstallsCoordinator() public {
        bytes32 policy = keccak256("policy");
        GateVerifier verifier = new GateVerifier();
        GateSettlement settlement = new GateSettlement(verifier);
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        address gateAddress = vm.computeCreateAddress(address(factory), 1);
        bytes memory gateCode = abi.encodePacked(
            type(ZkSysProofGateV1).creationCode,
            abi.encode(settlement, settlement, address(123), policy, verifier.vkHash(), address(0))
        );
        factory.deploy(gateCode);
        ZkSysProofGateV1 gate = ZkSysProofGateV1(gateAddress);
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

    function testIncorrectDeploymentAuthorityRollsBackCreation() public {
        ZkSysServiceGateDeploymentFactoryV1 factory = new ZkSysServiceGateDeploymentFactoryV1(address(this));
        bytes memory gateCode = abi.encodePacked(type(GateFactoryGateStub).creationCode, abi.encode(address(123)));
        vm.expectRevert(ZkSysServiceGateDeploymentFactoryV1.InvalidBinding.selector);
        factory.deploy(gateCode);
        assertEq(vm.computeCreateAddress(address(factory), 1).code.length, 0);
        assertEq(factory.deploymentCommitment(), bytes32(0));
    }
}
