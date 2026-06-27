// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ERC4337Utils} from "@openzeppelin/contracts/account/utils/draft-ERC4337Utils.sol";
import {IEntryPoint, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {PaymasterERC20} from "@openzeppelin/community-contracts/account/paymaster/PaymasterERC20.sol";

interface IERC20Burnable is IERC20 {
    function burn(address from, uint256 amount) external returns (bool);
}

/// @title PaliFixedRateTokenPaymaster
/// @notice ERC-4337 paymaster that charges one ERC-20 token unit per one native gas unit.
/// @dev Uses OZ Community Contracts' ERC-20 paymaster base and pins the token price to 1:1.
contract PaliFixedRateTokenPaymaster is PaymasterERC20, Ownable {
    using ERC4337Utils for PackedUserOperation;
    using SafeERC20 for IERC20;

    // EntryPoint charges a 10% unused-gas penalty when the 80k stipend leaves ~50k unused.
    uint256 private constant POST_OP_COST = 35_000;
    uint256 private constant MIN_PAYMASTER_POST_OP_GAS_LIMIT = POST_OP_COST;
    uint256 private constant MAX_PAYMASTER_POST_OP_GAS_LIMIT = 80_000;
    uint256 private constant PAYMASTER_POST_OP_GAS_LIMIT_OFFSET = 36;
    uint256 private constant PAYMASTER_POST_OP_GAS_LIMIT_END = 52;

    error InvalidAddress();
    error NativeWithdrawalsDisabled();
    error TokenWithdrawalsDisabled();

    IEntryPoint private immutable _entryPoint;
    IERC20Burnable public immutable token;

    constructor(IEntryPoint entryPoint_, IERC20Burnable token_, address owner_) Ownable(owner_) {
        if (
            address(entryPoint_) == address(0) || address(token_) == address(0) || owner_ == address(0)
                || address(entryPoint_).code.length == 0 || address(token_).code.length == 0
        ) {
            revert InvalidAddress();
        }
        _entryPoint = entryPoint_;
        token = token_;
    }

    receive() external payable {
        _depositNativeBalance();
    }

    function entryPoint() public view override returns (IEntryPoint) {
        return _entryPoint;
    }

    function deposit() public payable override {
        _depositNativeBalance();
    }

    function withdrawDepositTo(address payable, uint256) external pure {
        revert NativeWithdrawalsDisabled();
    }

    function addStake(uint32 unstakeDelaySec) public payable override onlyOwner {
        super.addStake(unstakeDelaySec);
    }

    function withdraw(address payable, uint256) public pure override {
        revert NativeWithdrawalsDisabled();
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
        if (
            paymasterPostOpGasLimit < MIN_PAYMASTER_POST_OP_GAS_LIMIT
                || paymasterPostOpGasLimit > MAX_PAYMASTER_POST_OP_GAS_LIMIT
        ) {
            return (ERC4337Utils.SIG_VALIDATION_FAILED, IERC20(address(0)), 0);
        }

        return (0, token, _tokenPriceDenominator());
    }

    function _prefund(
        PackedUserOperation calldata userOp,
        bytes32 userOpHash,
        IERC20 paymentToken,
        uint256 tokenPrice,
        address prefunder_,
        uint256 maxCost
    )
        internal
        override
        returns (bool prefunded, uint256 prefundAmount, address prefunder, bytes memory prefundContext)
    {
        if (userOp.paymasterAndData.length < PAYMASTER_POST_OP_GAS_LIMIT_END) {
            return (false, 0, prefunder_, "");
        }

        uint128 paymasterPostOpGasLimit = uint128(
            bytes16(userOp.paymasterAndData[PAYMASTER_POST_OP_GAS_LIMIT_OFFSET:PAYMASTER_POST_OP_GAS_LIMIT_END])
        );
        if (
            paymasterPostOpGasLimit < MIN_PAYMASTER_POST_OP_GAS_LIMIT
                || paymasterPostOpGasLimit > MAX_PAYMASTER_POST_OP_GAS_LIMIT
        ) {
            return (false, 0, prefunder_, "");
        }

        uint256 reservedPostOpCost = paymasterPostOpGasLimit * userOp.maxFeePerGas();
        if (reservedPostOpCost > maxCost) {
            return (false, 0, prefunder_, "");
        }
        uint256 adjustedMaxCost = maxCost - reservedPostOpCost;

        return super._prefund(userOp, userOpHash, paymentToken, tokenPrice, prefunder_, adjustedMaxCost);
    }

    function _refund(
        IERC20 paymentToken,
        uint256 tokenPrice,
        uint256 actualGasCost,
        uint256 actualUserOpFeePerGas,
        address prefunder,
        uint256 prefundAmount,
        bytes calldata
    ) internal override returns (bool refunded, uint256 actualAmount) {
        actualAmount = _erc20Cost(actualGasCost, actualUserOpFeePerGas, tokenPrice);
        refunded = paymentToken.trySafeTransfer(prefunder, prefundAmount - actualAmount);
        if (!refunded) {
            return (false, actualAmount);
        }

        refunded = token.burn(address(this), actualAmount);
    }

    function withdrawTokens(IERC20, address, uint256) public pure override {
        revert TokenWithdrawalsDisabled();
    }

    function _authorizeWithdraw() internal override onlyOwner {}

    function _postOpCost() internal pure override returns (uint256) {
        return POST_OP_COST;
    }

    function _depositNativeBalance() private {
        uint256 amount = address(this).balance;
        if (amount != 0) {
            _entryPoint.depositTo{value: amount}(address(this));
        }
    }
}
