// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";
import {ReentrancyGuardUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/security/ReentrancyGuardUpgradeable.sol";

interface IZkSysStakeWeightRegistry {
    function updateStakeWeight(address account, uint256 stakeWeight) external;
}

/// @title ZkSysNativeStakingVault
/// @notice Custodies native SYS stake and mirrors each account's stake balance into zkSYS reward weight.
contract ZkSysNativeStakingVault is Initializable, ReentrancyGuardUpgradeable {
    error InvalidAddress();
    error InvalidAmount();
    error InsufficientStake(uint256 requested, uint256 available);
    error NativeTransferFailed(address receiver, uint256 amount);

    IZkSysStakeWeightRegistry public weightRegistry;
    uint256 public totalStaked;

    mapping(address account => uint256 stake) public stakeOf;
    uint256[48] private __gap;

    event Deposited(address indexed account, address indexed payer, uint256 amount, uint256 newStake);
    event Withdrawn(address indexed account, address indexed receiver, uint256 amount, uint256 newStake);

    constructor() {
        _disableInitializers();
    }

    function initialize(IZkSysStakeWeightRegistry weightRegistry_) external initializer {
        if (address(weightRegistry_) == address(0)) {
            revert InvalidAddress();
        }

        __ReentrancyGuard_init();
        weightRegistry = weightRegistry_;
    }

    receive() external payable nonReentrant {
        _depositFor(msg.sender, msg.value);
    }

    function deposit() external payable nonReentrant {
        _depositFor(msg.sender, msg.value);
    }

    function depositFor(address account) external payable nonReentrant {
        _depositFor(account, msg.value);
    }

    function withdraw(uint256 amount) external nonReentrant {
        _withdrawTo(msg.sender, payable(msg.sender), amount);
    }

    function withdrawTo(address payable receiver, uint256 amount) external nonReentrant {
        _withdrawTo(msg.sender, receiver, amount);
    }

    function _depositFor(address account, uint256 amount) private {
        if (account == address(0)) {
            revert InvalidAddress();
        }
        if (amount == 0) {
            revert InvalidAmount();
        }

        uint256 newStake = stakeOf[account] + amount;
        stakeOf[account] = newStake;
        totalStaked += amount;
        weightRegistry.updateStakeWeight(account, newStake);

        emit Deposited(account, msg.sender, amount, newStake);
    }

    function _withdrawTo(address account, address payable receiver, uint256 amount) private {
        if (receiver == address(0)) {
            revert InvalidAddress();
        }
        if (amount == 0) {
            revert InvalidAmount();
        }

        uint256 currentStake = stakeOf[account];
        if (amount > currentStake) {
            revert InsufficientStake(amount, currentStake);
        }

        uint256 newStake = currentStake - amount;
        stakeOf[account] = newStake;
        totalStaked -= amount;
        weightRegistry.updateStakeWeight(account, newStake);

        (bool success,) = receiver.call{value: amount}("");
        if (!success) {
            revert NativeTransferFailed(receiver, amount);
        }

        emit Withdrawn(account, receiver, amount, newStake);
    }
}
