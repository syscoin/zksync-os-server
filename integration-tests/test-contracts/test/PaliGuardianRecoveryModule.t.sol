// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {MODULE_TYPE_EXECUTOR} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {Test} from "forge-std/Test.sol";
import {PaliGuardianRecoveryModule} from "../src/passkey/PaliGuardianRecoveryModule.sol";

contract MockRecoveryAccount {
    address public recoveryModule;

    constructor(address recoveryModule_) {
        recoveryModule = recoveryModule_;
    }

    function isModuleInstalled(uint256 moduleTypeId, address module, bytes calldata) external view returns (bool) {
        return moduleTypeId == MODULE_TYPE_EXECUTOR && module == recoveryModule;
    }
}

contract PaliGuardianRecoveryModuleTest is Test {
    PaliGuardianRecoveryModule private recovery;
    MockRecoveryAccount private account;

    uint256 private guardianPrivateKey = 0xA11CE;
    address private guardian;

    bytes32 private constant SALT = bytes32(0);
    bytes32 private constant MODE = bytes32(0);
    bytes private executionCalldata = hex"1234";

    function setUp() public {
        recovery = new PaliGuardianRecoveryModule();
        account = new MockRecoveryAccount(address(recovery));
        guardian = vm.addr(guardianPrivateKey);

        address[] memory guardians = new address[](1);
        guardians[0] = guardian;

        vm.prank(address(account));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1)));
    }

    function testActiveRecoveryCannotBeScheduledTwice() public {
        bytes[] memory signatures = _guardianSignatures();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryAlreadyScheduled.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
    }

    function testDifferentSaltCannotBypassActiveRecoveryLimit() public {
        bytes32 nextSalt = keccak256("next recovery attempt");
        bytes[] memory signatures = _guardianSignatures();
        bytes[] memory nextSignatures = _guardianSignatures(nextSalt);

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryAlreadyScheduled.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), nextSalt, MODE, executionCalldata, nextSignatures);
    }

    function testExpiredRecoveryCanBeRescheduledWithDeterministicSalt() public {
        bytes[] memory signatures = _guardianSignatures();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
        vm.warp(block.timestamp + 8 days + 1);

        bytes32 rescheduledOperationId =
            recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);

        assertEq(rescheduledOperationId, operationId);
    }

    function testExpiredRecoveryCanStartNewAttemptWithDifferentSalt() public {
        bytes32 nextSalt = keccak256("next recovery attempt");
        bytes[] memory signatures = _guardianSignatures();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
        vm.warp(block.timestamp + 8 days + 1);

        bytes[] memory nextSignatures = _guardianSignatures(nextSalt);
        bytes32 nextOperationId =
            recovery.scheduleRecovery(address(account), nextSalt, MODE, executionCalldata, nextSignatures);

        assertNotEq(nextOperationId, operationId);
    }

    function testCanceledRecoveryCannotBeRescheduledWithOldSignature() public {
        bytes[] memory signatures = _guardianSignatures();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
        vm.prank(address(account));
        recovery.cancelRecovery(address(account), SALT, MODE, executionCalldata);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryCanceledOperation.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
    }

    function testUninstallRevokesPendingRecovery() public {
        bytes[] memory signatures = _guardianSignatures();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, signatures);
        vm.prank(address(account));
        recovery.onUninstall("");

        address[] memory guardians = new address[](1);
        guardians[0] = guardian;
        vm.prank(address(account));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1)));

        vm.warp(block.timestamp + 1 days);
        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryUnknown.selector, operationId));
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
    }

    function _guardianSignatures() private view returns (bytes[] memory signatures) {
        return _guardianSignatures(SALT);
    }

    function _guardianSignatures(bytes32 salt) private view returns (bytes[] memory signatures) {
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), salt, MODE, executionCalldata);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(guardianPrivateKey, recoveryHash);

        signatures = new bytes[](1);
        signatures[0] = bytes.concat(r, s, bytes1(v));
    }
}
