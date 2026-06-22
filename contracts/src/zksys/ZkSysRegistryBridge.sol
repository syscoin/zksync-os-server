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
    }

    function applyL1SentryNodeUpdates(SentryNodeUpdate[] calldata updates) external;
}

/// @title ZkSysRegistryBridge
/// @notice L1/NEVM adapter that sends verified membership facts to the L2 zkSYS registry.
contract ZkSysRegistryBridge {
    address private constant NEVM_ADDRESS_PRECOMPILE = address(0x62);
    uint256 public constant MAX_BATCH_SIZE = 512;

    error DuplicateAccount(address account);
    error EmptyBatch();
    error InvalidAddress();
    error InvalidBatchSize(uint256 batchSize);
    error NevmCollateralHeightOverflow(address account, uint256 collateralHeight);
    error NevmLookupFailed(address account);

    IL1BridgehubMinimal public immutable bridgehub;
    uint256 public immutable zksysChainId;
    address public immutable l2Registry;

    event RegistryUpdatesRequested(bytes32 indexed canonicalTxHash, uint256 updateCount);

    constructor(IL1BridgehubMinimal bridgehub_, uint256 zksysChainId_, address l2Registry_) {
        if (address(bridgehub_) == address(0) || zksysChainId_ == 0 || l2Registry_ == address(0)) {
            revert InvalidAddress();
        }

        bridgehub = bridgehub_;
        zksysChainId = zksysChainId_;
        l2Registry = l2Registry_;
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
            updates[i] = IZkSysMembershipRegistryL2.SentryNodeUpdate({
                account: account,
                sentryNodeCollateralHeight: nevmCollateralHeight(account)
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
}
