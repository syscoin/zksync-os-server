// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;
import {Test} from "forge-std/Test.sol";
import {PaliGuardianRecoveryModule} from "contracts/src/pali/PaliGuardianRecoveryModule.sol";

contract RecoveryProbeAccount {
    address public module;
    bool public executed;

    constructor(address m) {
        module = m;
    }

    function isModuleInstalled(uint256 t, address m, bytes calldata) external view returns (bool) {
        return t == 2 && m == module;
    }

    function executeFromExecutor(bytes32, bytes calldata) external returns (bytes[] memory) {
        require(msg.sender == module);
        executed = true;
        return new bytes[](0);
    }
}

contract GuardianEpochReplayTest is Test {
    function testUnsubmittedApprovalRejectedAfterReinstallAndFreshApprovalExecutes() public {
        PaliGuardianRecoveryModule m = new PaliGuardianRecoveryModule();
        RecoveryProbeAccount a = new RecoveryProbeAccount(address(m));
        uint256 signer = 0xA11CE;
        address[] memory g = new address[](1);
        g[0] = vm.addr(signer);
        bytes memory init = abi.encode(uint32(1 days), uint32(7 days), g, uint64(1));
        vm.prank(address(a));
        m.onInstall(init);
        bytes32 salt = keccak256("previous-installation-consent");
        bytes32 mode;
        bytes memory data = hex"1234";
        bytes32 hash = m.getRecoveryScheduleHash(address(a), salt, mode, data);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signer, hash);
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals =
            new PaliGuardianRecoveryModule.GuardianApproval[](1);
        approvals[0] = PaliGuardianRecoveryModule.GuardianApproval(g[0], bytes.concat(r, s, bytes1(v)));
        vm.prank(address(a));
        m.onUninstall("");
        vm.warp(block.timestamp + 365 days);
        vm.prank(address(a));
        m.onInstall(init);
        assertNotEq(hash, m.getRecoveryScheduleHash(address(a), salt, mode, data));
        assertEq(m.policyEpoch(address(a)), 3);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        m.scheduleRecovery(address(a), salt, mode, data, approvals);
        (v, r, s) = vm.sign(signer, m.getRecoveryScheduleHash(address(a), salt, mode, data));
        approvals[0].signature = bytes.concat(r, s, bytes1(v));
        m.scheduleRecovery(address(a), salt, mode, data, approvals);
        vm.warp(block.timestamp + 1 days);
        m.executeRecovery(address(a), salt, mode, data);
        assertTrue(a.executed());
    }
}
