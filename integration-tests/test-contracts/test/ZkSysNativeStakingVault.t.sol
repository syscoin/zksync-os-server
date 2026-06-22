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

    function testDepositForCreditsRecipientStakeWeight() public {
        vm.deal(bob, 2 ether);

        vm.prank(bob);
        vault.depositFor{value: 2 ether}(alice);

        assertEq(vault.stakeOf(alice), 2 ether);
        assertEq(vault.stakeOf(bob), 0);
        assertEq(registry.stakeWeightOf(alice), 2 ether);
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
