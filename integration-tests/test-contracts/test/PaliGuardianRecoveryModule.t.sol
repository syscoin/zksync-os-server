// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {MODULE_TYPE_EXECUTOR} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";
import {IEntryPoint} from "@openzeppelin/contracts/interfaces/draft-IERC4337.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Test} from "forge-std/Test.sol";
import {PaliECDSAValidatorModule} from "contracts/src/pali/PaliECDSAValidatorModule.sol";
import {PaliGuardianRecoveryModule} from "contracts/src/pali/PaliGuardianRecoveryModule.sol";
import {PaliSmartAccount} from "contracts/src/pali/PaliSmartAccount.sol";

contract MockRecoveryAccount {
    address public recoveryModule;
    uint256 public executionCount;
    bytes32 public lastMode;
    bytes public lastExecutionCalldata;

    constructor(address recoveryModule_) {
        recoveryModule = recoveryModule_;
    }

    function isModuleInstalled(uint256 moduleTypeId, address module, bytes calldata) external view returns (bool) {
        return moduleTypeId == MODULE_TYPE_EXECUTOR && module == recoveryModule;
    }

    function executeFromExecutor(bytes32 mode, bytes calldata executionCalldata)
        external
        returns (bytes[] memory returnData)
    {
        require(msg.sender == recoveryModule, "unauthorized executor");
        ++executionCount;
        lastMode = mode;
        lastExecutionCalldata = executionCalldata;
        returnData = new bytes[](1);
        returnData[0] = executionCalldata;
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

        bytes32 oldOperationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.prank(address(account));
        recovery.onUninstall("");

        address[] memory guardians = new address[](1);
        guardians[0] = guardian;
        vm.prank(address(account));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(1)));

        bytes32 operationId = recovery.getOperationId(address(account), SALT, MODE, executionCalldata);
        assertNotEq(operationId, oldOperationId);
        vm.warp(block.timestamp + 1 days);
        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryUnknown.selector, operationId));
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);

        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        assertEq(account.executionCount(), 0);
    }

    function testPolicyEpochPersistsAcrossUninstallAndReinstall() public {
        assertEq(recovery.policyEpoch(address(account)), 1);
        vm.prank(address(account));
        recovery.onUninstall("");

        assertEq(recovery.policyEpoch(address(account)), 2);
        assertFalse(recovery.isInitialized(address(account)));
        assertFalse(recovery.isGuardian(address(account), guardian));
        assertEq(recovery.guardians(address(account)).length, 0);
        PaliGuardianRecoveryModule.RecoveryConfig memory cleared = recovery.config(address(account));
        assertEq(cleared.delay, 0);
        assertEq(cleared.expiration, 0);
        assertEq(cleared.threshold, 0);

        _installSingleGuardian(account, guardian, 1 days);
        assertEq(recovery.policyEpoch(address(account)), 3);
        assertTrue(recovery.isInitialized(address(account)));
    }

    function testHashesBindTheCurrentPolicyEpoch() public view {
        bytes32 typehash = keccak256(
            "PaliGuardianRecoverySchedule(uint256 chainId,address account,address module,uint256 policyEpoch,bytes32 salt,bytes32 mode,bytes32 executionCalldataHash)"
        );
        assertEq(recovery.RECOVERY_SCHEDULE_TYPEHASH(), typehash);
        assertEq(
            recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata),
            keccak256(
                abi.encode(
                    typehash,
                    block.chainid,
                    address(account),
                    address(recovery),
                    uint256(1),
                    SALT,
                    MODE,
                    keccak256(executionCalldata)
                )
            )
        );
        assertEq(
            recovery.getOperationId(address(account), SALT, MODE, executionCalldata),
            keccak256(abi.encode(address(account), uint256(1), SALT, MODE, executionCalldata))
        );
    }

    function testUnscheduledApprovalCannotSurviveReinstallWithSameGuardians() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory oldApprovals = _guardianApprovals();
        bytes32 oldHash = recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata);
        bytes32 oldOperationId = recovery.getOperationId(address(account), SALT, MODE, executionCalldata);

        vm.prank(address(account));
        recovery.onUninstall("");
        _installSingleGuardian(account, guardian, 1 days);

        assertNotEq(recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata), oldHash);
        assertNotEq(recovery.getOperationId(address(account), SALT, MODE, executionCalldata), oldOperationId);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, oldApprovals);

        PaliGuardianRecoveryModule.GuardianApproval[] memory freshApprovals = _guardianApprovals();
        bytes32 freshOperationId =
            recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, freshApprovals);
        assertNotEq(freshOperationId, oldOperationId);
        vm.warp(block.timestamp + 1 days);
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 1);
    }

    function testDirectPolicyReplacementInvalidatesPendingRecoveryAndOldApproval() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory oldApprovals = _guardianApprovals();
        bytes32 oldOperationId =
            recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, oldApprovals);

        _installSingleGuardian(account, guardian, 0);
        assertEq(recovery.policyEpoch(address(account)), 2);
        assertEq(recovery.guardians(address(account)).length, 1);
        bytes32 newOperationId = recovery.getOperationId(address(account), SALT, MODE, executionCalldata);
        assertNotEq(newOperationId, oldOperationId);
        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryUnknown.selector, newOperationId));
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, oldApprovals);

        PaliGuardianRecoveryModule.GuardianApproval[] memory freshApprovals = _guardianApprovals();
        assertEq(
            recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, freshApprovals), newOperationId
        );
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 1);
    }

    function testDirectPolicyReplacementRemovesPreviousGuardians() public {
        uint256 replacementKey = 0xB0B;
        address replacementGuardian = vm.addr(replacementKey);
        _installSingleGuardian(account, replacementGuardian, 2 days);

        assertFalse(recovery.isGuardian(address(account), guardian));
        assertTrue(recovery.isGuardian(address(account), replacementGuardian));
        address[] memory guardians = recovery.guardians(address(account));
        assertEq(guardians.length, 1);
        assertEq(guardians[0], replacementGuardian);
        assertEq(recovery.config(address(account)).delay, 2 days);

        PaliGuardianRecoveryModule.GuardianApproval[] memory removedGuardianApprovals = _guardianApprovals();
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, removedGuardianApprovals);

        PaliGuardianRecoveryModule.GuardianApproval[] memory freshApprovals =
            new PaliGuardianRecoveryModule.GuardianApproval[](1);
        freshApprovals[0] = _approvalFor(account, SALT, replacementKey);
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, freshApprovals);
    }

    function testInvalidReplacementRestoresPolicyEpochGuardiansAndPendingRecovery() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();
        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata);
        address candidateGuardian = vm.addr(0xB0B);
        address[] memory invalidGuardians = new address[](2);
        invalidGuardians[0] = candidateGuardian;
        invalidGuardians[1] = candidateGuardian;

        vm.prank(address(account));
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.DuplicateGuardian.selector, candidateGuardian)
        );
        recovery.onInstall(abi.encode(uint32(0), uint32(1), invalidGuardians, uint64(1)));
        invalidGuardians[1] = address(0);
        vm.prank(address(account));
        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.InvalidGuardian.selector, address(0)));
        recovery.onInstall(abi.encode(uint32(0), uint32(1), invalidGuardians, uint64(1)));
        vm.prank(address(account));
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.InvalidGuardianThreshold.selector, uint64(2), uint64(0))
        );
        recovery.onInstall(abi.encode(uint32(0), uint32(1), invalidGuardians, uint64(0)));

        assertEq(recovery.policyEpoch(address(account)), 1);
        assertTrue(recovery.isGuardian(address(account), guardian));
        assertFalse(recovery.isGuardian(address(account), candidateGuardian));
        assertEq(recovery.guardians(address(account)).length, 1);
        PaliGuardianRecoveryModule.RecoveryConfig memory config = recovery.config(address(account));
        assertEq(config.delay, 1 days);
        assertEq(config.expiration, 7 days);
        assertEq(config.threshold, 1);
        assertTrue(config.installed);
        assertEq(recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata), recoveryHash);
        assertEq(recovery.getOperationId(address(account), SALT, MODE, executionCalldata), operationId);

        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryNotReady.selector, operationId));
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        vm.warp(block.timestamp + 1 days);
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 1);
    }

    function testPolicyEpochAndSchedulesAreIsolatedBetweenAccounts() public {
        MockRecoveryAccount otherAccount = new MockRecoveryAccount(address(recovery));
        _installSingleGuardian(otherAccount, guardian, 1 days);
        PaliGuardianRecoveryModule.GuardianApproval[] memory otherApprovals =
            new PaliGuardianRecoveryModule.GuardianApproval[](1);
        otherApprovals[0] = _approvalFor(otherAccount, SALT, guardianPrivateKey);
        bytes32 otherHash = recovery.getRecoveryScheduleHash(address(otherAccount), SALT, MODE, executionCalldata);
        bytes32 otherOperationId =
            recovery.scheduleRecovery(address(otherAccount), SALT, MODE, executionCalldata, otherApprovals);

        vm.prank(address(account));
        recovery.onUninstall("");
        _installSingleGuardian(account, guardian, 0);

        assertEq(recovery.policyEpoch(address(account)), 3);
        assertEq(recovery.policyEpoch(address(otherAccount)), 1);
        assertEq(recovery.getRecoveryScheduleHash(address(otherAccount), SALT, MODE, executionCalldata), otherHash);
        assertEq(recovery.getOperationId(address(otherAccount), SALT, MODE, executionCalldata), otherOperationId);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, otherApprovals);

        vm.warp(block.timestamp + 1 days);
        recovery.executeRecovery(address(otherAccount), SALT, MODE, executionCalldata);
        assertEq(otherAccount.executionCount(), 1);
        assertEq(account.executionCount(), 0);
    }

    function testDelayExecutionAndReplayProtectionRemainEnforced() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();
        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.warp(block.timestamp + 1 days - 1);
        vm.expectRevert(abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryNotReady.selector, operationId));
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);

        vm.warp(block.timestamp + 1);
        bytes[] memory result = recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(result.length, 1);
        assertEq(result[0], executionCalldata);
        assertEq(account.lastMode(), MODE);
        assertEq(account.lastExecutionCalldata(), executionCalldata);
        assertEq(account.executionCount(), 1);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryExecutedOperation.selector, operationId)
        );
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryExecutedOperation.selector, operationId)
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testOnlyAccountCanCancelAndCanceledRecoveryCannotExecute() public {
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();
        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoveryCancel.selector, address(this))
        );
        recovery.cancelRecovery(address(account), SALT, MODE, executionCalldata);
        vm.prank(address(account));
        recovery.cancelRecovery(address(account), SALT, MODE, executionCalldata);
        vm.warp(block.timestamp + 1 days);
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.RecoveryCanceledOperation.selector, operationId)
        );
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 0);
    }

    function testThresholdStillRequiresDistinctCurrentGuardianApprovals() public {
        uint256 otherGuardianKey = 0xB0B;
        address[] memory guardians = new address[](2);
        guardians[0] = guardian;
        guardians[1] = vm.addr(otherGuardianKey);
        vm.prank(address(account));
        recovery.onInstall(abi.encode(uint32(1 days), uint32(7 days), guardians, uint64(2)));
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _guardianApprovals();
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);

        approvals = new PaliGuardianRecoveryModule.GuardianApproval[](2);
        approvals[0] = _approvalFor(account, SALT, guardianPrivateKey);
        approvals[1] = approvals[0];
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);

        approvals[1] = _approvalFor(account, SALT, otherGuardianKey);
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        vm.warp(block.timestamp + 1 days);
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 1);
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

    function testPaliSmartAccountGuardianRequiresERC7739WrappedRecoveryApproval() public {
        (PaliSmartAccount smartGuardian, PaliECDSAValidatorModule validator) = _deploySmartAccountGuardian();
        _installSingleGuardian(account, address(smartGuardian), 1 days);
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata);
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals =
            _smartAccountGuardianApprovals(smartGuardian, validator, recoveryHash);

        assertEq(smartGuardian.isValidSignature(recoveryHash, approvals[0].signature), bytes4(0xffffffff));
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);

        approvals = _smartAccountGuardianApprovals(
            smartGuardian, validator, _personalSignHash(smartGuardian.domainSeparator(), recoveryHash)
        );
        assertEq(smartGuardian.isValidSignature(recoveryHash, approvals[0].signature), bytes4(0x1626ba7e));
        bytes32 operationId = recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
        assertEq(operationId, recovery.getOperationId(address(account), SALT, MODE, executionCalldata));

        vm.warp(block.timestamp + 1 days);
        recovery.executeRecovery(address(account), SALT, MODE, executionCalldata);
        assertEq(account.executionCount(), 1);
    }

    function testPaliSmartAccountGuardianRejectsApprovalForAnotherGuardianAccount() public {
        (PaliSmartAccount smartGuardian, PaliECDSAValidatorModule validator) = _deploySmartAccountGuardian();
        (PaliSmartAccount otherGuardian,) = _deploySmartAccountGuardian();
        _installSingleGuardian(account, address(smartGuardian), 1 days);
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata);
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals = _smartAccountGuardianApprovals(
            smartGuardian, validator, _personalSignHash(otherGuardian.domainSeparator(), recoveryHash)
        );

        assertEq(smartGuardian.isValidSignature(recoveryHash, approvals[0].signature), bytes4(0xffffffff));
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function testPaliSmartAccountGuardianRejectsApprovalForAnotherChain() public {
        (PaliSmartAccount smartGuardian, PaliECDSAValidatorModule validator) = _deploySmartAccountGuardian();
        _installSingleGuardian(account, address(smartGuardian), 1 days);
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(account), SALT, MODE, executionCalldata);
        bytes32 otherChainDomain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("pali.smart-account.erc1271"),
                keccak256("1"),
                block.chainid + 1,
                address(smartGuardian)
            )
        );
        assertNotEq(otherChainDomain, smartGuardian.domainSeparator());
        PaliGuardianRecoveryModule.GuardianApproval[] memory approvals =
            _smartAccountGuardianApprovals(smartGuardian, validator, _personalSignHash(otherChainDomain, recoveryHash));

        assertEq(smartGuardian.isValidSignature(recoveryHash, approvals[0].signature), bytes4(0xffffffff));
        vm.expectRevert(
            abi.encodeWithSelector(PaliGuardianRecoveryModule.UnauthorizedRecoverySchedule.selector, address(this))
        );
        recovery.scheduleRecovery(address(account), SALT, MODE, executionCalldata, approvals);
    }

    function _deploySmartAccountGuardian()
        private
        returns (PaliSmartAccount smartGuardian, PaliECDSAValidatorModule validator)
    {
        validator = new PaliECDSAValidatorModule();
        PaliSmartAccount implementation = new PaliSmartAccount(IEntryPoint(address(0x4337)));
        address[] memory owners = new address[](1);
        owners[0] = guardian;
        PaliSmartAccount.ModuleInit[] memory validators = new PaliSmartAccount.ModuleInit[](1);
        validators[0] = PaliSmartAccount.ModuleInit({module: address(validator), data: abi.encode(owners, uint64(1))});
        PaliSmartAccount.ModuleInit[] memory empty = new PaliSmartAccount.ModuleInit[](0);
        PaliSmartAccount.ModuleInit memory fallbackHandler;
        bytes memory initCode = abi.encode(validators, empty, fallbackHandler, empty);
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(implementation), abi.encodeCall(PaliSmartAccount.initializeAccount, (initCode)));
        smartGuardian = PaliSmartAccount(payable(address(proxy)));
    }

    function _smartAccountGuardianApprovals(
        PaliSmartAccount smartGuardian,
        PaliECDSAValidatorModule validator,
        bytes32 signingHash
    ) private view returns (PaliGuardianRecoveryModule.GuardianApproval[] memory approvals) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(guardianPrivateKey, signingHash);
        approvals = new PaliGuardianRecoveryModule.GuardianApproval[](1);
        approvals[0] = PaliGuardianRecoveryModule.GuardianApproval({
            guardian: address(smartGuardian), signature: abi.encodePacked(address(validator), r, s, bytes1(v))
        });
    }

    function _personalSignHash(bytes32 domainSeparator, bytes32 hash) private pure returns (bytes32) {
        bytes32 structHash = keccak256(abi.encode(keccak256("PersonalSign(bytes prefixed)"), hash));
        return keccak256(abi.encodePacked(hex"1901", domainSeparator, structHash));
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
        approvals = new PaliGuardianRecoveryModule.GuardianApproval[](1);
        approvals[0] = _approvalFor(account, salt, guardianPrivateKey);
    }

    function _approvalFor(MockRecoveryAccount target, bytes32 salt, uint256 privateKey)
        private
        view
        returns (PaliGuardianRecoveryModule.GuardianApproval memory)
    {
        bytes32 recoveryHash = recovery.getRecoveryScheduleHash(address(target), salt, MODE, executionCalldata);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, recoveryHash);
        return PaliGuardianRecoveryModule.GuardianApproval({
            guardian: vm.addr(privateKey), signature: bytes.concat(r, s, bytes1(v))
        });
    }

    function _installSingleGuardian(MockRecoveryAccount target, address guardian_, uint32 delay) private {
        address[] memory guardians = new address[](1);
        guardians[0] = guardian_;
        vm.prank(address(target));
        recovery.onInstall(abi.encode(delay, uint32(7 days), guardians, uint64(1)));
    }
}
