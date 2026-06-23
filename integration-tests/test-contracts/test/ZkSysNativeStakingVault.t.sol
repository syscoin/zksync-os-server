// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC1967Proxy} from "@openzeppelin/contracts-v4/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {IZkSysStakeWeightRegistry, ZkSysNativeStakingVault} from "contracts/src/zksys/ZkSysNativeStakingVault.sol";

contract MockStakeWeightRegistry is IZkSysStakeWeightRegistry {
    address public lastAccount;
    uint256 public lastStakeWeight;

    mapping(address account => uint256 stakeWeight) public stakeWeightOf;

    function updateStakeWeight(address account, uint256 stakeWeight) external {
        lastAccount = account;
        lastStakeWeight = stakeWeight;
        stakeWeightOf[account] = stakeWeight;
    }
}

contract RejectNativeReceiver {
    receive() external payable {
        revert("reject native");
    }
}

contract ReenteringNativeReceiver {
    ZkSysNativeStakingVault public immutable vault;
    bool public attempted;
    bool public reenteredSuccessfully;

    constructor(ZkSysNativeStakingVault vault_) {
        vault = vault_;
    }

    function stakeInVault() external payable {
        vault.deposit{value: msg.value}();
    }

    receive() external payable {
        if (attempted) {
            return;
        }
        attempted = true;
        (reenteredSuccessfully,) = address(vault).call(abi.encodeCall(ZkSysNativeStakingVault.withdraw, (1 wei)));
    }
}

contract ZkSysNativeStakingVaultTest is Test {
    address private alice = address(0xA11CE);
    address private bob = address(0xB0B);

    MockStakeWeightRegistry private registry;
    ZkSysNativeStakingVault private vault;

    function setUp() public {
        registry = new MockStakeWeightRegistry();
        vault = _deployVault(registry);
    }

    function testDepositUpdatesStakeWeightAndCustodiesNativeSys() public {
        vm.deal(alice, 3 ether);

        vm.prank(alice);
        vault.deposit{value: 3 ether}();

        assertEq(vault.stakeOf(alice), 3 ether);
        assertEq(vault.totalStaked(), 3 ether);
        assertEq(address(vault).balance, 3 ether);
        assertEq(registry.stakeWeightOf(alice), 3 ether);
        assertEq(registry.lastAccount(), alice);
        assertEq(registry.lastStakeWeight(), 3 ether);
    }

    function testReceiveDepositsForSender() public {
        vm.deal(alice, 1 ether);

        vm.prank(alice);
        (bool success,) = address(vault).call{value: 1 ether}("");

        assertTrue(success);
        assertEq(vault.stakeOf(alice), 1 ether);
        assertEq(registry.stakeWeightOf(alice), 1 ether);
    }

    function testWithdrawReducesStakeWeightBeforeReturningNativeSys() public {
        vm.deal(alice, 5 ether);
        vm.prank(alice);
        vault.deposit{value: 5 ether}();

        uint256 balanceBefore = alice.balance;
        vm.prank(alice);
        vault.withdraw(2 ether);

        assertEq(alice.balance, balanceBefore + 2 ether);
        assertEq(vault.stakeOf(alice), 3 ether);
        assertEq(vault.totalStaked(), 3 ether);
        assertEq(address(vault).balance, 3 ether);
        assertEq(registry.stakeWeightOf(alice), 3 ether);
    }

    function testWithdrawToCanSendStakeToThirdParty() public {
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        vault.deposit{value: 2 ether}();

        uint256 bobBalanceBefore = bob.balance;
        vm.prank(alice);
        vault.withdrawTo(payable(bob), 1 ether);

        assertEq(bob.balance, bobBalanceBefore + 1 ether);
        assertEq(vault.stakeOf(alice), 1 ether);
        assertEq(vault.totalStaked(), 1 ether);
        assertEq(registry.stakeWeightOf(alice), 1 ether);
    }

    function testWithdrawToRejectsZeroReceiverAndFailedNativeTransfer() public {
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        vault.deposit{value: 2 ether}();

        vm.prank(alice);
        vm.expectRevert(ZkSysNativeStakingVault.InvalidAddress.selector);
        vault.withdrawTo(payable(address(0)), 1 ether);

        RejectNativeReceiver receiver = new RejectNativeReceiver();
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(ZkSysNativeStakingVault.NativeTransferFailed.selector, address(receiver), 1 ether)
        );
        vault.withdrawTo(payable(address(receiver)), 1 ether);
    }

    function testWithdrawToReentrantReceiverCannotWithdrawDuringNativeTransfer() public {
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        vault.deposit{value: 2 ether}();

        ReenteringNativeReceiver receiver = new ReenteringNativeReceiver(vault);
        vm.deal(address(receiver), 1 wei);
        receiver.stakeInVault{value: 1 wei}();

        vm.prank(alice);
        vault.withdrawTo(payable(address(receiver)), 1 ether);

        assertTrue(receiver.attempted());
        assertFalse(receiver.reenteredSuccessfully());
        assertEq(vault.stakeOf(alice), 1 ether);
        assertEq(vault.stakeOf(address(receiver)), 1 wei);
        assertEq(vault.totalStaked(), 1 ether + 1 wei);
        assertEq(registry.stakeWeightOf(alice), 1 ether);
        assertEq(registry.stakeWeightOf(address(receiver)), 1 wei);
    }

    function testRejectsZeroAmountAndOverWithdraw() public {
        vm.expectRevert(ZkSysNativeStakingVault.InvalidAmount.selector);
        vault.deposit{value: 0}();

        vm.deal(alice, 1 ether);
        vm.prank(alice);
        vault.deposit{value: 1 ether}();

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(ZkSysNativeStakingVault.InsufficientStake.selector, 2 ether, 1 ether));
        vault.withdraw(2 ether);
    }

    function testImplementationInitializationIsDisabled() public {
        ZkSysNativeStakingVault implementation = new ZkSysNativeStakingVault();

        vm.expectRevert(bytes("Initializable: contract is already initialized"));
        implementation.initialize(registry);
    }

    function _deployVault(IZkSysStakeWeightRegistry weightRegistry) private returns (ZkSysNativeStakingVault) {
        ZkSysNativeStakingVault implementation = new ZkSysNativeStakingVault();
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation), abi.encodeCall(ZkSysNativeStakingVault.initialize, (weightRegistry))
        );
        return ZkSysNativeStakingVault(payable(address(proxy)));
    }
}
