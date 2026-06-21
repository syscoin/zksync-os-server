// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {PaliSmartAccount} from "./PaliSmartAccount.sol";

contract PaliSmartAccountFactory {
    struct ModuleInit {
        address module;
        bytes data;
    }

    event AccountCreated(address indexed account, bytes32 indexed salt, bytes32 indexed initCodeHash);

    error AccountPrefundFailed(address account, uint256 amount);

    address public immutable implementation;

    constructor(address implementation_) {
        implementation = implementation_;
    }

    function createAccount(bytes32 salt, bytes memory initCode) public payable returns (address account) {
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
    )
        external
        payable
        returns (address account)
    {
        return createAccount(salt, getInitData(validators, executors, fallbackHandler, hooks));
    }

    function getAddress(bytes32 salt, bytes memory initCode) public view returns (address) {
        return Create2.computeAddress(salt, keccak256(_deploymentCode(initCode)));
    }

    function getInitData(bytes memory initCode) public pure returns (bytes memory) {
        return initCode;
    }

    function getInitData(
        address validator,
        bytes memory initData
    )
        public
        pure
        returns (bytes memory)
    {
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
    )
        public
        pure
        returns (bytes memory)
    {
        return abi.encode(validators, executors, fallbackHandler, hooks);
    }

    function _deploymentCode(bytes memory initCode) internal view returns (bytes memory) {
        return abi.encodePacked(
            type(ERC1967Proxy).creationCode,
            abi.encode(
                implementation,
                abi.encodeCall(PaliSmartAccount.initializeAccount, (initCode))
            )
        );
    }
}
