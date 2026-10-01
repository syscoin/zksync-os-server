// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ZkSysProverServiceRegistryV1} from "./ZkSysProverServiceRegistryV1.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    IZkSysWrapperRosterSourceV1,
    ZkSysServiceTypesV1
} from "./ZkSysServiceTypesV1.sol";

struct ZkSysL2MessageV1 {
    uint16 txNumberInBatch;
    address sender;
    bytes data;
}

interface IZkSysL2MessageVerificationV1 {
    function proveL2MessageInclusionShared(
        uint256 chainId,
        uint256 blockOrBatchNumber,
        uint256 index,
        ZkSysL2MessageV1 calldata message,
        bytes32[] calldata proof
    ) external view returns (bool);
}

interface IZkSysChildMailboxV1 {
    function proveL2MessageInclusion(
        uint256 batchNumber,
        uint256 index,
        ZkSysL2MessageV1 calldata message,
        bytes32[] calldata proof
    ) external view returns (bool);
}

struct ServiceMessageProofV1 {
    uint256 blockOrBatchNumber;
    uint256 messageIndex;
    uint16 txNumberInBatch;
    bytes32[] merkleProof;
}

/// @notice Child-chain receiver for proof-authenticated Gateway service messages.
/// @dev V32's canonical message verifier checks the imported Gateway root. Its history-stability
/// assumption is the same as native interop; this is not an independent root-finality mechanism.
contract ZkSysAcceptedServiceReceiverV1 {
    bytes32 public constant ACCEPTED_DOMAIN = keccak256("ZKSYS_ACCEPTED_SERVICE_V1");
    bytes32 public constant ACTIVATION_DOMAIN = keccak256("ZKSYS_SERVICE_ACTIVATION_V1");
    IZkSysL2MessageVerificationV1 public constant MESSAGE_VERIFICATION =
        IZkSysL2MessageVerificationV1(address(0x10009));

    error InvalidConfiguration();
    error MessageNotIncluded();
    error WrongRegistry();

    uint256 public immutable gatewayChainId;
    address public immutable gatewayGate;
    address public immutable rootMessageSink;
    address public immutable aliasedRootMessageSink;
    ZkSysProverServiceRegistryV1 public immutable registry;
    mapping(bytes32 messageHash => bool relayed) public relayedMessages;
    mapping(bytes32 messageHash => bool relayed) public relayedRootMessages;
    bytes32 public childActivationVk;
    bytes32 public rootActivationVk;
    bytes32 public activationRosterRoot;

    constructor(
        uint256 gatewayChainId_,
        address gatewayGate_,
        ZkSysProverServiceRegistryV1 registry_,
        address rootMessageSink_
    ) {
        if (gatewayChainId_ == 0 || gatewayGate_ == address(0) || address(registry_) == address(0)) {
            revert InvalidConfiguration();
        }
        // The registry constructor authenticates this receiver's code, so its expected address is
        // pinned before deploying the registry; the reciprocal binding is checked on every relay.
        gatewayChainId = gatewayChainId_;
        gatewayGate = gatewayGate_;
        registry = registry_;
        rootMessageSink = rootMessageSink_;
        if (rootMessageSink_ != address(0)) {
            unchecked {
                aliasedRootMessageSink =
                    address(uint160(rootMessageSink_) + uint160(0x1111000000000000000000000000000000001111));
            }
        } else {
            aliasedRootMessageSink = address(0);
        }
    }

    function relayAccepted(
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties,
        bool bootstrap,
        ServiceMessageProofV1 calldata inclusion
    ) external {
        if (accepted.chainId != block.chainid) revert WrongRegistry();
        bytes memory data = abi.encode(ACCEPTED_DOMAIN, ZkSysServiceTypesV1.hashPackage(accepted), bootstrap);
        bytes32 messageHash = keccak256(data);
        if (relayedMessages[messageHash]) return;
        _authenticate(data, inclusion);
        relayedMessages[messageHash] = true;
        if (bootstrap) registry.acceptBootstrapDuties(accepted, duties);
        else registry.acceptDuties(accepted, duties);
    }

    function relayActivation(bytes32 vkHash, bytes32 qualifiedRosterRoot, ServiceMessageProofV1 calldata inclusion)
        external
    {
        bytes memory data = abi.encode(ACTIVATION_DOMAIN, vkHash, qualifiedRosterRoot);
        bytes32 messageHash = keccak256(data);
        if (relayedMessages[messageHash]) return;
        _authenticate(data, inclusion);
        relayedMessages[messageHash] = true;
        _recordActivation(vkHash, qualifiedRosterRoot, false);
    }

    function receiveRootAccepted(AcceptedPackageV1 calldata accepted, DutySuccessV1[] calldata duties, bool bootstrap)
        external
    {
        _authenticateRoot();
        if (accepted.chainId != gatewayChainId) revert WrongRegistry();
        bytes32 messageHash =
            keccak256(abi.encode(ACCEPTED_DOMAIN, ZkSysServiceTypesV1.hashPackage(accepted), bootstrap));
        if (relayedRootMessages[messageHash]) return;
        relayedRootMessages[messageHash] = true;
        if (bootstrap) registry.acceptBootstrapDuties(accepted, duties);
        else registry.acceptDuties(accepted, duties);
    }

    function receiveRootActivation(bytes32 vkHash, bytes32 qualifiedRosterRoot) external {
        _authenticateRoot();
        bytes32 messageHash = keccak256(abi.encode(ACTIVATION_DOMAIN, vkHash, qualifiedRosterRoot));
        if (relayedRootMessages[messageHash]) return;
        relayedRootMessages[messageHash] = true;
        _recordActivation(vkHash, qualifiedRosterRoot, true);
    }

    function _recordActivation(bytes32 vkHash, bytes32 root, bool fromRoot) private {
        (, bytes32 expectedVk) = registry.supportedLane(fromRoot ? gatewayChainId : block.chainid);
        if (
            vkHash == bytes32(0) || vkHash != expectedVk || root == bytes32(0)
                || root != registry.publishedRosterRoot(registry.firstServicePeriod())
                || (activationRosterRoot != bytes32(0) && activationRosterRoot != root)
        ) revert WrongRegistry();
        if (fromRoot) {
            if (rootActivationVk != bytes32(0) && rootActivationVk != vkHash) revert WrongRegistry();
            rootActivationVk = vkHash;
        } else {
            if (childActivationVk != bytes32(0) && childActivationVk != vkHash) revert WrongRegistry();
            childActivationVk = vkHash;
        }
        activationRosterRoot = root;
        if (childActivationVk != bytes32(0) && (rootMessageSink == address(0) || rootActivationVk != bytes32(0))) {
            registry.activateService(childActivationVk, root);
        }
    }

    function _checkRegistry() private view {
        if (
            address(registry).code.length == 0 || registry.acceptanceSource() != address(this)
                || (rootMessageSink == address(0)) != (registry.gatewayChainId() == 0)
                || (rootMessageSink != address(0) && registry.gatewayChainId() != gatewayChainId)
        ) revert WrongRegistry();
    }

    function _authenticateRoot() private view {
        _checkRegistry();
        if (rootMessageSink == address(0) || msg.sender != aliasedRootMessageSink) revert MessageNotIncluded();
    }

    function _authenticate(bytes memory data, ServiceMessageProofV1 calldata inclusion) private view {
        _checkRegistry();
        if (address(MESSAGE_VERIFICATION).code.length == 0) revert MessageNotIncluded();
        ZkSysL2MessageV1 memory message =
            ZkSysL2MessageV1({txNumberInBatch: inclusion.txNumberInBatch, sender: gatewayGate, data: data});
        if (!MESSAGE_VERIFICATION.proveL2MessageInclusionShared(
                gatewayChainId, inclusion.blockOrBatchNumber, inclusion.messageIndex, message, inclusion.merkleProof
            )) revert MessageNotIncluded();
    }
}

/// @notice Gateway roster source populated only by executed child-chain message proofs.
contract ZkSysQualifiedRosterReceiverV1 is IZkSysWrapperRosterSourceV1 {
    bytes32 public constant ROSTER_DOMAIN = keccak256("ZKSYS_QUALIFIED_WRAPPER_ROSTER_V1");

    struct Configuration {
        uint256 childChainId;
        address childRegistry;
        address messageSender;
        IZkSysChildMailboxV1 childMailbox;
        bytes32 policyHash;
        uint256 startTime;
        uint256 periodSeconds;
        uint64 firstServicePeriod;
    }

    struct Roster {
        bytes32 root;
        uint32 count;
    }

    error InvalidConfiguration();
    error InvalidRoster();
    error MessageNotIncluded();
    error RosterConflict();
    error ServiceNotStarted();

    uint256 public immutable childChainId;
    address public immutable childRegistry;
    address public immutable messageSender;
    IZkSysChildMailboxV1 public immutable childMailbox;
    bytes32 public immutable policyHash;
    uint256 public immutable startTime;
    uint256 public immutable periodSeconds;
    uint64 public immutable firstServicePeriod;
    mapping(uint64 period => Roster roster) private _rosters;

    event RosterReceived(uint64 indexed period, bytes32 root, uint32 count);

    constructor(Configuration memory config) {
        if (
            config.childChainId == 0 || config.childRegistry == address(0) || config.messageSender == address(0)
                || address(config.childMailbox).code.length == 0 || config.policyHash == bytes32(0)
                || config.periodSeconds == 0
        ) revert InvalidConfiguration();
        childChainId = config.childChainId;
        childRegistry = config.childRegistry;
        messageSender = config.messageSender;
        childMailbox = config.childMailbox;
        policyHash = config.policyHash;
        startTime = config.startTime;
        periodSeconds = config.periodSeconds;
        firstServicePeriod = config.firstServicePeriod;
    }

    function relayRoster(uint64 period, bytes32 root, uint32 count, ServiceMessageProofV1 calldata inclusion) external {
        if (period < firstServicePeriod || root == bytes32(0) || count == 0) revert InvalidRoster();
        Roster storage previous = _rosters[period];
        if (previous.root != bytes32(0)) {
            if (previous.root != root || previous.count != count) revert RosterConflict();
            return;
        }
        ZkSysL2MessageV1 memory message = ZkSysL2MessageV1({
            txNumberInBatch: inclusion.txNumberInBatch,
            sender: messageSender,
            data: abi.encode(
                ROSTER_DOMAIN,
                childChainId,
                childRegistry,
                policyHash,
                startTime,
                periodSeconds,
                firstServicePeriod,
                period,
                root,
                count
            )
        });
        if (!childMailbox.proveL2MessageInclusion(
                inclusion.blockOrBatchNumber, inclusion.messageIndex, message, inclusion.merkleProof
            )) revert MessageNotIncluded();
        previous.root = root;
        previous.count = count;
        emit RosterReceived(period, root, count);
    }

    /// @dev The root receiver verifies this fixed Gateway sender through the Gateway's native
    /// root Mailbox after bootstrap proving has carried the child roster message to NEVM.
    function forwardRosterToRoot(uint64 period) external returns (bytes32 messageHash) {
        Roster memory roster = _rosters[period];
        if (roster.root == bytes32(0) || roster.count == 0 || address(0x8008).code.length == 0) revert InvalidRoster();
        return IZkSysRosterMessengerV1(address(0x8008))
            .sendToL1(
                abi.encode(
                    ROSTER_DOMAIN,
                    childChainId,
                    childRegistry,
                    policyHash,
                    startTime,
                    periodSeconds,
                    firstServicePeriod,
                    period,
                    roster.root,
                    roster.count
                )
            );
    }

    function rosterFor(uint64 period) public view returns (bytes32 root, uint32 count) {
        Roster storage roster = _rosters[period];
        return (roster.root, roster.count);
    }

    function currentRoster() external view returns (uint64 period, bytes32 root, uint32 count) {
        if (block.timestamp < startTime) revert ServiceNotStarted();
        uint256 currentPeriod = (block.timestamp - startTime) / periodSeconds;
        if (currentPeriod < firstServicePeriod || currentPeriod > type(uint64).max) revert ServiceNotStarted();
        period = uint64(currentPeriod);
        (root, count) = rosterFor(period);
    }
}

interface IZkSysRosterMessengerV1 {
    function sendToL1(bytes calldata message) external returns (bytes32);
}
