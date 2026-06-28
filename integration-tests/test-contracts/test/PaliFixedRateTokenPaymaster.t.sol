// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IEntryPoint, IPaymaster, PackedUserOperation} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {PaymasterCore} from "@openzeppelin/community-contracts/account/paymaster/PaymasterCore.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20Burnable, PaliFixedRateTokenPaymaster} from "contracts/src/pali/PaliFixedRateTokenPaymaster.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {TestERC20} from "../src/TestERC20.sol";

contract MockEntryPoint {
    mapping(address => uint256) public balanceOf;
    mapping(address => uint256) public stakeOf;
    mapping(address => uint32) public unstakeDelayOf;
    mapping(address => bool) public stakeUnlocked;
    address public SYSCOIN_SPONSORED_PAYMASTER;
    bool public ignoreBind;

    receive() external payable {}

    function setSyscoinSponsoredPaymaster(address paymaster) external {
        SYSCOIN_SPONSORED_PAYMASTER = paymaster;
    }

    function setIgnoreBind(bool ignoreBind_) external {
        ignoreBind = ignoreBind_;
    }

    function bindSyscoinSponsoredPaymaster(address paymaster) external {
        if (!ignoreBind) {
            SYSCOIN_SPONSORED_PAYMASTER = paymaster;
        }
    }

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

    function settleGasUsed(
        PaliFixedRateTokenPaymaster paymaster,
        IPaymaster.PostOpMode mode,
        bytes calldata context,
        uint256 actualGasCost,
        uint256 actualUserOpFeePerGas
    ) external returns (uint256 gasUsed) {
        uint256 beforeGas = gasleft();
        paymaster.postOp(mode, context, actualGasCost, actualUserOpFeePerGas);
        gasUsed = beforeGas - gasleft();
    }
}

contract PaliFixedRateTokenPaymasterTest is Test {
    uint256 private constant POST_OP_COST = 35_000;
    uint128 private constant MAX_PAYMASTER_POST_OP_GAS_LIMIT = 80_000;
    uint256 private constant MAX_PRE_VERIFICATION_GAS = 250_000;
    uint256 private constant MAX_SYNTHETIC_SPONSORED_GAS = 1_000_000;
    uint256 private constant MAX_SPONSORED_NATIVE_PREFUND = 10 ether;
    uint256 private constant UNUSED_GAS_PENALTY_PERCENT = 10;
    uint256 private constant PENALTY_GAS_THRESHOLD = 40_000;
    uint256 private constant TARGET_ENTRY_POINT_RESERVE = 100 ether;
    address payable private constant SYSCOIN_UNSPENDABLE_NATIVE_SINK =
        payable(0x0000000000000000000000000000000000005953);

    MockEntryPoint private entryPoint;
    PaliFixedRateTokenPaymaster private paymaster;
    TestERC20 private token;

    address private owner = address(0xA11CE);
    address private sender = address(0xB0B);

    function setUp() public {
        entryPoint = new MockEntryPoint();
        token = new TestERC20(0, "zkSYS", "zkSYS");
        paymaster = _deployPaymaster(address(token), TARGET_ENTRY_POINT_RESERVE);
        token.mint(sender, 1_000 ether);
    }

    function testConstructorRejectsEntryPointWithoutCode() public {
        vm.expectRevert(PaliFixedRateTokenPaymaster.InvalidAddress.selector);
        new PaliFixedRateTokenPaymaster(
            IEntryPoint(address(0xE0A)), IERC20Burnable(address(token)), owner, TARGET_ENTRY_POINT_RESERVE
        );
    }

    function testConstructorRejectsTokenWithoutCode() public {
        vm.expectRevert(PaliFixedRateTokenPaymaster.InvalidAddress.selector);
        new PaliFixedRateTokenPaymaster(
            IEntryPoint(address(entryPoint)), IERC20Burnable(address(0xE0A)), owner, TARGET_ENTRY_POINT_RESERVE
        );
    }

    function testValidateRejectsUnboundSyscoinEntryPoint() public {
        entryPoint.setIgnoreBind(true);
        paymaster = _deployPaymasterWithoutBinding(address(token), TARGET_ENTRY_POINT_RESERVE);

        _assertUserOpRejected(_userOp());
    }

    function testValidateRejectsEntryPointWithDifferentSponsoredPaymaster() public {
        entryPoint.setIgnoreBind(true);
        paymaster = _deployPaymasterWithoutBinding(address(token), TARGET_ENTRY_POINT_RESERVE);
        entryPoint.setSyscoinSponsoredPaymaster(address(0xBAD));

        _assertUserOpRejected(_userOp());
    }

    function testConstructorRejectsZeroEntryPointReserveCap() public {
        vm.expectRevert(PaliFixedRateTokenPaymaster.InvalidEntryPointReserveCap.selector);
        new PaliFixedRateTokenPaymaster(IEntryPoint(address(entryPoint)), IERC20Burnable(address(token)), owner, 0);
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

    function testProductionZkSysTokenRequiresBurnerRoleForSettlement() public {
        SyscoinZKSYSToken zkToken = _deployZkSysToken();
        paymaster = _deployPaymaster(address(zkToken), TARGET_ENTRY_POINT_RESERVE);
        zkToken.grantRole(zkToken.MINTER_ROLE(), address(this));
        zkToken.mint(sender, 1_000 ether);

        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOp();

        vm.prank(sender);
        zkToken.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);
        assertEq(validationData, 0);
        assertEq(zkToken.balanceOf(sender), 990 ether);
        assertEq(zkToken.balanceOf(address(paymaster)), maxCost);

        vm.expectRevert();
        entryPoint.settle(paymaster, IPaymaster.PostOpMode.opSucceeded, context, 6 ether, 1 gwei);

        assertEq(zkToken.balanceOf(sender), 990 ether);
        assertEq(zkToken.balanceOf(address(paymaster)), maxCost);
        assertEq(zkToken.totalSupply(), 1_000 ether);
    }

    function testProductionZkSysTokenBurnerRoleBurnsOnlyPaymasterBalanceOnSettlement() public {
        SyscoinZKSYSToken zkToken = _deployZkSysToken();
        paymaster = _deployPaymaster(address(zkToken), TARGET_ENTRY_POINT_RESERVE);
        zkToken.grantRole(zkToken.MINTER_ROLE(), address(this));
        zkToken.grantRole(zkToken.BURNER_ROLE(), address(paymaster));
        zkToken.mint(sender, 1_000 ether);

        uint256 maxCost = 10 ether;
        uint256 actualCost = 6 ether;
        uint256 actualFeePerGas = 1 gwei;
        uint256 postOpCost = 35_000 * actualFeePerGas;
        PackedUserOperation memory userOp = _userOp();

        vm.prank(sender);
        zkToken.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);
        assertEq(validationData, 0);

        entryPoint.settle(paymaster, IPaymaster.PostOpMode.opSucceeded, context, actualCost, actualFeePerGas);

        assertEq(zkToken.balanceOf(sender), 1_000 ether - actualCost - postOpCost);
        assertEq(zkToken.balanceOf(address(paymaster)), 0);
        assertEq(zkToken.totalSupply(), 1_000 ether - actualCost - postOpCost);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimit() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opSucceeded, false, false, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWhenReverted() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opReverted, false, false, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithDelegatedSender() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opSucceeded, true, false, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithDelegatedSenderWhenReverted() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opReverted, true, false, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithDelegatedPaymaster() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opSucceeded, false, true, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithDelegatedPaymasterWhenReverted() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opReverted, false, true, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithDelegatedSenderAndPaymaster() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opSucceeded, true, true, false);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithRepeatedCheckpoints() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opSucceeded, true, true, true);
    }

    function testProductionZkSysPostOpGasEstimateFitsConfiguredLimitWithRepeatedCheckpointsWhenReverted() public {
        _assertProductionZkSysPostOpGasFitsLimit(IPaymaster.PostOpMode.opReverted, true, true, true);
    }

    function _assertProductionZkSysPostOpGasFitsLimit(
        IPaymaster.PostOpMode mode,
        bool delegateSender,
        bool delegatePaymaster,
        bool repeatOperation
    ) private {
        (SyscoinZKSYSToken zkToken, bytes memory context, uint256 actualCost, uint256 actualFeePerGas) =
            _validatedProductionZkSysUserOp(delegateSender, delegatePaymaster);

        if (repeatOperation) {
            entryPoint.settle(paymaster, mode, context, actualCost, actualFeePerGas);
            (context, actualCost, actualFeePerGas) = _validateProductionZkSysUserOp(zkToken);
        }

        uint256 gasUsed = entryPoint.settleGasUsed(paymaster, mode, context, actualCost, actualFeePerGas);

        assertLe(gasUsed, MAX_PAYMASTER_POST_OP_GAS_LIMIT);
        assertLe(gasUsed + _unusedGasPenalty(gasUsed, MAX_PAYMASTER_POST_OP_GAS_LIMIT), POST_OP_COST);
        assertEq(zkToken.balanceOf(address(paymaster)), 0);
    }

    function testValidateRejectsUndersizedPostOpGasLimit() public {
        _assertPostOpGasLimitRejected(34_999);
    }

    function testValidateRejectsExcessivePostOpGasLimit() public {
        _assertPostOpGasLimitRejected(MAX_PAYMASTER_POST_OP_GAS_LIMIT + 1);
    }

    function testValidateAcceptsBoundedLargerPostOpGasLimit() public {
        uint256 maxFeePerGas = 1 gwei;
        uint256 maxCost = 10 ether;
        uint256 extraPostOpHeadroomCost = 45_000 * maxFeePerGas;
        PackedUserOperation memory userOp = _userOpWithPostOpGasLimit(80_000);

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertGt(context.length, 0);
        assertEq(token.balanceOf(sender), 1_000 ether - maxCost + extraPostOpHeadroomCost);
        assertEq(token.balanceOf(address(paymaster)), maxCost - extraPostOpHeadroomCost);
    }

    function testValidateAcceptsPriorityFeeForWalletCompatibility() public {
        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOp();
        userOp.gasFees = bytes32(abi.encodePacked(uint128(1), uint128(1 gwei)));

        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertGt(context.length, 0);
    }

    function testValidateAcceptsHighPreVerificationGasForWalletCompatibility() public {
        PackedUserOperation memory userOp = _userOp();
        userOp.preVerificationGas = MAX_PRE_VERIFICATION_GAS;

        _assertUserOpAccepted(userOp, 10 ether);
    }

    function testValidateRejectsExcessivePreVerificationGas() public {
        PackedUserOperation memory userOp = _userOp();
        userOp.preVerificationGas = MAX_PRE_VERIFICATION_GAS + 1;

        _assertUserOpRejected(userOp);
    }

    function testValidateAcceptsHighVerificationGasLimitForWalletCompatibility() public {
        PackedUserOperation memory userOp = _userOp();

        userOp.accountGasLimits = _accountGasLimits(5_000_000, 250_000);
        _assertUserOpAccepted(userOp, 10 ether);
    }

    function testValidateAcceptsHighCallGasLimitForWalletCompatibility() public {
        PackedUserOperation memory userOp = _userOp();

        userOp.preVerificationGas = MAX_PRE_VERIFICATION_GAS;
        userOp.accountGasLimits = _accountGasLimits(200_000, 7_000_000);
        _assertUserOpAccepted(userOp, 10 ether);
    }

    function testValidateRejectsExcessiveSyntheticSponsoredGas() public {
        PackedUserOperation memory userOp = _userOp();
        userOp.accountGasLimits = _accountGasLimits(200_000, 10_000_000);

        _assertUserOpRejected(userOp);
    }

    function testValidateAcceptsHighPaymasterVerificationGasLimitForWalletCompatibility() public {
        uint256 maxCost = 10 ether;
        PackedUserOperation memory userOp = _userOpWithPaymasterGasLimits(1_000_000, MAX_PAYMASTER_POST_OP_GAS_LIMIT);

        _assertUserOpAccepted(userOp, maxCost);
    }

    function testValidateAcceptsMaxSponsoredNativePrefundBoundary() public {
        PackedUserOperation memory userOp = _userOp();

        _assertUserOpAccepted(userOp, MAX_SPONSORED_NATIVE_PREFUND);
    }

    function testValidateRejectsExcessiveSponsoredNativePrefund() public {
        PackedUserOperation memory userOp = _userOp();

        _assertUserOpRejected(userOp, MAX_SPONSORED_NATIVE_PREFUND + 1);
    }

    function testSponsoredGasPolicyCostModelAtThreeHundredGwei() public pure {
        uint256 expectedMaxFeePerGas = 300 gwei;
        uint256 maxCallGasAtBoundary =
            (MAX_SYNTHETIC_SPONSORED_GAS - MAX_PRE_VERIFICATION_GAS - MAX_PAYMASTER_POST_OP_GAS_LIMIT / 10) * 10;

        assertEq(maxCallGasAtBoundary, 7_420_000);
        assertEq(MAX_SYNTHETIC_SPONSORED_GAS * expectedMaxFeePerGas, 0.3 ether);
        assertEq(MAX_SPONSORED_NATIVE_PREFUND / expectedMaxFeePerGas, 33_333_333);
        assertEq(_ceilDiv(100_000 ether, MAX_SPONSORED_NATIVE_PREFUND), 10_000);
        assertEq(_ceilDiv(1_000_000 ether, MAX_SPONSORED_NATIVE_PREFUND), 100_000);
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

    function _assertUserOpAccepted(PackedUserOperation memory userOp, uint256 maxCost) private {
        vm.prank(sender);
        token.approve(address(paymaster), maxCost);

        (bytes memory context, uint256 validationData) = entryPoint.validate(paymaster, userOp, maxCost);

        assertEq(validationData, 0);
        assertGt(context.length, 0);
    }

    function _assertUserOpRejected(PackedUserOperation memory userOp) private {
        _assertUserOpRejected(userOp, 10 ether);
    }

    function _assertUserOpRejected(PackedUserOperation memory userOp, uint256 maxCost) private {
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

    function testDepositSyncsDirectlyCreditedNativeBalanceToEntryPoint() public {
        vm.deal(address(paymaster), 3 ether);

        paymaster.deposit();

        assertEq(address(paymaster).balance, 0);
        assertEq(entryPoint.balanceOf(address(paymaster)), 3 ether);
    }

    function testReceiveCapsEntryPointReserveAndSendsExcessToSink() public {
        PaliFixedRateTokenPaymaster cappedPaymaster = _deployPaymaster(address(token), 1 ether);
        uint256 sinkBalanceBefore = SYSCOIN_UNSPENDABLE_NATIVE_SINK.balance;
        vm.deal(address(this), 3 ether);

        (bool success,) = address(cappedPaymaster).call{value: 3 ether}("");

        assertTrue(success);
        assertEq(entryPoint.balanceOf(address(cappedPaymaster)), 1 ether);
        assertEq(address(cappedPaymaster).balance, 0);
        assertEq(SYSCOIN_UNSPENDABLE_NATIVE_SINK.balance, sinkBalanceBefore + 2 ether);
    }

    function testDepositSendsAllNativeToSinkWhenEntryPointReserveIsFull() public {
        PaliFixedRateTokenPaymaster cappedPaymaster = _deployPaymaster(address(token), 1 ether);
        vm.deal(address(cappedPaymaster), 1 ether);
        cappedPaymaster.deposit();
        uint256 sinkBalanceBefore = SYSCOIN_UNSPENDABLE_NATIVE_SINK.balance;
        vm.deal(address(cappedPaymaster), 2 ether);

        cappedPaymaster.deposit();

        assertEq(entryPoint.balanceOf(address(cappedPaymaster)), 1 ether);
        assertEq(address(cappedPaymaster).balance, 0);
        assertEq(SYSCOIN_UNSPENDABLE_NATIVE_SINK.balance, sinkBalanceBefore + 2 ether);
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
        userOp = _userOpWithPaymasterGasLimits(120_000, paymasterPostOpGasLimit);
    }

    function _userOpWithPaymasterGasLimits(uint128 paymasterVerificationGasLimit, uint128 paymasterPostOpGasLimit)
        private
        view
        returns (PackedUserOperation memory userOp)
    {
        userOp.sender = sender;
        userOp.preVerificationGas = 50_000;
        userOp.accountGasLimits = _accountGasLimits(200_000, 250_000);
        userOp.gasFees = bytes32(abi.encodePacked(uint128(0), uint128(1 gwei)));
        userOp.paymasterAndData =
            abi.encodePacked(address(paymaster), paymasterVerificationGasLimit, paymasterPostOpGasLimit);
    }

    function _accountGasLimits(uint256 verificationGasLimit, uint256 callGasLimit) private pure returns (bytes32) {
        return bytes32(abi.encodePacked(uint128(verificationGasLimit), uint128(callGasLimit)));
    }

    function _unusedGasPenalty(uint256 gasUsed, uint256 gasLimit) private pure returns (uint256) {
        if (gasLimit <= gasUsed + PENALTY_GAS_THRESHOLD) {
            return 0;
        }
        return ((gasLimit - gasUsed) * UNUSED_GAS_PENALTY_PERCENT) / 100;
    }

    function _ceilDiv(uint256 numerator, uint256 denominator) private pure returns (uint256) {
        return (numerator + denominator - 1) / denominator;
    }

    function _deployZkSysToken() private returns (SyscoinZKSYSToken) {
        SyscoinZKSYSToken implementation = new SyscoinZKSYSToken();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), address(this)))
        );
        return SyscoinZKSYSToken(address(proxy));
    }

    function _validatedProductionZkSysUserOp(bool delegateSender, bool delegatePaymaster)
        private
        returns (SyscoinZKSYSToken zkToken, bytes memory context, uint256 actualCost, uint256 actualFeePerGas)
    {
        zkToken = _deployZkSysToken();
        paymaster = _deployPaymaster(address(zkToken), TARGET_ENTRY_POINT_RESERVE);
        zkToken.grantRole(zkToken.MINTER_ROLE(), address(this));
        zkToken.grantRole(zkToken.BURNER_ROLE(), address(paymaster));
        zkToken.mint(sender, 1_000 ether);

        if (delegateSender) {
            vm.prank(sender);
            zkToken.delegate(sender);
        }
        if (delegatePaymaster) {
            // Synthetic worst-case checkpoint setup: the paymaster has no production self-delegate hook.
            vm.prank(address(paymaster));
            zkToken.delegate(address(paymaster));
        }

        (context, actualCost, actualFeePerGas) = _validateProductionZkSysUserOp(zkToken);
    }

    function _validateProductionZkSysUserOp(SyscoinZKSYSToken zkToken)
        private
        returns (bytes memory context, uint256 actualCost, uint256 actualFeePerGas)
    {
        actualCost = 6 ether;
        actualFeePerGas = 1 gwei;

        vm.prank(sender);
        zkToken.approve(address(paymaster), 10 ether);

        uint256 validationData;
        (context, validationData) = entryPoint.validate(paymaster, _userOp(), 10 ether);
        assertEq(validationData, 0);
    }

    function _deployPaymaster(address burnableToken, uint256 targetEntryPointReserve)
        private
        returns (PaliFixedRateTokenPaymaster)
    {
        return _deployPaymasterWithoutBinding(burnableToken, targetEntryPointReserve);
    }

    function _deployPaymasterWithoutBinding(address burnableToken, uint256 targetEntryPointReserve)
        private
        returns (PaliFixedRateTokenPaymaster)
    {
        return new PaliFixedRateTokenPaymaster(
            IEntryPoint(address(entryPoint)), IERC20Burnable(burnableToken), owner, targetEntryPointReserve
        );
    }
}
