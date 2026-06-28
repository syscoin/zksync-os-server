// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {PaliSmartAccount} from "./PaliSmartAccount.sol";

interface IEntryPointSenderCreator {
    function senderCreator() external view returns (address);
}

contract PaliSmartAccountFactory {
    struct ModuleInit {
        address module;
        bytes data;
    }

    event AccountCreated(address indexed account, bytes32 indexed salt, bytes32 indexed initCodeHash);

    error AccountPrefundFailed(address account, uint256 amount);
    error InvalidAccountEntryPoint(address accountEntryPoint, address factoryEntryPoint);
    error InvalidImplementation(address implementation);
    error OnlySenderCreator(address caller);

    address public immutable implementation;
    address public immutable entryPoint;
    /// @dev Cached EntryPoint SenderCreator. Per ERC-4337, account deployment
    /// must go through the EntryPoint initCode path, which calls factories via
    /// this contract. Gating createAccount on it prevents anyone else from
    /// triggering deployments outside the 4337 flow.
    address public immutable senderCreator;

    constructor(address implementation_, address entryPoint_) {
        if (implementation_.code.length == 0) {
            revert InvalidImplementation(implementation_);
        }
        address accountEntryPoint = address(PaliSmartAccount(payable(implementation_)).entryPoint());
        if (accountEntryPoint != entryPoint_) {
            revert InvalidAccountEntryPoint(accountEntryPoint, entryPoint_);
        }
        implementation = implementation_;
        entryPoint = entryPoint_;
        senderCreator = IEntryPointSenderCreator(entryPoint_).senderCreator();
    }

    function createAccount(bytes32 salt, bytes memory initCode) public payable returns (address account) {
        if (msg.sender != senderCreator) {
            revert OnlySenderCreator(msg.sender);
        }

        bytes memory deploymentCode = _deploymentCode(initCode);
        account = Create2.computeAddress(salt, keccak256(deploymentCode));

        if (account.code.length == 0) {
            account = Create2.deploy(msg.value, salt, deploymentCode);
            emit AccountCreated(account, salt, keccak256(initCode));
        } else if (msg.value != 0) {
            (bool success,) = payable(account).call{value: msg.value}("");
            if (!success) {
                revert AccountPrefundFailed(account, msg.value);
            }
        }
    }

    function createAccountWithModules(
        bytes32 salt,
        ModuleInit[] calldata validators,
        ModuleInit[] calldata executors,
        ModuleInit calldata fallbackHandler,
        ModuleInit[] calldata hooks
    ) external payable returns (address account) {
        return createAccount(salt, getInitData(validators, executors, fallbackHandler, hooks));
    }

    function getAddress(bytes32 salt, bytes memory initCode) public view returns (address) {
        return Create2.computeAddress(salt, keccak256(_deploymentCode(initCode)));
    }

    function getInitData(bytes memory initCode) public pure returns (bytes memory) {
        return initCode;
    }

    function getInitData(address validator, bytes memory initData) public pure returns (bytes memory) {
        ModuleInit[] memory validators = new ModuleInit[](1);
        validators[0] = ModuleInit({module: validator, data: initData});
        ModuleInit[] memory executors = new ModuleInit[](0);
        ModuleInit memory fallbackHandler;
        ModuleInit[] memory hooks = new ModuleInit[](0);

        return getInitData(validators, executors, fallbackHandler, hooks);
    }

    function getInitData(
        ModuleInit[] memory validators,
        ModuleInit[] memory executors,
        ModuleInit memory fallbackHandler,
        ModuleInit[] memory hooks
    ) public pure returns (bytes memory) {
        return abi.encode(validators, executors, fallbackHandler, hooks);
    }

    function _deploymentCode(bytes memory initCode) internal view returns (bytes memory) {
        return abi.encodePacked(
            type(ERC1967Proxy).creationCode,
            abi.encode(implementation, abi.encodeCall(PaliSmartAccount.initializeAccount, (initCode)))
        );
    }
}
