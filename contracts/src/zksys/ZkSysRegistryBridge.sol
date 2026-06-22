// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

interface IL1BridgehubMinimal {
    struct L2TransactionRequestDirect {
        uint256 chainId;
        uint256 mintValue;
        address l2Contract;
        uint256 l2Value;
        bytes l2Calldata;
        uint256 l2GasLimit;
        uint256 l2GasPerPubdataByteLimit;
        bytes[] factoryDeps;
        address refundRecipient;
    }

    function requestL2TransactionDirect(L2TransactionRequestDirect calldata request)
        external
        payable
        returns (bytes32 canonicalTxHash);
}

interface IZkSysMembershipRegistryL2 {
    struct SentryNodeUpdate {
        address account;
        uint32 sentryNodeCollateralHeight;
        uint128 sentryNodeWeight;
    }

    function applyL1SentryNodeUpdates(SentryNodeUpdate[] calldata updates) external;
}

/// @title ZkSysRegistryBridge
/// @notice L1/NEVM adapter that sends verified membership facts to the L2 zkSYS registry.
contract ZkSysRegistryBridge {
    address private constant NEVM_ADDRESS_PRECOMPILE = address(0x62);
    uint256 public constant MAX_BATCH_SIZE = 512;
    uint256 public constant SENTRY_NODE_BASE_WEIGHT = 100_000 ether;
    uint256 public constant BPS_DENOMINATOR = 10_000;

    error DuplicateAccount(address account);
    error EmptyBatch();
    error InvalidAddress();
    error InvalidBatchSize(uint256 batchSize);
    error NevmCollateralHeightOverflow(address account, uint256 collateralHeight);
    error NevmLookupFailed(address account);
    error InvalidSeniorityConfig();
    error SentryNodeWeightOverflow(uint256 weight);

    IL1BridgehubMinimal public immutable bridgehub;
    uint256 public immutable zksysChainId;
    address public immutable l2Registry;
    uint32 public immutable nevmStartBlock;
    uint32 public immutable seniorityHeight1;
    uint32 public immutable seniorityHeight2;
    uint16 public immutable seniorityLevel1Bps;
    uint16 public immutable seniorityLevel2Bps;

    event RegistryUpdatesRequested(bytes32 indexed canonicalTxHash, uint256 updateCount);

    constructor(
        IL1BridgehubMinimal bridgehub_,
        uint256 zksysChainId_,
        address l2Registry_,
        uint32 nevmStartBlock_,
        uint32 seniorityHeight1_,
        uint32 seniorityHeight2_,
        uint16 seniorityLevel1Bps_,
        uint16 seniorityLevel2Bps_
    ) {
        if (address(bridgehub_) == address(0) || zksysChainId_ == 0 || l2Registry_ == address(0)) {
            revert InvalidAddress();
        }
        if (
            nevmStartBlock_ == 0 || seniorityHeight1_ == 0 || seniorityHeight2_ <= seniorityHeight1_
                || seniorityLevel2Bps_ < seniorityLevel1Bps_
        ) {
            revert InvalidSeniorityConfig();
        }

        bridgehub = bridgehub_;
        zksysChainId = zksysChainId_;
        l2Registry = l2Registry_;
        nevmStartBlock = nevmStartBlock_;
        seniorityHeight1 = seniorityHeight1_;
        seniorityHeight2 = seniorityHeight2_;
        seniorityLevel1Bps = seniorityLevel1Bps_;
        seniorityLevel2Bps = seniorityLevel2Bps_;
    }

    function pushSentryNodeUpdates(
        address[] calldata accounts,
        uint256 l2GasLimit,
        uint256 l2GasPerPubdataByteLimit,
        address refundRecipient
    ) external payable returns (bytes32 canonicalTxHash) {
        if (accounts.length == 0) {
            revert EmptyBatch();
        }
        if (accounts.length > MAX_BATCH_SIZE) {
            revert InvalidBatchSize(accounts.length);
        }

        for (uint256 i = 0; i < accounts.length; ++i) {
            address account = accounts[i];
            if (account == address(0)) {
                revert InvalidAddress();
            }
            for (uint256 j = 0; j < i; ++j) {
                if (accounts[j] == account) {
                    revert DuplicateAccount(account);
                }
            }
        }

        IZkSysMembershipRegistryL2.SentryNodeUpdate[] memory updates =
            new IZkSysMembershipRegistryL2.SentryNodeUpdate[](accounts.length);

        for (uint256 i = 0; i < accounts.length; ++i) {
            address account = accounts[i];
            uint32 collateralHeight = nevmCollateralHeight(account);
            updates[i] = IZkSysMembershipRegistryL2.SentryNodeUpdate({
                account: account,
                sentryNodeCollateralHeight: collateralHeight,
                sentryNodeWeight: sentryNodeWeight(collateralHeight, block.number)
            });
        }

        bytes[] memory factoryDeps = new bytes[](0);
        canonicalTxHash = bridgehub.requestL2TransactionDirect{value: msg.value}(
            IL1BridgehubMinimal.L2TransactionRequestDirect({
                chainId: zksysChainId,
                mintValue: msg.value,
                l2Contract: l2Registry,
                l2Value: 0,
                l2Calldata: abi.encodeCall(IZkSysMembershipRegistryL2.applyL1SentryNodeUpdates, (updates)),
                l2GasLimit: l2GasLimit,
                l2GasPerPubdataByteLimit: l2GasPerPubdataByteLimit,
                factoryDeps: factoryDeps,
                refundRecipient: refundRecipient == address(0) ? msg.sender : refundRecipient
            })
        );

        emit RegistryUpdatesRequested(canonicalTxHash, accounts.length);
    }

    function nevmCollateralHeight(address account) public view returns (uint32 collateralHeight) {
        (bool success, bytes memory result) = NEVM_ADDRESS_PRECOMPILE.staticcall(abi.encodePacked(account));
        if (!success || result.length != 32) {
            revert NevmLookupFailed(account);
        }

        uint256 rawCollateralHeight = uint256(bytes32(result));
        if (rawCollateralHeight > type(uint32).max) {
            revert NevmCollateralHeightOverflow(account, rawCollateralHeight);
        }
        collateralHeight = uint32(rawCollateralHeight);
    }

    function nevmSentryNodeWeight(address account) public view returns (uint128) {
        uint32 collateralHeight = nevmCollateralHeight(account);
        return sentryNodeWeight(collateralHeight, block.number);
    }

    function sentryNodeWeight(uint32 collateralHeight, uint256 currentHeight) public view returns (uint128 weight) {
        if (collateralHeight == 0) {
            return 0;
        }

        uint256 seniorityBps = seniorityBpsAt(collateralHeight, currentHeight);
        uint256 computedWeight = SENTRY_NODE_BASE_WEIGHT + SENTRY_NODE_BASE_WEIGHT * seniorityBps / BPS_DENOMINATOR;
        if (computedWeight > type(uint128).max) {
            revert SentryNodeWeightOverflow(computedWeight);
        }
        weight = uint128(computedWeight);
    }

    function seniorityBpsAt(uint32 collateralHeight, uint256 currentHeight) public view returns (uint16) {
        uint256 syscoinHeight = nevmStartBlock + currentHeight;
        uint256 seniorityAge = syscoinHeight > collateralHeight ? syscoinHeight - collateralHeight : 0;

        if (seniorityAge >= seniorityHeight2) {
            return seniorityLevel2Bps;
        }
        if (seniorityAge >= seniorityHeight1) {
            return seniorityLevel1Bps;
        }
        return 0;
    }
}
