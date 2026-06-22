// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IEntryPoint, IPaymaster, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {PaymasterCore} from "@openzeppelin/community-contracts/account/paymaster/PaymasterCore.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20Burnable, PaliFixedRateTokenPaymaster} from "contracts/src/pali/PaliFixedRateTokenPaymaster.sol";
import {TestERC20} from "../src/TestERC20.sol";

contract MockEntryPoint {
    mapping(address => uint256) public balanceOf;
    mapping(address => uint256) public stakeOf;
    mapping(address => uint32) public unstakeDelayOf;
    mapping(address => bool) public stakeUnlocked;

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

    function addStake(uint32 unstakeDelaySec) external payable {
        stakeOf[msg.sender] += msg.value;
        if (unstakeDelaySec > unstakeDelayOf[msg.sender]) {
            unstakeDelayOf[msg.sender] = unstakeDelaySec;
        }
    }

    function unlockStake() external {
        require(stakeOf[msg.sender] != 0, "no stake");
        stakeUnlocked[msg.sender] = true;
    }

    function withdrawStake(address payable withdrawAddress) external {
        require(stakeUnlocked[msg.sender], "stake locked");
        uint256 amount = stakeOf[msg.sender];
        stakeOf[msg.sender] = 0;
        withdrawAddress.transfer(amount);
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

    function setUp() public {
        entryPoint = new MockEntryPoint();
        token = new TestERC20(0, "zkSYS", "zkSYS");
        paymaster =
            new PaliFixedRateTokenPaymaster(IEntryPoint(address(entryPoint)), IERC20Burnable(address(token)), owner);
        token.mint(sender, 1_000 ether);
    }

    function testValidatePrechargesMaxCostAndPostOpRefundsExcess() public {
        uint256 maxCost = 10 ether;
        uint256 actualCost = 6 ether;
        uint256 actualFeePerGas = 1 gwei;
        uint256 postOpCost = 35_000 * actualFeePerGas;
        PackedUserOperation memory userOp = _userOp();

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertEq(token.balanceOf(sender), 990 ether);
        assertEq(token.balanceOf(address(paymaster)), maxCost);

        entryPoint.settle(paymaster, IPaymaster.PostOpMode.opSucceeded, context, actualCost, actualFeePerGas);

        assertEq(token.balanceOf(sender), 1_000 ether - actualCost - postOpCost);
        assertEq(token.balanceOf(address(paymaster)), 0);
        assertEq(token.totalSupply(), 1_000 ether - actualCost - postOpCost);
    }

    function testValidateRejectsExcessivePostOpGasLimit() public {
        _assertPostOpGasLimitRejected(80_001);
    }

    function testValidateRejectsUndersizedPostOpGasLimit() public {
        _assertPostOpGasLimitRejected(34_999);
    }

    function testValidateAcceptsBoundedLargerPostOpGasLimit() public {
        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOpWithPostOpGasLimit(80_000);

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertGt(context.length, 0);
        assertEq(token.balanceOf(sender), 990 ether);
        assertEq(token.balanceOf(address(paymaster)), maxCost);
    }

    function testValidateDoesNotPrechargeExtraPostOpHeadroom() public {
        uint256 maxFeePerGas = 1 gwei;
        uint256 maxCost = 10 ether;
        uint256 extraPostOpHeadroomCost = 45_000 * maxFeePerGas;
        PackedUserOperation memory userOp = _userOpWithPostOpGasLimit(80_000);
        userOp.gasFees = bytes32(abi.encodePacked(uint128(0), uint128(maxFeePerGas)));

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertGt(context.length, 0);
        assertEq(token.balanceOf(sender), 1_000 ether - maxCost + extraPostOpHeadroomCost);
        assertEq(token.balanceOf(address(paymaster)), maxCost - extraPostOpHeadroomCost);
    }

    function _assertPostOpGasLimitRejected(uint128 paymasterPostOpGasLimit) private {
        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOpWithPostOpGasLimit(paymasterPostOpGasLimit);

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

    function testNativeDepositCannotBeWithdrawn() public {
        vm.deal(address(this), 2 ether);
        paymaster.deposit{value: 2 ether}();

        vm.prank(owner);
        vm.expectRevert(PaliFixedRateTokenPaymaster.NativeWithdrawalsDisabled.selector);
        paymaster.withdrawDepositTo(payable(owner), 1 ether);

        vm.prank(owner);
        vm.expectRevert(PaliFixedRateTokenPaymaster.NativeWithdrawalsDisabled.selector);
        paymaster.withdraw(payable(owner), 1 ether);

        assertEq(owner.balance, 0);
        assertEq(entryPoint.balanceOf(address(paymaster)), 2 ether);
    }

    function testOwnerCanManageStakeBond() public {
        vm.deal(owner, 2 ether);

        vm.prank(owner);
        paymaster.addStake{value: 2 ether}(1 days);

        vm.prank(owner);
        paymaster.unlockStake();

        uint256 beforeBalance = owner.balance;
        vm.prank(owner);
        paymaster.withdrawStake(payable(owner));

        assertEq(owner.balance, beforeBalance + 2 ether);
        assertEq(entryPoint.stakeOf(address(paymaster)), 0);
    }

    function testOnlyOwnerCanAddStake() public {
        vm.deal(owner, 2 ether);
        vm.deal(sender, 1 ether);

        vm.prank(sender);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, sender));
        paymaster.addStake{value: 1 ether}(365 days);

        vm.prank(owner);
        paymaster.addStake{value: 2 ether}(1 days);

        assertEq(entryPoint.stakeOf(address(paymaster)), 2 ether);
        assertEq(entryPoint.unstakeDelayOf(address(paymaster)), 1 days);
    }

    function testCollectedTokensCannotBeWithdrawnByOwner() public {
        token.mint(address(paymaster), 5 ether);

        vm.prank(owner);
        vm.expectRevert(PaliFixedRateTokenPaymaster.TokenWithdrawalsDisabled.selector);
        paymaster.withdrawTokens(IERC20(address(token)), owner, type(uint256).max);

        assertEq(token.balanceOf(owner), 0);
        assertEq(token.balanceOf(address(paymaster)), 5 ether);
    }

    function _userOp() private view returns (PackedUserOperation memory userOp) {
        userOp = _userOpWithPostOpGasLimit(35_000);
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
