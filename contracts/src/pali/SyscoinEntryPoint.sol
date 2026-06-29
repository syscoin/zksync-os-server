// SPDX-License-Identifier: GPL-3.0
pragma solidity ^0.8.28;

import {EntryPoint} from "@account-abstraction/core/EntryPoint.sol";
import {IAggregator} from "@account-abstraction/interfaces/IAggregator.sol";
import {PackedUserOperation} from "@account-abstraction/interfaces/PackedUserOperation.sol";

/// @title SyscoinEntryPoint
/// @notice ERC-4337 EntryPoint v0.9 with Syscoin-specific routing for canonical Pali-sponsored ops.
/// @dev SYSCOIN: do not edit upstream EntryPoint. Keep the routing delta isolated here.
contract SyscoinEntryPoint is EntryPoint {
    address public SYSCOIN_SPONSORED_PAYMASTER;

    event SyscoinSponsoredPaymasterBound(address indexed paymaster);

    error InvalidSyscoinSponsoredPaymaster();
    error SyscoinSponsoredPaymasterAlreadyBound(address paymaster);

    function bindSyscoinSponsoredPaymaster(address syscoinSponsoredPaymaster_) external {
        if (SYSCOIN_SPONSORED_PAYMASTER != address(0)) {
            revert SyscoinSponsoredPaymasterAlreadyBound(SYSCOIN_SPONSORED_PAYMASTER);
        }
        if (syscoinSponsoredPaymaster_ == address(0)) {
            revert InvalidSyscoinSponsoredPaymaster();
        }

        if (
            syscoinSponsoredPaymaster_.code.length != 0 || msg.sender != syscoinSponsoredPaymaster_
                || msg.sender == tx.origin
        ) {
            revert InvalidSyscoinSponsoredPaymaster();
        }

        SYSCOIN_SPONSORED_PAYMASTER = syscoinSponsoredPaymaster_;
        emit SyscoinSponsoredPaymasterBound(syscoinSponsoredPaymaster_);
    }

    /// @inheritdoc EntryPoint
    function handleOps(PackedUserOperation[] calldata ops, address payable beneficiary)
        external
        virtual
        override
        nonReentrant
    {
        uint256 opslen = ops.length;
        UserOpInfo[] memory opInfos = new UserOpInfo[](opslen);
        unchecked {
            _iterateValidationPhase(ops, opInfos, address(0), 0);

            uint256 beneficiaryCollected = 0;
            uint256 syscoinSponsoredCollected = 0;
            emit BeforeExecution();

            for (uint256 i = 0; i < opslen; i++) {
                uint256 collected = _executeUserOp(i, ops[i], opInfos[i]);
                if (_isSyscoinSponsoredOp(opInfos[i])) {
                    syscoinSponsoredCollected += collected;
                } else {
                    beneficiaryCollected += collected;
                }
            }

            _routeCompensation(beneficiary, beneficiaryCollected, syscoinSponsoredCollected);
        }
    }

    /// @inheritdoc EntryPoint
    function handleAggregatedOps(UserOpsPerAggregator[] calldata opsPerAggregator, address payable beneficiary)
        external
        virtual
        override
        nonReentrant
    {
        unchecked {
            uint256 opasLen = opsPerAggregator.length;
            uint256 totalOps = 0;
            for (uint256 i = 0; i < opasLen; i++) {
                UserOpsPerAggregator calldata opa = opsPerAggregator[i];
                PackedUserOperation[] calldata ops = opa.userOps;
                IAggregator aggregator = opa.aggregator;

                require(address(aggregator) != address(1), SignatureValidationFailed(address(aggregator)));

                if (address(aggregator) != address(0)) {
                    // solhint-disable-next-line no-empty-blocks
                    try aggregator.validateSignatures(ops, opa.signature) {}
                    catch {
                        revert SignatureValidationFailed(address(aggregator));
                    }
                }

                totalOps += ops.length;
            }

            UserOpInfo[] memory opInfos = new UserOpInfo[](totalOps);

            uint256 opIndex = 0;
            for (uint256 a = 0; a < opasLen; a++) {
                UserOpsPerAggregator calldata opa = opsPerAggregator[a];
                PackedUserOperation[] calldata ops = opa.userOps;
                IAggregator aggregator = opa.aggregator;

                opIndex += _iterateValidationPhase(ops, opInfos, address(aggregator), opIndex);
            }

            emit BeforeExecution();

            uint256 beneficiaryCollected = 0;
            uint256 syscoinSponsoredCollected = 0;
            opIndex = 0;
            for (uint256 a = 0; a < opasLen; a++) {
                UserOpsPerAggregator calldata opa = opsPerAggregator[a];
                emit SignatureAggregatorChanged(address(opa.aggregator));
                PackedUserOperation[] calldata ops = opa.userOps;
                uint256 opslen = ops.length;

                for (uint256 i = 0; i < opslen; i++) {
                    uint256 collected = _executeUserOp(opIndex, ops[i], opInfos[opIndex]);
                    if (_isSyscoinSponsoredOp(opInfos[opIndex])) {
                        syscoinSponsoredCollected += collected;
                    } else {
                        beneficiaryCollected += collected;
                    }
                    opIndex++;
                }
            }

            _routeCompensation(beneficiary, beneficiaryCollected, syscoinSponsoredCollected);
        }
    }

    function _isSyscoinSponsoredOp(UserOpInfo memory opInfo) internal view virtual returns (bool) {
        return opInfo.mUserOp.paymaster == SYSCOIN_SPONSORED_PAYMASTER;
    }

    function _routeCompensation(
        address payable beneficiary,
        uint256 beneficiaryCollected,
        uint256 syscoinSponsoredCollected
    ) internal virtual {
        // SYSCOIN: canonical Pali-sponsored reimbursement is routed back into the
        // paymaster's EntryPoint deposit instead of to the bundler beneficiary.
        if (syscoinSponsoredCollected != 0) {
            _incrementDeposit(SYSCOIN_SPONSORED_PAYMASTER, syscoinSponsoredCollected);
        }
        _compensate(beneficiary, beneficiaryCollected);
    }
}
