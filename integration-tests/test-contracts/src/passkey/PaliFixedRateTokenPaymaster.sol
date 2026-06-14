// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ERC4337Utils} from "@openzeppelin/contracts/account/utils/draft-ERC4337Utils.sol";
import {IEntryPoint, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {PaymasterERC20} from "@openzeppelin/community-contracts/account/paymaster/PaymasterERC20.sol";

/// @title PaliFixedRateTokenPaymaster
/// @notice ERC-4337 paymaster that charges one ERC-20 token unit per one native gas unit.
/// @dev Uses OZ Community Contracts' ERC-20 paymaster base and pins the token price to 1:1.
contract PaliFixedRateTokenPaymaster is PaymasterERC20, Ownable {
    uint256 private constant POST_OP_COST = 30_000;
    uint256 private constant PAYMASTER_POST_OP_GAS_LIMIT_OFFSET = 36;
    uint256 private constant PAYMASTER_POST_OP_GAS_LIMIT_END = 52;

    error InvalidAddress();

    event TreasuryChanged(address indexed previousTreasury, address indexed newTreasury);

    IEntryPoint private immutable _entryPoint;
    IERC20 public immutable token;

    address public treasury;

    constructor(IEntryPoint entryPoint_, IERC20 token_, address treasury_, address owner_) Ownable(owner_) {
        if (
            address(entryPoint_) == address(0) || address(token_) == address(0) || treasury_ == address(0)
                || owner_ == address(0)
        ) {
            revert InvalidAddress();
        }
        _entryPoint = entryPoint_;
        token = token_;
        treasury = treasury_;
        emit TreasuryChanged(address(0), treasury_);
    }

    receive() external payable {
        deposit();
    }

    function entryPoint() public view override returns (IEntryPoint) {
        return _entryPoint;
    }

    function withdrawDepositTo(address payable withdrawAddress, uint256 withdrawAmount) external onlyOwner {
        if (withdrawAddress == address(0)) {
            revert InvalidAddress();
        }
        withdraw(withdrawAddress, withdrawAmount);
    }

    function addStake(uint32 unstakeDelaySec) public payable override onlyOwner {
        super.addStake(unstakeDelaySec);
    }

    function setTreasury(address newTreasury) external onlyOwner {
        if (newTreasury == address(0)) {
            revert InvalidAddress();
        }
        emit TreasuryChanged(treasury, newTreasury);
        treasury = newTreasury;
    }

    function _fetchDetails(PackedUserOperation calldata userOp, bytes32)
        internal
        view
        override
        returns (uint256 validationData, IERC20 paymentToken, uint256 tokenPrice)
    {
        if (userOp.paymasterAndData.length < PAYMASTER_POST_OP_GAS_LIMIT_END) {
            return (ERC4337Utils.SIG_VALIDATION_FAILED, IERC20(address(0)), 0);
        }

        uint128 paymasterPostOpGasLimit = uint128(
            bytes16(userOp.paymasterAndData[PAYMASTER_POST_OP_GAS_LIMIT_OFFSET:PAYMASTER_POST_OP_GAS_LIMIT_END])
        );
        if (paymasterPostOpGasLimit != POST_OP_COST) {
            return (ERC4337Utils.SIG_VALIDATION_FAILED, IERC20(address(0)), 0);
        }

        return (0, token, _tokenPriceDenominator());
    }

    function _authorizeWithdraw() internal override onlyOwner {}

    function _postOpCost() internal pure override returns (uint256) {
        return POST_OP_COST;
    }
}
