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

// SYSCOIN: adversarial token used to pin the tank's exact-transfer and
// callback isolation requirements. Production zkSYS is exact-transfer and
// callback-free today, but its proxy must remain safe across future upgrades.
contract AdversarialGasToken is IZkSysGasToken {
    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    uint256 public transferFromFee;
    uint256 public transferFromExtraDebit;
    uint256 public transferExtraDebit;
    uint256 public transferDeliveryFee;
    uint256 public burnExtraDebit;
    address public callbackTarget;
    bytes private _callbackData;
    address public observedAccount;
    uint256 public observedCreditDuringCallback;
    bool public callbackAttempted;
    bool public callbackSucceeded;
    bytes4 public callbackRevertSelector;

    function decimals() external pure returns (uint8) {
        return 18;
    }

    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    function allowance(address owner, address spender) external view returns (uint256) {
        return _allowances[owner][spender];
    }

    function mint(address account, uint256 amount) external {
        _balances[account] += amount;
    }

    function confiscate(address account, uint256 amount) external {
        _balances[account] -= amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function setTransferFromFee(uint256 fee) external {
        transferFromFee = fee;
    }

    function setTransferFromExtraDebit(uint256 extraDebit) external {
        transferFromExtraDebit = extraDebit;
    }

    function setTransferBehavior(uint256 extraDebit, uint256 deliveryFee) external {
        transferExtraDebit = extraDebit;
        transferDeliveryFee = deliveryFee;
    }

    function setBurnExtraDebit(uint256 extraDebit) external {
        burnExtraDebit = extraDebit;
    }

    function configureCallback(address target, bytes calldata data, address account) external {
        callbackTarget = target;
        _callbackData = data;
        observedAccount = account;
        callbackAttempted = false;
        callbackSucceeded = false;
        callbackRevertSelector = bytes4(0);
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = _allowances[from][msg.sender];
        require(allowed >= amount, "allowance");
        _allowances[from][msg.sender] = allowed - amount;

        uint256 fee = transferFromFee;
        require(amount >= fee, "fee");
        _balances[from] -= amount + transferFromExtraDebit;
        _balances[to] += amount - fee;
        _attemptCallback();
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        uint256 deliveryFee = transferDeliveryFee;
        require(amount >= deliveryFee, "delivery fee");
        _balances[msg.sender] -= amount + transferExtraDebit;
        _balances[to] += amount - deliveryFee;
        _attemptCallback();
        return true;
    }

    function burn(address from, uint256 amount) external returns (bool) {
        _balances[from] -= amount + burnExtraDebit;
        _attemptCallback();
        return true;
    }

    function _attemptCallback() private {
        address target = callbackTarget;
        if (target == address(0)) {
            return;
        }

        // A read-only callback is allowed and proves funding credit is not
        // published until the exact token transfer has completed.
        observedCreditDuringCallback = ZkSysGasTank(target).creditOf(observedAccount);
        callbackAttempted = true;
        bytes memory returnData;
        (callbackSucceeded, returnData) = target.call(_callbackData);
        if (!callbackSucceeded && returnData.length >= 4) {
            bytes4 selector;
            assembly {
                selector := mload(add(returnData, 0x20))
            }
            callbackRevertSelector = selector;
        }
    }
}

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

    /// Emulate the bootloader's fee precharge: debit only the sender's credit
    /// via a raw storage write, exactly as the STF does. `totalCredits` is
    /// intentionally untouched so the pending refund/tip stays backed and the
    /// precharge never appears as burnable surplus mid-transaction.
    function _stfPrecharge(address account, uint256 amount) private {
        bytes32 creditSlot = _creditSlot(account);
        uint256 credit = uint256(vm.load(address(tank), creditSlot));
        require(credit >= amount, "stf precharge underflow");
        vm.store(address(tank), creditSlot, bytes32(credit - amount));
    }

    /// Emulate the bootloader's refund/tip: credit the account's ledger entry
    /// only, without touching totalCredits.
    function _stfCreditAccountOnly(address account, uint256 amount) private {
        bytes32 creditSlot = _creditSlot(account);
        uint256 credit = uint256(vm.load(address(tank), creditSlot));
        vm.store(address(tank), creditSlot, bytes32(credit + amount));
    }

    /// Emulate the bootloader's settlement burn: reduce totalCredits by the
    /// burned portion of the fee (precharge minus refund minus tip).
    function _stfDebitTotalCredits(uint256 amount) private {
        uint256 total = uint256(vm.load(address(tank), bytes32(TOTAL_CREDITS_SLOT)));
        require(total >= amount, "stf totalCredits underflow");
        vm.store(address(tank), bytes32(TOTAL_CREDITS_SLOT), bytes32(total - amount));
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

    function test_WithdrawFailsClosedWhenTotalCreditsInvariantIsCorrupted() public {
        vm.startPrank(alice);
        token.approve(address(tank), 10 ether);
        tank.fund(10 ether);
        vm.stopPrank();

        // Emulate a prior faulty direct bootloader mutation: the account still
        // owns ten credits but the aggregate claims to back only one.
        vm.store(address(tank), bytes32(TOTAL_CREDITS_SLOT), bytes32(uint256(1 ether)));

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TotalCreditsUnderflow.selector, 1 ether, 2 ether));
        tank.withdraw(2 ether);

        assertEq(tank.creditOf(alice), 10 ether);
        assertEq(tank.totalCredits(), 1 ether);
        assertEq(token.balanceOf(address(tank)), 10 ether);
    }

    function test_FeeOnTransferFundingRevertsAtomically() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);
        adversarialToken.setTransferFromFee(1 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenBalanceMismatch.selector, 10 ether, 9 ether));
        guardedTank.fund(10 ether);
        vm.stopPrank();

        // The failed exact-balance check reverts both contracts atomically.
        assertEq(adversarialToken.balanceOf(alice), 100 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 0);
        assertEq(adversarialToken.allowance(alice, address(guardedTank)), 10 ether);
        assertEq(guardedTank.creditOf(alice), 0);
        assertEq(guardedTank.totalCredits(), 0);
    }

    function test_FunderOverDebitRevertsAtomically() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);
        adversarialToken.setTransferFromExtraDebit(1 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenBalanceMismatch.selector, 90 ether, 89 ether));
        guardedTank.fund(10 ether);
        vm.stopPrank();

        assertEq(adversarialToken.balanceOf(alice), 100 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 0);
        assertEq(adversarialToken.allowance(alice, address(guardedTank)), 10 ether);
        assertEq(guardedTank.creditOf(alice), 0);
        assertEq(guardedTank.totalCredits(), 0);
    }

    function test_FundingFailsClosedUntilPreexistingDeficitIsRestored() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 10 ether);
        adversarialToken.mint(bob, 20 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();
        adversarialToken.confiscate(address(guardedTank), 1 ether);

        vm.startPrank(bob);
        adversarialToken.approve(address(guardedTank), 5 ether);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.InsufficientBacking.selector, 9 ether, 10 ether));
        guardedTank.fund(5 ether);
        vm.stopPrank();

        assertEq(adversarialToken.balanceOf(bob), 20 ether);
        assertEq(adversarialToken.allowance(bob, address(guardedTank)), 5 ether);
        assertEq(guardedTank.creditOf(bob), 0);
        assertEq(guardedTank.totalCredits(), 10 ether);

        // A direct donation restores solvency without mutating consensus
        // ledger slots; subsequent exact funding can safely proceed.
        adversarialToken.mint(address(guardedTank), 1 ether);
        vm.prank(bob);
        guardedTank.fund(5 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 15 ether);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.creditOf(bob), 5 ether);
        assertEq(guardedTank.totalCredits(), 15 ether);
    }

    function test_TokenCallbackCannotReenterOrObserveUncommittedCredit() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);
        adversarialToken.configureCallback(address(guardedTank), abi.encodeCall(ZkSysGasTank.fund, (1)), alice);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 20 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();

        assertTrue(adversarialToken.callbackAttempted());
        assertFalse(adversarialToken.callbackSucceeded());
        assertEq(adversarialToken.callbackRevertSelector(), ZkSysGasTank.ReentrantCall.selector);
        assertEq(adversarialToken.observedCreditDuringCallback(), 0);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.totalCredits(), 10 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 10 ether);

        // A completed call clears transient guard state; a later transaction
        // can fund normally and its callback sees only already-committed credit.
        vm.prank(alice);
        guardedTank.fund(5 ether);
        assertFalse(adversarialToken.callbackSucceeded());
        assertEq(adversarialToken.callbackRevertSelector(), ZkSysGasTank.ReentrantCall.selector);
        assertEq(adversarialToken.observedCreditDuringCallback(), 10 ether);
        assertEq(guardedTank.creditOf(alice), 15 ether);
        assertEq(guardedTank.totalCredits(), 15 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 15 ether);
    }

    function test_WithdrawAndBurnTokenCallbacksCannotReenter() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();

        adversarialToken.configureCallback(
            address(guardedTank), abi.encodeCall(ZkSysGasTank.withdraw, (1 ether)), alice
        );
        vm.prank(alice);
        guardedTank.withdraw(2 ether);

        assertTrue(adversarialToken.callbackAttempted());
        assertFalse(adversarialToken.callbackSucceeded());
        assertEq(adversarialToken.callbackRevertSelector(), ZkSysGasTank.ReentrantCall.selector);
        assertEq(guardedTank.creditOf(alice), 8 ether);
        assertEq(guardedTank.totalCredits(), 8 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 8 ether);

        // Directly donated tokens are surplus. The token's burn callback also
        // cannot recursively enter permissionless surplus burning.
        adversarialToken.mint(address(guardedTank), 2 ether);
        adversarialToken.configureCallback(address(guardedTank), abi.encodeCall(ZkSysGasTank.burnSurplus, ()), alice);
        assertEq(guardedTank.burnSurplus(), 2 ether);

        assertTrue(adversarialToken.callbackAttempted());
        assertFalse(adversarialToken.callbackSucceeded());
        assertEq(adversarialToken.callbackRevertSelector(), ZkSysGasTank.ReentrantCall.selector);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 8 ether);
        assertEq(guardedTank.totalCredits(), 8 ether);
    }

    function test_WithdrawRejectsTokenOverDebitAtomically() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();
        adversarialToken.mint(address(guardedTank), 5 ether);
        adversarialToken.setTransferBehavior(1 ether, 0);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenBalanceMismatch.selector, 13 ether, 12 ether));
        guardedTank.withdraw(2 ether);

        assertEq(adversarialToken.balanceOf(alice), 90 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 15 ether);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.totalCredits(), 10 ether);
    }

    function test_WithdrawFailsClosedWhenBackingWasConfiscated() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();
        adversarialToken.confiscate(address(guardedTank), 1 ether);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.InsufficientBacking.selector, 9 ether, 10 ether));
        guardedTank.withdraw(2 ether);

        assertEq(adversarialToken.balanceOf(address(guardedTank)), 9 ether);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.totalCredits(), 10 ether);
    }

    function test_WithdrawRejectsTokenUnderDeliveryAtomically() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();
        adversarialToken.setTransferBehavior(0, 1 ether);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenBalanceMismatch.selector, 92 ether, 91 ether));
        guardedTank.withdraw(2 ether);

        assertEq(adversarialToken.balanceOf(alice), 90 ether);
        assertEq(adversarialToken.balanceOf(address(guardedTank)), 10 ether);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.totalCredits(), 10 ether);
    }

    function test_BurnRejectsTokenOverDebitAtomically() public {
        AdversarialGasToken adversarialToken = new AdversarialGasToken();
        ZkSysGasTank guardedTank = new ZkSysGasTank(IZkSysGasToken(address(adversarialToken)));
        adversarialToken.mint(alice, 100 ether);

        vm.startPrank(alice);
        adversarialToken.approve(address(guardedTank), 10 ether);
        guardedTank.fund(10 ether);
        vm.stopPrank();
        adversarialToken.mint(address(guardedTank), 3 ether);
        adversarialToken.setBurnExtraDebit(1 ether);

        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.TokenBalanceMismatch.selector, 10 ether, 9 ether));
        guardedTank.burnSurplus();

        assertEq(adversarialToken.balanceOf(address(guardedTank)), 13 ether);
        assertEq(guardedTank.creditOf(alice), 10 ether);
        assertEq(guardedTank.totalCredits(), 10 ether);
        assertEq(guardedTank.surplus(), 3 ether);
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

        // Bootloader precharges the full fee (sender credit only), then after
        // execution refunds the unused part to the sender, tips the operator,
        // and reduces totalCredits by the burned portion, which becomes
        // surplus.
        uint256 fee = 10 ether;
        uint256 refund = 4 ether;
        uint256 tip = 1 ether;
        uint256 burned = fee - refund - tip;
        _stfPrecharge(alice, fee);
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);

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
    /// decremented her credit entry, so only the remainder is withdrawable and
    /// the tank stays solvent (token balance >= totalCredits) at every step.
    function test_MidTxWithdrawCannotDoubleSpendPrechargedFee() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        // Bootloader precharges the full fee before execution starts. Only
        // the sender's credit drops; totalCredits keeps backing the pending
        // refund/tip.
        uint256 fee = 10 ether;
        _stfPrecharge(alice, fee);
        assertEq(tank.creditOf(alice), 90 ether);
        assertEq(tank.totalCredits(), 100 ether);

        // Mid-execution, alice tries to pull the precharged fee too.
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysGasTank.InsufficientCredit.selector, 90 ether, 100 ether));
        tank.withdraw(100 ether);

        // She can only withdraw what is genuinely hers.
        vm.prank(alice);
        tank.withdraw(90 ether);
        assertEq(token.balanceOf(alice), 990 ether);
        assertEq(tank.creditOf(alice), 0);
        // The in-flight precharge still counts toward totalCredits, fully
        // backed by the tokens it left in the tank.
        assertEq(tank.totalCredits(), fee);
        assertEq(token.balanceOf(address(tank)), fee);
        assertEq(tank.surplus(), 0);

        // Post-execution the bootloader refunds unused gas, tips the
        // operator, and burns the rest out of totalCredits; refund and tip
        // stay fully backed, the burned part is surplus.
        uint256 refund = 4 ether;
        uint256 tip = 1 ether;
        uint256 burned = fee - refund - tip;
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);
        assertEq(tank.totalCredits(), refund + tip);
        assertGe(token.balanceOf(address(tank)), tank.totalCredits());
        assertEq(tank.surplus(), burned);

        vm.prank(alice);
        tank.withdraw(refund);
        vm.prank(coinbase);
        tank.withdraw(tip);
        tank.burnSurplus();
        assertEq(token.balanceOf(address(tank)), 0);
        assertEq(tank.totalCredits(), 0);
    }

    /// Regression for the release-blocker found in review: a tank-paid tx
    /// calling burnSurplus() mid-execution (after the precharge, before
    /// refund/tip settlement) must not be able to burn the tokens backing the
    /// pending refund and tip. Because the precharge leaves totalCredits
    /// untouched, the precharge is never visible as surplus and the tank
    /// remains solvent (token balance >= totalCredits) throughout.
    function test_MidTxBurnSurplusCannotOverburnPendingRefundAndTip() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        uint256 fee = 10 ether;
        uint256 refund = 4 ether;
        uint256 tip = 1 ether;
        uint256 burned = fee - refund - tip;

        // Precharge debits the sender credit only, not totalCredits.
        _stfPrecharge(alice, fee);
        assertEq(tank.creditOf(alice), 90 ether);
        assertEq(tank.totalCredits(), 100 ether);

        // The precharge must NOT appear as burnable surplus during execution.
        assertEq(tank.surplus(), 0);
        vm.expectRevert(ZkSysGasTank.NoSurplus.selector);
        tank.burnSurplus();

        // Even combined with a mid-tx withdrawal of everything else, nothing
        // becomes burnable and the tank stays solvent.
        vm.prank(alice);
        tank.withdraw(90 ether);
        assertEq(tank.surplus(), 0);
        vm.expectRevert(ZkSysGasTank.NoSurplus.selector);
        tank.burnSurplus();
        assertGe(token.balanceOf(address(tank)), tank.totalCredits());

        // Settlement restores totalCredits == sum of credits; only the truly
        // burned portion becomes surplus.
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);

        assertEq(tank.totalCredits(), refund + tip);
        assertEq(token.balanceOf(address(tank)), fee);
        assertEq(tank.surplus(), burned);

        tank.burnSurplus();

        // Remaining credits are exactly backed; everyone can exit.
        assertEq(token.balanceOf(address(tank)), refund + tip);
        assertEq(tank.totalCredits(), refund + tip);
        vm.prank(alice);
        tank.withdraw(refund);
        vm.prank(coinbase);
        tank.withdraw(tip);
        assertEq(token.balanceOf(address(tank)), 0);
        assertEq(tank.totalCredits(), 0);
    }

    /// A surplus accumulated by earlier, fully settled transactions must stay
    /// burnable mid-transaction, while the in-flight precharge remains
    /// protected: a mid-tx burnSurplus() burns exactly the old surplus and
    /// nothing of the pending refund/tip backing.
    function test_MidTxBurnSurplusBurnsOnlyPreexistingSurplus() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        // A prior tank-paid tx settles completely, leaving 5 zkSYS of
        // unburned surplus in the tank.
        uint256 oldBurned = 5 ether;
        _stfPrecharge(alice, 10 ether);
        _stfCreditAccountOnly(alice, 4 ether);
        _stfCreditAccountOnly(coinbase, 1 ether);
        _stfDebitTotalCredits(oldBurned);
        assertEq(tank.surplus(), oldBurned);

        // A new tank-paid tx precharges; the precharge must not enlarge the
        // burnable surplus.
        uint256 fee = 20 ether;
        uint256 refund = 8 ether;
        uint256 tip = 2 ether;
        uint256 burned = fee - refund - tip;
        _stfPrecharge(alice, fee);
        assertEq(tank.surplus(), oldBurned);

        // Mid-execution, burnSurplus() destroys exactly the old surplus.
        uint256 supplyBefore = token.totalSupply();
        uint256 burnedOut = tank.burnSurplus();
        assertEq(burnedOut, oldBurned);
        assertEq(token.totalSupply(), supplyBefore - oldBurned);
        assertEq(tank.surplus(), 0);
        assertGe(token.balanceOf(address(tank)), tank.totalCredits());

        // Nothing further is burnable until the tx settles.
        vm.expectRevert(ZkSysGasTank.NoSurplus.selector);
        tank.burnSurplus();

        // Settlement exposes exactly the new burned portion as surplus and
        // keeps totalCredits equal to the sum of account credits.
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);
        assertEq(tank.surplus(), burned);
        assertEq(tank.totalCredits(), tank.creditOf(alice) + tank.creditOf(coinbase));

        // Full exit stays solvent to the last wei.
        tank.burnSurplus();
        uint256 aliceCredit = tank.creditOf(alice);
        uint256 coinbaseCredit = tank.creditOf(coinbase);
        vm.prank(alice);
        tank.withdraw(aliceCredit);
        vm.prank(coinbase);
        tank.withdraw(coinbaseCredit);
        assertEq(token.balanceOf(address(tank)), 0);
        assertEq(tank.totalCredits(), 0);
    }

    /// Regression pinning why settlement burns `fee_to_prepay - refund - tip`
    /// rather than `gas_used * gas_price - tip`: the precharge
    /// (`fee_to_prepay`) can include a blob/pubdata fee component that is
    /// neither refunded to the sender nor tipped to the operator. That
    /// component must also leave totalCredits at settlement, or it would stay
    /// stranded there forever (overstating totalCredits and understating the
    /// burnable surplus).
    function test_BlobFeeComponentIsBurnedFromTotalCredits() public {
        vm.startPrank(alice);
        token.approve(address(tank), 100 ether);
        tank.fund(100 ether);
        vm.stopPrank();

        uint256 gasFee = 10 ether;
        uint256 blobFee = 3 ether;
        uint256 fee = gasFee + blobFee; // fee_to_prepay
        uint256 refund = 4 ether; // gas portion only
        uint256 tip = 1 ether; // gas portion only
        uint256 baseBurn = gasFee - refund - tip;
        uint256 burned = fee - refund - tip; // includes the blob component

        _stfPrecharge(alice, fee);
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);

        // totalCredits dropped by the blob component on top of the base burn,
        // and stays equal to the sum of account credits.
        assertEq(tank.totalCredits(), 100 ether - baseBurn - blobFee);
        assertEq(tank.totalCredits(), tank.creditOf(alice) + tank.creditOf(coinbase));

        // The surplus is the base burn plus the blob burn, all destroyable.
        assertEq(tank.surplus(), baseBurn + blobFee);
        uint256 burnedOut = tank.burnSurplus();
        assertEq(burnedOut, baseBurn + blobFee);
        assertEq(token.balanceOf(address(tank)), tank.totalCredits());

        // Full exit stays solvent to the last wei.
        uint256 aliceCredit = tank.creditOf(alice);
        uint256 coinbaseCredit = tank.creditOf(coinbase);
        vm.prank(alice);
        tank.withdraw(aliceCredit);
        vm.prank(coinbase);
        tank.withdraw(coinbaseCredit);
        assertEq(token.balanceOf(address(tank)), 0);
        assertEq(tank.totalCredits(), 0);
    }

    function testFuzz_BootloaderSettlementPreservesLedgerAndBacking(uint96 rawFee, uint96 rawRefund, uint96 rawTip)
        public
    {
        uint256 initialCredit = 100 ether;
        uint256 fee = bound(uint256(rawFee), 1, initialCredit);
        uint256 refund = bound(uint256(rawRefund), 0, fee);
        uint256 tip = bound(uint256(rawTip), 0, fee - refund);
        uint256 burned = fee - refund - tip;

        vm.startPrank(alice);
        token.approve(address(tank), initialCredit);
        tank.fund(initialCredit);
        vm.stopPrank();

        _stfPrecharge(alice, fee);
        _stfCreditAccountOnly(alice, refund);
        _stfCreditAccountOnly(coinbase, tip);
        _stfDebitTotalCredits(burned);

        uint256 sumCredits = tank.creditOf(alice) + tank.creditOf(coinbase);
        assertEq(tank.totalCredits(), sumCredits);
        assertGe(token.balanceOf(address(tank)), tank.totalCredits());
        assertEq(tank.surplus(), burned);
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
