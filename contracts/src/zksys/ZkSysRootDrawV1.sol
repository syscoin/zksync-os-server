// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IL1BridgehubMinimal} from "./ZkSysRegistryBridge.sol";

interface IZkSysMessageProofV1 {
    struct L2Message {
        uint16 txNumberInBatch;
        address sender;
        bytes data;
    }

    function proveL2MessageInclusion(
        uint256 batchNumber,
        uint256 index,
        L2Message calldata message,
        bytes32[] calldata proof
    ) external view returns (bool);
}

interface IZkSysRootDrawReceiverV1 {
    function receiveDraw(bytes32 commitment, uint64 height, bytes32 rootHash) external;
}

/// @notice NEVM draw source whose entropy is sampled after the Gateway freeze is proved.
/// @dev This deliberately requires a Gateway-to-root round trip. A delayed relayed root head plus
/// an offset is not necessarily a future block and allows a proposer to choose known entropy.
/// Native block hashes retain PoW producer bias; this contract does not verify ChainLocks.
/// At least one independent keeper must capture each available result. If every keeper withholds
/// an unfavorable result until blockhash expiry, the recovery path permits selective-abort bias.
contract ZkSysRootDrawV1 {
    bytes32 public constant DRAW_DOMAIN = keccak256("ZKSYS_WRAPPER_DRAW_V1");

    struct Draw {
        uint64 height;
        bytes32 blockHash;
    }

    error InvalidConfiguration();
    error InvalidFreezeProof();
    error DrawNotReady();
    error DrawStillAvailable();

    IZkSysMessageProofV1 public immutable gatewayMailbox;
    IL1BridgehubMinimal public immutable bridgehub;
    address public immutable gatewayCoordinator;
    address public immutable gatewayReceiver;
    uint256 public immutable gatewayChainId;
    uint64 public immutable futureBlocks;
    uint64 public immutable confirmations;
    mapping(bytes32 commitment => Draw draw) public draws;

    event DrawRequested(bytes32 indexed commitment, uint64 indexed height);
    event DrawCaptured(bytes32 indexed commitment, uint64 indexed height, bytes32 rootHash);
    event DrawRelayed(bytes32 indexed commitment, bytes32 indexed canonicalTxHash);

    constructor(
        IZkSysMessageProofV1 mailbox_,
        IL1BridgehubMinimal bridgehub_,
        address coordinator_,
        address receiver_,
        uint256 gatewayChainId_,
        uint64 futureBlocks_,
        uint64 confirmations_
    ) {
        if (
            address(mailbox_).code.length == 0 || address(bridgehub_).code.length == 0 || coordinator_ == address(0)
                || receiver_ == address(0) || gatewayChainId_ == 0 || futureBlocks_ == 0 || confirmations_ == 0
                || confirmations_ > 255
        ) revert InvalidConfiguration();
        gatewayMailbox = mailbox_;
        bridgehub = bridgehub_;
        gatewayCoordinator = coordinator_;
        gatewayReceiver = receiver_;
        gatewayChainId = gatewayChainId_;
        futureBlocks = futureBlocks_;
        confirmations = confirmations_;
    }

    function requestDraw(
        bytes32 commitment,
        uint256 batchNumber,
        uint256 index,
        uint16 txNumberInBatch,
        bytes32[] calldata proof
    ) external {
        if (commitment == bytes32(0)) revert InvalidFreezeProof();
        if (draws[commitment].height != 0) return;
        IZkSysMessageProofV1.L2Message memory message = IZkSysMessageProofV1.L2Message({
            txNumberInBatch: txNumberInBatch, sender: gatewayCoordinator, data: abi.encode(DRAW_DOMAIN, commitment)
        });
        if (!gatewayMailbox.proveL2MessageInclusion(batchNumber, index, message, proof)) {
            revert InvalidFreezeProof();
        }
        _schedule(commitment);
    }

    /// @dev Once captured, a result can never be rerolled. An expired uncaptured block can only
    /// schedule fresh future entropy for the same immutable work commitment.
    function retryExpiredDraw(bytes32 commitment) external {
        Draw storage draw = draws[commitment];
        if (draw.height == 0 || draw.blockHash != bytes32(0) || block.number <= uint256(draw.height) + 256) {
            revert DrawStillAvailable();
        }
        _schedule(commitment);
    }

    function _schedule(bytes32 commitment) private {
        uint256 height = block.number + futureBlocks;
        if (height > type(uint64).max) revert InvalidConfiguration();
        draws[commitment].height = uint64(height);
        emit DrawRequested(commitment, uint64(height));
    }

    function captureDraw(bytes32 commitment) public {
        Draw storage draw = draws[commitment];
        if (draw.blockHash != bytes32(0)) return;
        if (draw.height == 0 || block.number < uint256(draw.height) + confirmations) revert DrawNotReady();
        bytes32 rootHash = blockhash(draw.height);
        if (rootHash == bytes32(0)) revert DrawNotReady();
        draw.blockHash = rootHash;
        emit DrawCaptured(commitment, draw.height, rootHash);
    }

    /// @dev Repeated paid relay requests repair a failed L1-to-L2 delivery without altering a draw.
    function relayDraw(bytes32 commitment, uint256 gasLimit, uint256 gasPerPubdataByteLimit, address refundRecipient)
        external
        payable
        returns (bytes32 canonicalTxHash)
    {
        captureDraw(commitment);
        Draw memory draw = draws[commitment];
        canonicalTxHash = bridgehub.requestL2TransactionDirect{value: msg.value}(
            IL1BridgehubMinimal.L2TransactionRequestDirect({
                chainId: gatewayChainId,
                mintValue: msg.value,
                l2Contract: gatewayReceiver,
                l2Value: 0,
                l2Calldata: abi.encodeCall(
                    IZkSysRootDrawReceiverV1.receiveDraw, (commitment, draw.height, draw.blockHash)
                ),
                l2GasLimit: gasLimit,
                l2GasPerPubdataByteLimit: gasPerPubdataByteLimit,
                factoryDeps: new bytes[](0),
                refundRecipient: refundRecipient == address(0) ? msg.sender : refundRecipient
            })
        );
        emit DrawRelayed(commitment, canonicalTxHash);
    }
}

/// @notice Gateway receiver; only the canonical aliased NEVM draw contract can install entropy.
contract ZkSysRootDrawReceiverV1 is IZkSysRootDrawReceiverV1 {
    error UnauthorizedSource();
    error InvalidDraw();
    address public immutable aliasedRootSource;
    mapping(bytes32 commitment => ZkSysRootDrawV1.Draw draw) private _draws;

    constructor(address rootSource) {
        if (rootSource == address(0)) revert InvalidDraw();
        unchecked {
            aliasedRootSource = address(uint160(rootSource) + uint160(0x1111000000000000000000000000000000001111));
        }
    }

    function receiveDraw(bytes32 commitment, uint64 height, bytes32 rootHash) external {
        if (msg.sender != aliasedRootSource) revert UnauthorizedSource();
        if (commitment == bytes32(0) || height == 0 || rootHash == bytes32(0)) revert InvalidDraw();
        ZkSysRootDrawV1.Draw storage draw = _draws[commitment];
        if (draw.height != 0 && (draw.height != height || draw.blockHash != rootHash)) revert InvalidDraw();
        draw.height = height;
        draw.blockHash = rootHash;
    }

    function drawFor(bytes32 commitment) external view returns (uint64 height, bytes32 rootHash) {
        ZkSysRootDrawV1.Draw memory draw = _draws[commitment];
        return (draw.height, draw.blockHash);
    }
}
