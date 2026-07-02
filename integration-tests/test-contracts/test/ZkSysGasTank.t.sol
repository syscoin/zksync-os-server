// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {
    ITransparentUpgradeableProxy,
    TransparentUpgradeableProxy
} from "@openzeppelin/contracts-v4/proxy/transparent/TransparentUpgradeableProxy.sol";
import {Test} from "forge-std/Test.sol";
import {ZkSysProxyAdmin} from "contracts/src/zksys/ZkSysProxyAdmin.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";
import {IZkSysGasToken, ZkSysGasTank} from "contracts/src/zksys/ZkSysGasTank.sol";

contract ZkSysGasTankTest is Test {
    // Must match the bootloader constants in zksync-os
    // basic_bootloader/src/bootloader/transaction_flow/zk/syscoin_gas_tank.rs.
    uint256 private constant CREDIT_MAPPING_SLOT = 0;
    uint256 private constant TOTAL_CREDITS_SLOT = 1;

    address private admin = address(0xAD);
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);
    address private coinbase = address(0xC01BBA5E);

    SyscoinZKSYSToken private token;
    ZkSysGasTank private tank;

    function setUp() public {
        SyscoinZKSYSToken implementation = new SyscoinZKSYSToken();
        ZkSysProxyAdmin proxyAdmin = new ZkSysProxyAdmin(admin);
        token = SyscoinZKSYSToken(
            address(
                new TransparentUpgradeableProxy(
                    address(implementation),
                    address(proxyAdmin),
                    abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), admin))
                )
            )
        );
        tank = new ZkSysGasTank(IZkSysGasToken(address(token)));

        vm.startPrank(admin);
        token.grantRole(token.MINTER_ROLE(), admin);
        token.grantRole(token.BURNER_ROLE(), address(tank));
        token.mint(alice, 1_000 ether);
        token.mint(bob, 1_000 ether);
        vm.stopPrank();
    }

    function _creditSlot(address account) private pure returns (bytes32) {
        return keccak256(abi.encode(account, CREDIT_MAPPING_SLOT));
    }

    /// Emulate the bootloader's fee precharge: debit credit and totalCredits
    /// via raw storage writes, exactly as the STF does.
    function _stfDebit(address account, uint256 amount) private {
        bytes32 creditSlot = _creditSlot(account);
        uint256 credit = uint256(vm.load(address(tank), creditSlot));
        uint256 total = uint256(vm.load(address(tank), bytes32(TOTAL_CREDITS_SLOT)));
        require(credit >= amount && total >= amount, "stf debit underflow");
        vm.store(address(tank), creditSlot, bytes32(credit - amount));
        vm.store(address(tank), bytes32(TOTAL_CREDITS_SLOT), bytes32(total - amount));
    }

    /// Emulate the bootloader's refund/tip: credit account and totalCredits.
    function _stfCredit(address account, uint256 amount) private {
        bytes32 creditSlot = _creditSlot(account);
        uint256 credit = uint256(vm.load(address(tank), creditSlot));
        uint256 total = uint256(vm.load(address(tank), bytes32(TOTAL_CREDITS_SLOT)));
        vm.store(address(tank), creditSlot, bytes32(credit + amount));
        vm.store(address(tank), bytes32(TOTAL_CREDITS_SLOT), bytes32(total + amount));
    }

    // ---- storage layout pinning (consensus-critical) ----

    function test_StorageLayoutMatchesBootloaderAssumptions() public {
        vm.startPrank(alice);
        token.approve(address(tank), 123 ether);
        tank.fund(123 ether);
        vm.stopPrank();

        // credit mapping at slot 0
        assertEq(uint256(vm.load(address(tank), _creditSlot(alice))), 123 ether);
        // totalCredits at slot 1
        assertEq(uint256(vm.load(address(tank), bytes32(TOTAL_CREDITS_SLOT))), 123 ether);
        // views agree with raw slots
        assertEq(tank.creditOf(alice), 123 ether);
        assertEq(tank.totalCredits(), 123 ether);
    }

    // ---- fund / fundFor / withdraw ----

    function test_FundAndWithdrawRoundTrip() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        assertEq(token.balanceOf(alice), 900 ether);
        assertEq(tank.creditOf(alice), 100 ether);

        tank.withdraw(40 ether);
        vm.stopPrank();

        assertEq(token.balanceOf(alice), 940 ether);
        assertEq(tank.creditOf(alice), 60 ether);
        assertEq(tank.totalCredits(), 60 ether);
        assertEq(tank.surplus(), 0);
    }

    function test_FundForSponsorsAnotherAccount() public {
        vm.startPrank(bob);
        token.approve(address(tank), 50 ether);
        tank.fundFor(alice, 50 ether);
        vm.stopPrank();

        assertEq(tank.creditOf(alice), 50 ether);
        assertEq(tank.creditOf(bob), 0);

        // The sponsored account owns the credit and can withdraw it.
        vm.prank(alice);
        tank.withdraw(50 ether);
        assertEq(token.balanceOf(alice), 1_050 ether);
    }

    function test_RevertsOnZeroAmountAndZeroAddress() public {
        vm.startPrank(alice);
        token.approve(address(tank), 1 ether);
        vm.expectRevert(ZkSysGasTank.ZeroAmount.selector);
        tank.fund(0);
        vm.expectRevert(ZkSysGasTank.ZeroAddress.selector);
        tank.fundFor(address(0), 1 ether);
        vm.expectRevert(ZkSysGasTank.ZeroAmount.selector);
        tank.withdraw(0);
        vm.stopPrank();
    }

    function test_WithdrawMoreThanCreditReverts() public {
        vm.startPrank(alice);
        token.approve(address(tank), 10 ether);
        tank.fund(10 ether);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.InsufficientCredit.selector, 10 ether, 11 ether));
        tank.withdraw(11 ether);
        vm.stopPrank();
    }

    function test_ConstructorRejectsNonNativeDecimals() public {
        SyscoinZKSYSToken implementation = new SyscoinZKSYSToken();
        ZkSysProxyAdmin proxyAdmin = new ZkSysProxyAdmin(admin);
        SyscoinZKSYSToken eightDecimals = SyscoinZKSYSToken(
            address(
                new TransparentUpgradeableProxy(
                    address(implementation),
                    address(proxyAdmin),
                    abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS8", "ZKSYS8", uint8(8), admin))
                )
            )
        );
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenDecimalsMismatch.selector, uint8(8)));
        new ZkSysGasTank(IZkSysGasToken(address(eightDecimals)));

        vm.expectRevert(ZkSysGasTank.ZeroAddress.selector);
        new ZkSysGasTank(IZkSysGasToken(address(0)));
    }

    // ---- bootloader fee-flow emulation and surplus burning ----

    function test_BootloaderFeeFlowAccumulatesBurnableSurplus() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        // Bootloader precharges the full fee, refunds the unused part to the
        // sender, and tips the operator; the base-fee portion is never
        // credited back and becomes surplus.
        uint256 fee = 10 ether;
        uint256 refund = 4 ether;
        uint256 tip = 1 ether;
        _stfDebit(alice, fee);
        _stfCredit(alice, refund);
        _stfCredit(coinbase, tip);

        uint256 burned = fee - refund - tip;
        assertEq(tank.creditOf(alice), 100 ether - fee + refund);
        assertEq(tank.creditOf(coinbase), tip);
        assertEq(tank.totalCredits(), 100 ether - burned);
        assertEq(tank.surplus(), burned);

        // The operator can withdraw the tip in zkSYS.
        vm.prank(coinbase);
        tank.withdraw(tip);
        assertEq(token.balanceOf(coinbase), tip);
        assertEq(tank.surplus(), burned);

        // Anyone can burn the surplus; supply shrinks by the burned base fee.
        uint256 supplyBefore = token.totalSupply();
        vm.prank(bob);
        uint256 burnedOut = tank.burnSurplus();
        assertEq(burnedOut, burned);
        assertEq(token.totalSupply(), supplyBefore - burned);
        assertEq(tank.surplus(), 0);

        // Remaining credits are still fully backed and withdrawable.
        vm.prank(alice);
        tank.withdraw(100 ether - fee + refund);
        assertEq(tank.totalCredits(), 0);
        assertEq(token.balanceOf(address(tank)), 0);
    }

    /// Adversarial ordering: the sender tries to double-spend precharged
    /// credit by withdrawing mid-transaction (between the bootloader's
    /// precharge debit and the refund credit). The precharge already
    /// decremented the ledger, so only the remainder is withdrawable and the
    /// tank stays solvent (token balance >= totalCredits) at every step.
    function test_MidTxWithdrawCannotDoubleSpendPrechargedFee() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        // Bootloader precharges the full fee before execution starts.
        uint256 fee = 10 ether;
        _stfDebit(alice, fee);
        assertEq(tank.creditOf(alice), 90 ether);

        // Mid-execution, alice tries to pull the precharged fee too.
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.InsufficientCredit.selector, 90 ether, 100 ether));
        tank.withdraw(100 ether);

        // She can only withdraw what is genuinely hers.
        vm.prank(alice);
        tank.withdraw(90 ether);
        assertEq(token.balanceOf(alice), 990 ether);
        assertEq(tank.creditOf(alice), 0);
        assertEq(tank.totalCredits(), 0);
        // The precharged fee still backs the pending refund/tip.
        assertEq(token.balanceOf(address(tank)), fee);

        // Post-execution the bootloader refunds unused gas and tips the
        // operator; both stay fully backed, the rest is burnable surplus.
        uint256 refund = 4 ether;
        uint256 tip = 1 ether;
        _stfCredit(alice, refund);
        _stfCredit(coinbase, tip);
        assertEq(tank.totalCredits(), refund + tip);
        assertGe(token.balanceOf(address(tank)), tank.totalCredits());
        assertEq(tank.surplus(), fee - refund - tip);

        vm.prank(alice);
        tank.withdraw(refund);
        vm.prank(coinbase);
        tank.withdraw(tip);
        tank.burnSurplus();
        assertEq(token.balanceOf(address(tank)), 0);
        assertEq(tank.totalCredits(), 0);
    }

    function test_BurnSurplusRevertsWithoutSurplus() public {
        vm.startPrank(alice);
        token.approve(address(tank), 5 ether);
        tank.fund(5 ether);
        vm.stopPrank();

        vm.expectRevert(ZkSysGasTank.NoSurplus.selector);
        tank.burnSurplus();
    }

    function test_BurnSurplusRequiresBurnerRole() public {
        bytes32 burnerRole = token.BURNER_ROLE();
        vm.prank(admin);
        token.revokeRole(burnerRole, address(tank));

        vm.startPrank(alice);
        token.approve(address(tank), 5 ether);
        tank.fund(5 ether);
        vm.stopPrank();

        // Donate directly to create surplus without STF emulation.
        vm.prank(bob);
        token.transfer(address(tank), 1 ether);

        vm.expectRevert();
        tank.burnSurplus();
    }
}
