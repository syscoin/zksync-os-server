// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IL1BridgehubMinimal} from "./ZkSysRegistryBridge.sol";
import {
    AcceptedPackageV1,
    DutySuccessV1,
    ZkSysServiceTypesV1,
    IZkSysServiceMessageSinkV1,
    IZkSysNativeRootDrawV1
} from "./ZkSysServiceTypesV1.sol";

interface IZkSysRootAcceptedReceiverV1 {
    function receiveRootAccepted(AcceptedPackageV1 calldata accepted, DutySuccessV1[] calldata duties, bool bootstrap)
        external;
    function receiveRootActivation(bytes32 vkHash, bytes32 qualifiedRosterRoot) external;
}

/// @notice NEVM outbox for messages emitted atomically by the Gateway's native proof gate.
/// @dev Publication has no independent verifier or owner attestation. A failed native proof reverts
/// the gate transaction and this outbox together; the Bridgehub relay is retryable and fee-funded.
contract ZkSysRootServiceMessageSinkV1 is IZkSysServiceMessageSinkV1 {
    bytes32 public constant ACCEPTED_DOMAIN = keccak256("ZKSYS_ACCEPTED_SERVICE_V1");
    bytes32 public constant ACTIVATION_DOMAIN = keccak256("ZKSYS_SERVICE_ACTIVATION_V1");

    error InvalidConfiguration();
    error UnauthorizedPublisher();
    error MessageNotPublished();

    struct Configuration {
        address publisher;
        IL1BridgehubMinimal bridgehub;
        uint256 gatewayChainId;
        address gatewayChainAddress;
        uint256 registryChainId;
        address registryReceiver;
        bytes32 policyHash;
    }

    address public immutable publisher;
    IL1BridgehubMinimal public immutable bridgehub;
    uint256 public immutable gatewayChainId;
    address public immutable gatewayChainAddress;
    uint256 public immutable registryChainId;
    address public immutable registryReceiver;
    bytes32 public immutable policyHash;
    mapping(bytes32 messageHash => bool published) public messages;

    event MessagePublished(bytes32 indexed messageHash);
    event MessageRelayed(bytes32 indexed messageHash, bytes32 indexed canonicalTxHash);

    constructor(Configuration memory config) {
        if (
            config.publisher == address(0) || address(config.bridgehub).code.length == 0 || config.gatewayChainId == 0
                || config.gatewayChainAddress == address(0) || config.registryChainId == 0
                || config.registryChainId == config.gatewayChainId || config.registryReceiver == address(0)
                || config.policyHash == bytes32(0)
        ) revert InvalidConfiguration();
        publisher = config.publisher;
        bridgehub = config.bridgehub;
        gatewayChainId = config.gatewayChainId;
        gatewayChainAddress = config.gatewayChainAddress;
        registryChainId = config.registryChainId;
        registryReceiver = config.registryReceiver;
        policyHash = config.policyHash;
    }

    function publish(bytes calldata message) external {
        if (msg.sender != publisher) revert UnauthorizedPublisher();
        if (message.length != 96) revert MessageNotPublished();
        bytes32 domain = abi.decode(message, (bytes32));
        if (domain != ACCEPTED_DOMAIN && domain != ACTIVATION_DOMAIN) revert MessageNotPublished();
        bytes32 hash = keccak256(message);
        messages[hash] = true;
        emit MessagePublished(hash);
    }

    function relayAccepted(
        AcceptedPackageV1 calldata accepted,
        DutySuccessV1[] calldata duties,
        bool bootstrap,
        uint256 gasLimit,
        uint256 gasPerPubdataByteLimit,
        address refundRecipient
    ) external payable returns (bytes32) {
        if (
            accepted.chainId != gatewayChainId || accepted.chainAddress != gatewayChainAddress
                || accepted.policyHash != policyHash || ZkSysServiceTypesV1.hashReport(duties) != accepted.reportHash
        ) {
            revert MessageNotPublished();
        }
        bytes32 hash = keccak256(abi.encode(ACCEPTED_DOMAIN, ZkSysServiceTypesV1.hashPackage(accepted), bootstrap));
        return _relay(
            hash,
            abi.encodeCall(IZkSysRootAcceptedReceiverV1.receiveRootAccepted, (accepted, duties, bootstrap)),
            gasLimit,
            gasPerPubdataByteLimit,
            refundRecipient
        );
    }

    function relayActivation(
        bytes32 vkHash,
        bytes32 root,
        uint256 gasLimit,
        uint256 gasPerPubdataByteLimit,
        address refundRecipient
    ) external payable returns (bytes32) {
        bytes32 hash = keccak256(abi.encode(ACTIVATION_DOMAIN, vkHash, root));
        return _relay(
            hash,
            abi.encodeCall(IZkSysRootAcceptedReceiverV1.receiveRootActivation, (vkHash, root)),
            gasLimit,
            gasPerPubdataByteLimit,
            refundRecipient
        );
    }

    function _relay(
        bytes32 hash,
        bytes memory payload,
        uint256 gasLimit,
        uint256 gasPerPubdataByteLimit,
        address refundRecipient
    ) private returns (bytes32 canonicalTxHash) {
        if (!messages[hash]) revert MessageNotPublished();
        canonicalTxHash = bridgehub.requestL2TransactionDirect{value: msg.value}(
            IL1BridgehubMinimal.L2TransactionRequestDirect({
                chainId: registryChainId,
                mintValue: msg.value,
                l2Contract: registryReceiver,
                l2Value: 0,
                l2Calldata: payload,
                l2GasLimit: gasLimit,
                l2GasPerPubdataByteLimit: gasPerPubdataByteLimit,
                factoryDeps: new bytes[](0),
                refundRecipient: refundRecipient == address(0) ? msg.sender : refundRecipient
            })
        );
        emit MessageRelayed(hash, canonicalTxHash);
    }
}

/// @notice Future root entropy requested directly by the NEVM Gateway coordinator.
/// @dev As for the relayed draw, native PoW block hashes are biasable and an expired uncaptured
/// result permits selective-abort bias. Independent capture keepers remain a launch requirement.
contract ZkSysNativeRootDrawV1 is IZkSysNativeRootDrawV1 {
    struct Draw {
        uint64 height;
        bytes32 blockHash;
    }
    error InvalidConfiguration();
    error UnauthorizedCoordinator();
    error DrawNotReady();
    error DrawStillAvailable();

    address public immutable coordinator;
    uint64 public immutable futureBlocks;
    uint64 public immutable confirmations;
    mapping(bytes32 commitment => Draw draw) public draws;

    event DrawRequested(bytes32 indexed commitment, uint64 indexed height);
    event DrawCaptured(bytes32 indexed commitment, uint64 indexed height, bytes32 rootHash);

    constructor(address coordinator_, uint64 futureBlocks_, uint64 confirmations_) {
        if (coordinator_ == address(0) || futureBlocks_ == 0 || confirmations_ == 0 || confirmations_ > 255) {
            revert InvalidConfiguration();
        }
        coordinator = coordinator_;
        futureBlocks = futureBlocks_;
        confirmations = confirmations_;
    }

    function requestDraw(bytes32 commitment) external {
        if (msg.sender != coordinator) revert UnauthorizedCoordinator();
        if (commitment == bytes32(0)) revert InvalidConfiguration();
        if (draws[commitment].height != 0) return;
        _schedule(commitment);
    }

    function _schedule(bytes32 commitment) private {
        uint256 height = block.number + futureBlocks;
        if (height > type(uint64).max) revert InvalidConfiguration();
        draws[commitment].height = uint64(height);
        emit DrawRequested(commitment, uint64(height));
    }

    function captureDraw(bytes32 commitment) external {
        Draw storage draw = draws[commitment];
        if (draw.blockHash != bytes32(0)) return;
        if (draw.height == 0 || block.number < uint256(draw.height) + confirmations) revert DrawNotReady();
        bytes32 hash = blockhash(draw.height);
        if (hash == bytes32(0)) revert DrawNotReady();
        draw.blockHash = hash;
        emit DrawCaptured(commitment, draw.height, hash);
    }

    function retryExpiredDraw(bytes32 commitment) external {
        Draw storage draw = draws[commitment];
        if (draw.height == 0 || draw.blockHash != bytes32(0) || block.number <= uint256(draw.height) + 256) {
            revert DrawStillAvailable();
        }
        _schedule(commitment);
    }

    function drawFor(bytes32 commitment) external view returns (uint64 height, bytes32 hash) {
        Draw memory draw = draws[commitment];
        return (draw.height, draw.blockHash);
    }
}
