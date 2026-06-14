// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IEntryPoint, IPaymaster, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {PaymasterCore} from "@openzeppelin/community-contracts/account/paymaster/PaymasterCore.sol";
import {Test} from "forge-std/Test.sol";
import {PaliFixedRateTokenPaymaster} from "../src/passkey/PaliFixedRateTokenPaymaster.sol";
import {TestERC20} from "../src/TestERC20.sol";

contract MockEntryPoint {
    mapping(address => uint256) public balanceOf;

    receive() external payable {}

    function depositTo(address account) external payable {
        balanceOf[account] += msg.value;
    }

    function withdrawTo(address payable withdrawAddress, uint256 withdrawAmount) external {
        address account = msg.sender;
        require(balanceOf[account] >= withdrawAmount, "insufficient deposit");
        balanceOf[account] -= withdrawAmount;
        withdrawAddress.transfer(withdrawAmount);
    }

    function validate(PaliFixedRateTokenPaymaster paymaster, PackedUserOperation calldata userOp, uint256 maxCost)
        external
        returns (bytes memory context, uint256 validationData)
    {
        return paymaster.validatePaymasterUserOp(userOp, bytes32(0), maxCost);
    }

    function settle(
        PaliFixedRateTokenPaymaster paymaster,
        IPaymaster.PostOpMode mode,
        bytes calldata context,
        uint256 actualGasCost,
        uint256 actualUserOpFeePerGas
    ) external {
        paymaster.postOp(mode, context, actualGasCost, actualUserOpFeePerGas);
    }
}

contract PaliFixedRateTokenPaymasterTest is Test {
    MockEntryPoint private entryPoint;
    PaliFixedRateTokenPaymaster private paymaster;
    TestERC20 private token;

    address private owner = address(0xA11CE);
    address private sender = address(0xB0B);
    address private treasury = address(0xCAFE);

    function setUp() public {
        entryPoint = new MockEntryPoint();
        token = new TestERC20(0, "zkSYS", "zkSYS");
        paymaster =
            new PaliFixedRateTokenPaymaster(IEntryPoint(address(entryPoint)), IERC20(address(token)), treasury, owner);
        token.mint(sender, 1_000 ether);
    }

    function testValidatePrechargesMaxCostAndPostOpRefundsExcess() public {
        uint256 maxCost = 10 ether;
        uint256 actualCost = 6 ether;
        uint256 actualFeePerGas = 1 gwei;
        uint256 postOpCost = 45_000 * actualFeePerGas;
        PackedUserOperation memory userOp = _userOp();

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertEq(token.balanceOf(sender), 990 ether);
        assertEq(token.balanceOf(address(paymaster)), maxCost);

        entryPoint.settle(paymaster, IPaymaster.PostOpMode.opSucceeded, context, actualCost, actualFeePerGas);

        assertEq(token.balanceOf(sender), 1_000 ether - actualCost - postOpCost);
        assertEq(token.balanceOf(treasury), actualCost + postOpCost);
        assertEq(token.balanceOf(address(paymaster)), 0);
    }

    function testValidateRejectsExcessivePostOpGasLimit() public {
        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOpWithPostOpGasLimit(45_001);

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 1);
        assertEq(context.length, 0);
        assertEq(token.balanceOf(sender), 1_000 ether);
        assertEq(token.balanceOf(address(paymaster)), 0);
    }

    function testValidateRejectsCallsOutsideEntryPoint() public {
        PackedUserOperation memory userOp = _userOp();

        vm.expectRevert(abi.encodeWithSelector(PaymasterCore.PaymasterUnauthorized.selector, address(this)));
        paymaster.validatePaymasterUserOp(userOp, bytes32(0), 1 ether);
    }

    function testReceiveDepositsNativeFundsToEntryPoint() public {
        vm.deal(address(this), 2 ether);

        (bool success,) = address(paymaster).call{value: 2 ether}("");

        assertTrue(success);
        assertEq(entryPoint.balanceOf(address(paymaster)), 2 ether);
    }

    function testOwnerCanWithdrawEntryPointDeposit() public {
        vm.deal(address(this), 2 ether);
        paymaster.deposit{value: 2 ether}();

        uint256 beforeBalance = owner.balance;
        vm.prank(owner);
        paymaster.withdrawDepositTo(payable(owner), 1 ether);

        assertEq(owner.balance, beforeBalance + 1 ether);
        assertEq(entryPoint.balanceOf(address(paymaster)), 1 ether);
    }

    function testOwnerCanWithdrawCollectedTokens() public {
        token.mint(address(paymaster), 5 ether);

        vm.prank(owner);
        paymaster.withdrawTokens(IERC20(address(token)), owner, type(uint256).max);

        assertEq(token.balanceOf(owner), 5 ether);
        assertEq(token.balanceOf(address(paymaster)), 0);
    }

    function testOnlyOwnerCanChangeTreasury() public {
        address nextTreasury = address(0xBEEF);

        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, address(this)));
        paymaster.setTreasury(nextTreasury);

        vm.prank(owner);
        paymaster.setTreasury(nextTreasury);

        assertEq(paymaster.treasury(), nextTreasury);
    }

    function _userOp() private view returns (PackedUserOperation memory userOp) {
        userOp = _userOpWithPostOpGasLimit(45_000);
    }

    function _userOpWithPostOpGasLimit(uint128 paymasterPostOpGasLimit)
        private
        view
        returns (PackedUserOperation memory userOp)
    {
        userOp.sender = sender;
        userOp.paymasterAndData = abi.encodePacked(address(paymaster), uint128(120_000), paymasterPostOpGasLimit);
    }
}
