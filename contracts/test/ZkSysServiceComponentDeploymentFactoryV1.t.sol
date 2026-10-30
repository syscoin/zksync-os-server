// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {ZkSysServiceComponentDeploymentFactoryV1} from "../src/zksys/ZkSysServiceComponentDeploymentFactoryV1.sol";
import {
    ZkSysRootPrioritySourceV1,
    ZkSysPriorityCheckpointReceiverV1
} from "contracts/src/zksys/ZkSysPriorityGuardV1.sol";
import {ZkSysRootServiceMessageSinkV1, ZkSysNativeRootDrawV1} from "contracts/src/zksys/ZkSysRootServiceV1.sol";
import {ZkSysRootDrawV1, ZkSysRootDrawReceiverV1, IZkSysMessageProofV1} from "contracts/src/zksys/ZkSysRootDrawV1.sol";
import {
    ZkSysQualifiedRosterReceiverV1,
    IZkSysChildMailboxV1
} from "contracts/src/zksys/ZkSysServiceMessageReceiversV1.sol";
import {ZkSysWrapperCoordinatorV1} from "contracts/src/zksys/ZkSysWrapperCoordinatorV1.sol";
import {
    IZkSysAuthenticatedRootSourceV1,
    IZkSysWrapperRosterSourceV1
} from "contracts/src/zksys/ZkSysServiceTypesV1.sol";
import {IL1BridgehubMinimal} from "contracts/src/zksys/ZkSysRegistryBridge.sol";
import {PriorityMailboxMock} from "./ZkSysPriorityGuard.t.sol";
import {RootDrawHub} from "./ZkSysRootDraw.t.sol";
import {GateRosterSource} from "./ZkSysProofGate.t.sol";

contract ZkSysServiceComponentDeploymentFactoryV1Test is Test {
    bytes32 internal constant POLICY = keccak256("policy");

    function testActualCrossChainPriorityComponents() public {
        PriorityMailboxMock mailbox = new PriorityMailboxMock();
        RootDrawHub hub = new RootDrawHub();
        ZkSysServiceComponentDeploymentFactoryV1 sourceFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        ZkSysServiceComponentDeploymentFactoryV1 receiverFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        address source = vm.computeCreateAddress(address(sourceFactory), 1);
        address receiver = vm.computeCreateAddress(address(receiverFactory), 1);
        bytes memory sourceCode = abi.encodePacked(
            type(ZkSysRootPrioritySourceV1).creationCode, abi.encode(mailbox, hub, receiver, 5050, POLICY, 100)
        );
        bytes memory receiverCode =
            abi.encodePacked(type(ZkSysPriorityCheckpointReceiverV1).creationCode, abi.encode(source, 57, POLICY));
        sourceFactory.deploy(sourceCode);
        receiverFactory.deploy(receiverCode);
        assertEq(sourceFactory.component(), source);
        assertEq(receiverFactory.component(), receiver);
        assertEq(ZkSysRootPrioritySourceV1(source).gatewayReceiver(), receiver);
        assertEq(ZkSysRootPrioritySourceV1(source).childChainId(), 57);
        assertEq(ZkSysPriorityCheckpointReceiverV1(receiver).childChainId(), 57);
        address aliasAddress;
        unchecked {
            aliasAddress = address(uint160(source) + uint160(0x1111000000000000000000000000000000001111));
        }
        assertEq(ZkSysPriorityCheckpointReceiverV1(receiver).aliasedRootSource(), aliasAddress);
        assertEq(sourceFactory.deploymentCommitment(), keccak256(sourceCode));
    }

    function testActualRootServiceSinkAndOneTimeAuthorization() public {
        RootDrawHub hub = new RootDrawHub();
        PriorityMailboxMock gatewayMailbox = new PriorityMailboxMock();
        hub.setChain(57, address(gatewayMailbox));
        ZkSysServiceComponentDeploymentFactoryV1 factory = new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        address sink = vm.computeCreateAddress(address(factory), 1);
        ZkSysRootServiceMessageSinkV1.Configuration memory config = ZkSysRootServiceMessageSinkV1.Configuration({
            publisher: address(10),
            gatewayGate: address(13),
            bridgehub: IL1BridgehubMinimal(address(hub)),
            gatewayChainId: 57,
            gatewayChainAddress: address(gatewayMailbox),
            registryChainId: 57057,
            registryReceiver: address(12),
            policyHash: POLICY
        });
        config.gatewayChainId = 58;
        vm.expectRevert(ZkSysRootServiceMessageSinkV1.InvalidConfiguration.selector);
        new ZkSysRootServiceMessageSinkV1(config);
        config.gatewayChainId = 57;
        bytes memory code = abi.encodePacked(type(ZkSysRootServiceMessageSinkV1).creationCode, abi.encode(config));
        vm.prank(address(42));
        vm.expectRevert(ZkSysServiceComponentDeploymentFactoryV1.Unauthorized.selector);
        factory.deploy(code);
        factory.deploy(code);
        assertEq(factory.component(), sink);
        assertEq(ZkSysRootServiceMessageSinkV1(sink).publisher(), address(10));
        assertEq(ZkSysRootServiceMessageSinkV1(sink).gatewayGate(), address(13));
        assertEq(ZkSysRootServiceMessageSinkV1(sink).registryReceiver(), address(12));
        vm.expectRevert(ZkSysServiceComponentDeploymentFactoryV1.AlreadyDeployed.selector);
        factory.deploy(code);
    }

    function testActualRosterAndCrossChainDrawConstructors() public {
        PriorityMailboxMock mailbox = new PriorityMailboxMock();
        RootDrawHub hub = new RootDrawHub();
        ZkSysServiceComponentDeploymentFactoryV1 rosterFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        ZkSysQualifiedRosterReceiverV1.Configuration memory config = ZkSysQualifiedRosterReceiverV1.Configuration({
            childChainId: 57057,
            childRegistry: address(10),
            messageSender: address(11),
            childMailbox: IZkSysChildMailboxV1(address(mailbox)),
            policyHash: POLICY,
            startTime: 1000,
            periodSeconds: 86400,
            firstServicePeriod: 0
        });
        rosterFactory.deploy(abi.encodePacked(type(ZkSysQualifiedRosterReceiverV1).creationCode, abi.encode(config)));
        ZkSysQualifiedRosterReceiverV1 roster = ZkSysQualifiedRosterReceiverV1(rosterFactory.component());
        assertEq(roster.childRegistry(), address(10));
        assertEq(roster.messageSender(), address(11));
        assertEq(address(roster.childMailbox()), address(mailbox));

        ZkSysServiceComponentDeploymentFactoryV1 sourceFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        ZkSysServiceComponentDeploymentFactoryV1 receiverFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        address source = vm.computeCreateAddress(address(sourceFactory), 1);
        address receiver = vm.computeCreateAddress(address(receiverFactory), 1);
        receiverFactory.deploy(abi.encodePacked(type(ZkSysRootDrawReceiverV1).creationCode, abi.encode(source)));
        sourceFactory.deploy(
            abi.encodePacked(
                type(ZkSysRootDrawV1).creationCode,
                abi.encode(IZkSysMessageProofV1(address(mailbox)), hub, address(12), receiver, 57001, 3, 2)
            )
        );
        assertEq(ZkSysRootDrawV1(source).gatewayReceiver(), receiver);
        assertEq(ZkSysRootDrawV1(source).gatewayCoordinator(), address(12));
        address aliasAddress;
        unchecked {
            aliasAddress = address(uint160(source) + uint160(0x1111000000000000000000000000000000001111));
        }
        assertEq(ZkSysRootDrawReceiverV1(receiver).aliasedRootSource(), aliasAddress);
    }

    function testActualNativeDrawPinsPredictedCoordinator() public {
        PriorityMailboxMock gate = new PriorityMailboxMock();
        GateRosterSource roster = new GateRosterSource(POLICY);
        ZkSysServiceComponentDeploymentFactoryV1 drawFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        ZkSysServiceComponentDeploymentFactoryV1 coordinatorFactory =
            new ZkSysServiceComponentDeploymentFactoryV1(address(this));
        address draw = vm.computeCreateAddress(address(drawFactory), 1);
        address coordinator = vm.computeCreateAddress(address(coordinatorFactory), 1);
        drawFactory.deploy(abi.encodePacked(type(ZkSysNativeRootDrawV1).creationCode, abi.encode(coordinator, 3, 2)));
        ZkSysWrapperCoordinatorV1.Configuration memory config = ZkSysWrapperCoordinatorV1.Configuration({
            acceptanceGate: address(gate),
            rootSource: IZkSysAuthenticatedRootSourceV1(draw),
            rosterSource: IZkSysWrapperRosterSourceV1(address(roster)),
            sequencer: address(10),
            childChainId: 57001,
            childChainAddress: address(11),
            policyHash: POLICY,
            productionVkHash: keccak256("vk"),
            initialParent: keccak256("parent"),
            firstBatch: 4,
            firstServicePeriod: 0,
            turnSeconds: 30,
            nativeRootDraw: true
        });
        coordinatorFactory.deploy(abi.encodePacked(type(ZkSysWrapperCoordinatorV1).creationCode, abi.encode(config)));
        assertEq(ZkSysNativeRootDrawV1(draw).coordinator(), coordinator);
        assertEq(address(ZkSysWrapperCoordinatorV1(coordinator).rootSource()), draw);
        assertEq(ZkSysWrapperCoordinatorV1(coordinator).nextBatch(), 4);
    }
}
