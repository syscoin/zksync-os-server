// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {
    IERC7579Execution,
    IERC7579Module,
    IERC7579ModuleConfig,
    MODULE_TYPE_EXECUTOR
} from "@openzeppelin/contracts/interfaces/draft-IERC7579.sol";

contract PaliGuardianRecoveryModule is IERC7579Module {
    bytes32 public constant RECOVERY_SCHEDULE_TYPEHASH = keccak256(
        "PaliGuardianRecoverySchedule(uint256 chainId,address account,address module,bytes32 salt,bytes32 mode,bytes32 executionCalldataHash)"
    );

    struct RecoveryConfig {
        uint32 delay;
        uint32 expiration;
        uint64 threshold;
        bool installed;
    }

    struct RecoverySchedule {
        uint48 scheduledAt;
        bool executed;
        bool canceled;
    }

    struct GuardianApproval {
        address guardian;
        bytes signature;
    }

    event RecoveryScheduled(address indexed account, bytes32 indexed operationId, uint48 executableAt);
    event RecoveryCanceled(address indexed account, bytes32 indexed operationId);
    event RecoveryExecuted(address indexed account, bytes32 indexed operationId);

    error DuplicateGuardian(address guardian);
    error InvalidGuardian(address guardian);
    error InvalidGuardianThreshold(uint64 guardianCount, uint64 threshold);
    error RecoveryAlreadyScheduled(bytes32 operationId);
    error RecoveryCanceledOperation(bytes32 operationId);
    error RecoveryExecutedOperation(bytes32 operationId);
    error RecoveryExpired(bytes32 operationId);
    error RecoveryModuleNotInstalled(address account);
    error RecoveryNotReady(bytes32 operationId);
    error RecoveryUnknown(bytes32 operationId);
    error UnauthorizedRecoveryCancel(address sender);
    error UnauthorizedRecoverySchedule(address sender);

    mapping(address account => address[]) private _guardians;
    mapping(address account => mapping(address guardian => bool)) private _isGuardian;
    mapping(address account => RecoveryConfig) private _configs;
    mapping(address account => bytes32 operationId) private _activeRecovery;
    mapping(bytes32 operationId => RecoverySchedule) private _schedules;

    function isModuleType(uint256 moduleTypeId) external pure override returns (bool) {
        return moduleTypeId == MODULE_TYPE_EXECUTOR;
    }

    function isInitialized(address account) external view returns (bool) {
        return _configs[account].installed;
    }

    function onInstall(bytes calldata initData) external override {
        (uint32 delay, uint32 expiration, address[] memory guardians_, uint64 threshold_) =
            abi.decode(initData, (uint32, uint32, address[], uint64));
        _setGuardians(msg.sender, guardians_, threshold_);
        _configs[msg.sender] = RecoveryConfig({
            delay: delay,
            expiration: expiration == 0 ? uint32(7 days) : expiration,
            threshold: threshold_,
            installed: true
        });
    }

    function onUninstall(bytes calldata) external override {
        address[] storage guardians_ = _guardians[msg.sender];
        for (uint256 i = 0; i < guardians_.length; ++i) {
            delete _isGuardian[msg.sender][guardians_[i]];
        }
        bytes32 activeOperationId = _activeRecovery[msg.sender];
        if (activeOperationId != bytes32(0)) {
            _schedules[activeOperationId].canceled = true;
            delete _activeRecovery[msg.sender];
        }
        delete _guardians[msg.sender];
        delete _configs[msg.sender];
    }

    function guardians(address account) external view returns (address[] memory) {
        return _guardians[account];
    }

    function config(address account) external view returns (RecoveryConfig memory) {
        return _configs[account];
    }

    function isGuardian(address account, address guardian) public view returns (bool) {
        return _isGuardian[account][guardian];
    }

    function scheduleRecovery(
        address account,
        bytes32 salt,
        bytes32 mode,
        bytes calldata executionCalldata,
        GuardianApproval[] calldata approvals
    ) external returns (bytes32 operationId) {
        RecoveryConfig memory config_ = _installedConfig(account);
        bytes32 recoveryHash = getRecoveryScheduleHash(account, salt, mode, executionCalldata);
        if (!_validateGuardianApprovals(account, recoveryHash, approvals)) {
            revert UnauthorizedRecoverySchedule(msg.sender);
        }

        operationId = getOperationId(account, salt, mode, executionCalldata);
        bytes32 activeOperationId = _activeRecovery[account];
        if (activeOperationId != bytes32(0) && activeOperationId != operationId) {
            RecoverySchedule storage activeSchedule = _schedules[activeOperationId];
            if (
                activeSchedule.scheduledAt != 0 && !activeSchedule.executed && !activeSchedule.canceled
                    && !_isExpired(activeSchedule, config_)
            ) {
                revert RecoveryAlreadyScheduled(activeOperationId);
            }
        }

        RecoverySchedule storage schedule = _schedules[operationId];
        if (schedule.scheduledAt != 0) {
            if (schedule.executed) {
                revert RecoveryExecutedOperation(operationId);
            }
            if (schedule.canceled) {
                revert RecoveryCanceledOperation(operationId);
            }
            if (_isExpired(schedule, config_)) {
                revert RecoveryExpired(operationId);
            }
            revert RecoveryAlreadyScheduled(operationId);
        }

        uint48 scheduledAt = uint48(block.timestamp);
        schedule.scheduledAt = scheduledAt;
        schedule.executed = false;
        schedule.canceled = false;
        _activeRecovery[account] = operationId;
        emit RecoveryScheduled(account, operationId, scheduledAt + config_.delay);
    }

    function cancelRecovery(address account, bytes32 salt, bytes32 mode, bytes calldata executionCalldata) external {
        if (msg.sender != account) {
            revert UnauthorizedRecoveryCancel(msg.sender);
        }

        bytes32 operationId = getOperationId(account, salt, mode, executionCalldata);
        RecoverySchedule storage schedule = _schedules[operationId];
        if (schedule.scheduledAt == 0) {
            revert RecoveryUnknown(operationId);
        }
        if (schedule.executed) {
            revert RecoveryExecutedOperation(operationId);
        }
        schedule.canceled = true;
        if (_activeRecovery[account] == operationId) {
            delete _activeRecovery[account];
        }
        emit RecoveryCanceled(account, operationId);
    }

    function executeRecovery(address account, bytes32 salt, bytes32 mode, bytes calldata executionCalldata)
        external
        returns (bytes[] memory returnData)
    {
        RecoveryConfig memory config_ = _installedConfig(account);
        bytes32 operationId = getOperationId(account, salt, mode, executionCalldata);
        RecoverySchedule storage schedule = _schedules[operationId];
        if (schedule.scheduledAt == 0) {
            revert RecoveryUnknown(operationId);
        }
        if (schedule.canceled) {
            revert RecoveryCanceledOperation(operationId);
        }
        if (schedule.executed) {
            revert RecoveryExecutedOperation(operationId);
        }
        if (block.timestamp < schedule.scheduledAt + config_.delay) {
            revert RecoveryNotReady(operationId);
        }
        if (block.timestamp >= schedule.scheduledAt + config_.delay + config_.expiration) {
            revert RecoveryExpired(operationId);
        }

        schedule.executed = true;
        if (_activeRecovery[account] == operationId) {
            delete _activeRecovery[account];
        }
        emit RecoveryExecuted(account, operationId);
        return IERC7579Execution(account).executeFromExecutor(mode, executionCalldata);
    }

    function _isExpired(RecoverySchedule memory schedule, RecoveryConfig memory config_) private view returns (bool) {
        return block.timestamp >= schedule.scheduledAt + config_.delay + config_.expiration;
    }

    function getOperationId(address account, bytes32 salt, bytes32 mode, bytes calldata executionCalldata)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(account, salt, mode, executionCalldata));
    }

    function getRecoveryScheduleHash(address account, bytes32 salt, bytes32 mode, bytes calldata executionCalldata)
        public
        view
        returns (bytes32)
    {
        return keccak256(
            abi.encode(
                RECOVERY_SCHEDULE_TYPEHASH,
                block.chainid,
                account,
                address(this),
                salt,
                mode,
                keccak256(executionCalldata)
            )
        );
    }

    function _installedConfig(address account) private view returns (RecoveryConfig memory config_) {
        if (!IERC7579ModuleConfig(account).isModuleInstalled(MODULE_TYPE_EXECUTOR, address(this), "")) {
            revert RecoveryModuleNotInstalled(account);
        }
        config_ = _configs[account];
        if (!config_.installed) {
            revert RecoveryModuleNotInstalled(account);
        }
    }

    function _setGuardians(address account, address[] memory guardians_, uint64 threshold_) private {
        if (threshold_ == 0 || threshold_ > guardians_.length) {
            revert InvalidGuardianThreshold(uint64(guardians_.length), threshold_);
        }

        for (uint256 i = 0; i < guardians_.length; ++i) {
            address guardian = guardians_[i];
            if (guardian == address(0)) {
                revert InvalidGuardian(guardian);
            }
            if (_isGuardian[account][guardian]) {
                revert DuplicateGuardian(guardian);
            }
            _isGuardian[account][guardian] = true;
            _guardians[account].push(guardian);
        }
    }

    function _validateGuardianApprovals(address account, bytes32 recoveryHash, GuardianApproval[] calldata approvals)
        private
        view
        returns (bool)
    {
        RecoveryConfig memory config_ = _configs[account];
        if (approvals.length < config_.threshold) {
            return false;
        }

        address[] memory seen = new address[](approvals.length);
        uint64 validSignatures;
        for (uint256 i = 0; i < approvals.length; ++i) {
            GuardianApproval calldata approval = approvals[i];
            address guardian = approval.guardian;
            if (
                !_isGuardian[account][guardian] || _contains(seen, i, guardian)
                    || !SignatureChecker.isValidSignatureNowCalldata(guardian, recoveryHash, approval.signature)
            ) {
                return false;
            }

            seen[i] = guardian;
            unchecked {
                ++validSignatures;
            }
            if (validSignatures >= config_.threshold) {
                return true;
            }
        }

        return false;
    }

    function _contains(address[] memory values, uint256 length, address value) private pure returns (bool) {
        for (uint256 i = 0; i < length; ++i) {
            if (values[i] == value) {
                return true;
            }
        }
        return false;
    }
}
