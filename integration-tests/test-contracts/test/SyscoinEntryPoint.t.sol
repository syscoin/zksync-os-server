// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IEntryPoint} from "@account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";
import {Test} from "forge-std/Test.sol";
import {SyscoinEntryPoint} from "contracts/src/pali/SyscoinEntryPoint.sol";

contract SyscoinEntryPointHarness is SyscoinEntryPoint {
    constructor(address syscoinSponsoredPaymaster_) SyscoinEntryPoint(syscoinSponsoredPaymaster_) {}

    function routeCompensation(
        address payable beneficiary,
        uint256 beneficiaryCollected,
        uint256 syscoinSponsoredCollected
    ) external {
        _routeCompensation(beneficiary, beneficiaryCollected, syscoinSponsoredCollected);
    }

    function _iterateValidationPhase(
        PackedUserOperation[] calldata ops,
        UserOpInfo[] memory opInfos,
        address,
        uint256 opIndexOffset
    ) internal pure override returns (uint256 opsLen) {
        opsLen = ops.length;
        for (uint256 i = 0; i < opsLen; i++) {
            opInfos[opIndexOffset + i].mUserOp.paymaster = _paymasterOf(ops[i]);
        }
    }

    function _executeUserOp(uint256, PackedUserOperation calldata userOp, UserOpInfo memory)
        internal
        pure
        override
        returns (uint256 collected)
    {
        return userOp.nonce;
    }

    function _paymasterOf(PackedUserOperation calldata userOp) private pure returns (address paymaster) {
        bytes calldata paymasterAndData = userOp.paymasterAndData;
        if (paymasterAndData.length >= 20) {
            paymaster = address(bytes20(paymasterAndData[0:20]));
        }
    }
}

contract SyscoinEntryPointTest is Test {
    address private paymaster = address(0xAA);
    address private otherPaymaster = address(0xBB);
    address payable private beneficiary = payable(address(0xBEEF));
    address private bundler = address(0xCA11);

    function testConstructorRejectsZeroPaymaster() public {
        vm.expectRevert(SyscoinEntryPoint.InvalidSyscoinSponsoredPaymaster.selector);
        new SyscoinEntryPoint(address(0));
    }

    function testRoutesSyscoinSponsoredCompensationBackToPaymasterDeposit() public {
        SyscoinEntryPointHarness entryPoint = new SyscoinEntryPointHarness(paymaster);
        vm.deal(address(entryPoint), 7 ether);

        entryPoint.routeCompensation(beneficiary, 2 ether, 5 ether);

        assertEq(beneficiary.balance, 2 ether);
        assertEq(entryPoint.balanceOf(paymaster), 5 ether);
        assertEq(address(entryPoint).balance, 5 ether);
    }

    function testHandleOpsRoutesSponsoredCompensationBackToPaymasterDeposit() public {
        SyscoinEntryPointHarness entryPoint = new SyscoinEntryPointHarness(paymaster);
        vm.deal(address(entryPoint), 5 ether);

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _userOp(paymaster, 5 ether);

        vm.prank(bundler, bundler);
        entryPoint.handleOps(ops, beneficiary);

        assertEq(beneficiary.balance, 0);
        assertEq(entryPoint.balanceOf(paymaster), 5 ether);
        assertEq(address(entryPoint).balance, 5 ether);
    }

    function testHandleOpsSplitsMixedSponsoredAndNonSponsoredCompensation() public {
        SyscoinEntryPointHarness entryPoint = new SyscoinEntryPointHarness(paymaster);
        vm.deal(address(entryPoint), 7 ether);

        PackedUserOperation[] memory ops = new PackedUserOperation[](2);
        ops[0] = _userOp(paymaster, 5 ether);
        ops[1] = _userOp(otherPaymaster, 2 ether);

        vm.prank(bundler, bundler);
        entryPoint.handleOps(ops, beneficiary);

        assertEq(beneficiary.balance, 2 ether);
        assertEq(entryPoint.balanceOf(paymaster), 5 ether);
        assertEq(entryPoint.balanceOf(otherPaymaster), 0);
        assertEq(address(entryPoint).balance, 5 ether);
    }

    function testHandleAggregatedOpsSplitsMixedSponsoredAndNonSponsoredCompensation() public {
        SyscoinEntryPointHarness entryPoint = new SyscoinEntryPointHarness(paymaster);
        vm.deal(address(entryPoint), 7 ether);

        IEntryPoint.UserOpsPerAggregator[] memory opsPerAggregator = new IEntryPoint.UserOpsPerAggregator[](1);
        opsPerAggregator[0].userOps = new PackedUserOperation[](2);
        opsPerAggregator[0].userOps[0] = _userOp(paymaster, 5 ether);
        opsPerAggregator[0].userOps[1] = _userOp(otherPaymaster, 2 ether);

        vm.prank(bundler, bundler);
        entryPoint.handleAggregatedOps(opsPerAggregator, beneficiary);

        assertEq(beneficiary.balance, 2 ether);
        assertEq(entryPoint.balanceOf(paymaster), 5 ether);
        assertEq(entryPoint.balanceOf(otherPaymaster), 0);
        assertEq(address(entryPoint).balance, 5 ether);
    }

    function testHandleOpsPaysBeneficiaryForWrongPaymaster() public {
        SyscoinEntryPointHarness entryPoint = new SyscoinEntryPointHarness(paymaster);
        vm.deal(address(entryPoint), 2 ether);

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _userOp(otherPaymaster, 2 ether);

        vm.prank(bundler, bundler);
        entryPoint.handleOps(ops, beneficiary);

        assertEq(beneficiary.balance, 2 ether);
        assertEq(entryPoint.balanceOf(paymaster), 0);
        assertEq(entryPoint.balanceOf(otherPaymaster), 0);
        assertEq(address(entryPoint).balance, 0);
    }

    function _userOp(address opPaymaster, uint256 collected) private pure returns (PackedUserOperation memory userOp) {
        userOp.nonce = collected;
        userOp.paymasterAndData = abi.encodePacked(opPaymaster);
    }
}
