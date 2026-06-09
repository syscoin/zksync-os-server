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

contract MockERC1271Guardian {
    bytes4 internal constant EIP1271_SUCCESS = 0x1626ba7e;

    bytes32 public validHash;
    bytes32 public validSignatureHash;

    function setValidSignature(bytes32 hash, bytes calldata signature) external {
        validHash = hash;
        validSignatureHash = keccak256(signature);
    }

    function isValidSignature(bytes32 hash, bytes calldata signature) external view returns (bytes4) {
        return hash == validHash && keccak256(signature) == validSignatureHash ? EIP1271_SUCCESS : bytes4(0xffffffff);
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
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryAlreadyScheduled.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testDifferentSaltCannotBypassActiveRecoveryLimit() public {
        bytes32 nextSalt = keccak256("next recovery attempt");
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();
        PaliGuardianRecoveryModule.GuardianApproval[] memory nextApprovals = _guardianApprovals(nextSalt);

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryAlreadyScheduled.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), nextSalt, MODE, executionCalldata, nextApprovals);
    }

    function testExpiredRecoveryCannotBeRescheduledWithOldApprovals() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.warp(block.timestamp + 8 days + 1);

        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryExpired.selector, operationId));
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testExpiredRecoveryCanStartNewAttemptWithDifferentSalt() public {
        bytes32 nextSalt = keccak256("next recovery attempt");
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.warp(block.timestamp + 8 days + 1);

        PaliGuardianRecoveryModule.GuardianApproval[] memory nextApprovals = _guardianApprovals(nextSalt);
        bytes32 nextOperationId =
            recovery.scheduleRecovery(address(account), nextSalt, MODE, executionCalldata, nextApprovals);

        assertNotEq(nextOperationId, operationId);
    }

    function testCanceledRecoveryCannotBeRescheduledWithOldSignature() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.prank(address(account));
        recovery.cancelRecovery(address(account), SALT, MODE, executionCalldata);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryCanceledOperation.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testUninstallRevokesPendingRecovery() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();

        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.prank(address(account));
        recovery.onUninstall("");

        address[] memory guardians = new address[](1);
        guardians[0] = guardian;
        vm.prank(address(account));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1)));

        vm.warp(block.timestamp + 1 days);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryCanceledOperation.selector, operationId)
        );
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryCanceledOperation.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testContractGuardianCanApproveRecoveryViaERC1271() public {
        MockERC1271Guardian contractGuardian = new MockERC1271Guardian();
        MockRecoveryAccount contractGuardianAccount = new MockRecoveryAccount(address(recovery));
        address[] memory guardians = new address[](1);
        guardians[0] = address(contractGuardian);

        vm.prank(address(contractGuardianAccount));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1)));

        bytes32 recoveryHash =
            recovery.getRecoveryScheduleHash(address(contractGuardianAccount), SALT, MODE, executionCalldata);
        bytes memory signature = hex"c0ffee";
        contractGuardian.setValidSignature(recoveryHash, signature);

        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals =
            new PaliGuardianRecoveryModule.GuardianApproval[](1);
        approvals[0] =
            PaliGuardianRecoveryModule.GuardianApproval({guardian: address(contractGuardian), signature: signature});

        bytes32 operationId =
            recovery.scheduleRecovery(address(contractGuardianAccount), SALT, MODE, executionCalldata, approvals);

        assertEq(operationId, recovery.getOperationId(address(contractGuardianAccount), SALT, MODE, executionCalldata));
    }

    function _guardianApprovals()
        private
        view
        returns (PaliGuardianRecoveryModule.GuardianApproval[] memory approvals)
    {
        return _guardianApprovals(SALT);
    }

    function _guardianApprovals(bytes32 salt)
        private
        view
        returns (PaliGuardianRecoveryModule.GuardianApproval[] memory approvals)
    {
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), salt, MODE, executionCalldata);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(guardianPrivateKey, recoveryHash);

        approvals = new PaliGuardianRecoveryModule.GuardianApproval[](1);
        approvals[0] =
            PaliGuardianRecoveryModule.GuardianApproval({guardian: guardian, signature: bytes.concat(r, s, bytes1(v))});
    }
}
