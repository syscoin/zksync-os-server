// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/access/AccessControlUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";

interface IZkSysSentryNodeReceiver {
    function onSentryNodeStatusChange(address account, uint32 oldCollateralHeight, uint32 newCollateralHeight) external;
}

/// @title ZkSysMembershipRegistry
/// @notice L2 mirror of NEVM membership facts used by zkSYS issuance.
contract ZkSysMembershipRegistry is Initializable, AccessControlUpgradeable {
    struct Member {
        uint32 sentryNodeCollateralHeight;
    }

    struct SentryNodeUpdate {
        address account;
        uint32 sentryNodeCollateralHeight;
    }

    error InvalidAddress();
    error L1RegistryBridgeAlreadySet(address currentL1RegistryBridge);
    error UnauthorizedL1RegistryBridge(address caller);
    error SentryNodeReceiverAlreadySet(address currentReceiver);
    error SentryNodeReceiverNotSet();

    mapping(address account => Member member) private _members;
    mapping(address account => uint256 indexPlusOne) private _activeSentryNodeIndexPlusOne;
    address[] private _activeSentryNodes;

    IZkSysSentryNodeReceiver public sentryNodeReceiver;
    address public l1RegistryBridge;
    address public aliasedL1RegistryBridge;
    uint256[46] private __gap;

    event L1RegistryBridgeUpdated(address indexed l1RegistryBridge, address indexed aliasedL1RegistryBridge);
    event SentryNodeReceiverUpdated(address indexed receiver);
    event SentryNodeCollateralHeightUpdated(
        address indexed account, uint32 oldSentryNodeCollateralHeight, uint32 newSentryNodeCollateralHeight
    );
    event SentryNodeMembershipUpdated(address indexed account, bool active);

    constructor() {
        _disableInitializers();
    }

    function initialize(address admin, address l1RegistryBridge_) external initializer {
        if (admin == address(0)) {
            revert InvalidAddress();
        }

        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        if (l1RegistryBridge_ != address(0)) {
            _setL1RegistryBridge(l1RegistryBridge_);
        }
    }

    function setSentryNodeReceiver(IZkSysSentryNodeReceiver receiver_) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (address(receiver_) == address(0)) {
            revert InvalidAddress();
        }
        IZkSysSentryNodeReceiver currentReceiver = sentryNodeReceiver;
        if (address(currentReceiver) != address(0) && currentReceiver != receiver_) {
            revert SentryNodeReceiverAlreadySet(address(currentReceiver));
        }
        sentryNodeReceiver = receiver_;
        emit SentryNodeReceiverUpdated(address(receiver_));
    }

    function setL1RegistryBridge(address l1RegistryBridge_) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setL1RegistryBridge(l1RegistryBridge_);
    }

    function applyL1SentryNodeUpdates(SentryNodeUpdate[] calldata updates) external {
        if (msg.sender != aliasedL1RegistryBridge) {
            revert UnauthorizedL1RegistryBridge(msg.sender);
        }

        for (uint256 i = 0; i < updates.length; ++i) {
            SentryNodeUpdate calldata update = updates[i];
            _updateSentryNodeCollateralHeight(update.account, update.sentryNodeCollateralHeight);
        }
    }

    function member(address account) external view returns (Member memory) {
        return _members[account];
    }

    function isActiveSentryNode(address account) external view returns (bool) {
        return _activeSentryNodeIndexPlusOne[account] != 0;
    }

    function activeSentryNodeCount() external view returns (uint256) {
        return _activeSentryNodes.length;
    }

    function activeSentryNodeAt(uint256 index) external view returns (address) {
        return _activeSentryNodes[index];
    }

    function l1ToL2Alias(address l1Address) public pure returns (address) {
        unchecked {
            return address(uint160(l1Address) + uint160(0x1111000000000000000000000000000000001111));
        }
    }

    function _setL1RegistryBridge(address l1RegistryBridge_) private {
        if (l1RegistryBridge_ == address(0)) {
            revert InvalidAddress();
        }
        address currentL1RegistryBridge = l1RegistryBridge;
        if (currentL1RegistryBridge != address(0) && currentL1RegistryBridge != l1RegistryBridge_) {
            revert L1RegistryBridgeAlreadySet(currentL1RegistryBridge);
        }

        l1RegistryBridge = l1RegistryBridge_;
        aliasedL1RegistryBridge = l1ToL2Alias(l1RegistryBridge_);
        emit L1RegistryBridgeUpdated(l1RegistryBridge_, aliasedL1RegistryBridge);
    }

    function _updateSentryNodeCollateralHeight(address account, uint32 sentryNodeCollateralHeight) private {
        if (account == address(0)) {
            revert InvalidAddress();
        }

        Member storage stored = _members[account];
        uint32 oldSentryNodeCollateralHeight = stored.sentryNodeCollateralHeight;
        stored.sentryNodeCollateralHeight = sentryNodeCollateralHeight;

        _setSentryNodeActive(account, sentryNodeCollateralHeight != 0);

        if (oldSentryNodeCollateralHeight != sentryNodeCollateralHeight) {
            IZkSysSentryNodeReceiver receiver = sentryNodeReceiver;
            if (address(receiver) == address(0)) {
                revert SentryNodeReceiverNotSet();
            }
            receiver.onSentryNodeStatusChange(account, oldSentryNodeCollateralHeight, sentryNodeCollateralHeight);
            emit SentryNodeCollateralHeightUpdated(account, oldSentryNodeCollateralHeight, sentryNodeCollateralHeight);
        }
    }

    function _setSentryNodeActive(address account, bool active) private {
        uint256 indexPlusOne = _activeSentryNodeIndexPlusOne[account];
        if (active) {
            if (indexPlusOne == 0) {
                _activeSentryNodes.push(account);
                _activeSentryNodeIndexPlusOne[account] = _activeSentryNodes.length;
                emit SentryNodeMembershipUpdated(account, true);
            }
            return;
        }

        if (indexPlusOne == 0) {
            return;
        }

        uint256 index = indexPlusOne - 1;
        uint256 lastIndex = _activeSentryNodes.length - 1;
        if (index != lastIndex) {
            address movedAccount = _activeSentryNodes[lastIndex];
            _activeSentryNodes[index] = movedAccount;
            _activeSentryNodeIndexPlusOne[movedAccount] = indexPlusOne;
        }
        _activeSentryNodes.pop();
        delete _activeSentryNodeIndexPlusOne[account];
        emit SentryNodeMembershipUpdated(account, false);
    }
}
