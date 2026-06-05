// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {PasskeySmartAccount} from "./PasskeySmartAccount.sol";

contract PasskeySmartAccountFactory {
    bytes32 internal constant PASSKEY_CREATE_TYPEHASH = keccak256("PALI_PASSKEY_SMART_ACCOUNT_CREATE_V1");

    address public immutable implementation;

    struct AccountParams {
        bytes32 passkeyX;
        bytes32 passkeyY;
        bytes32 credentialIdHash;
        bytes32 rpIdHash;
        bytes32 originHash;
        uint256 originLength;
        bytes32 salt;
    }

    event AccountCreated(address indexed account, bytes32 indexed lookupKey, bytes32 salt);

    constructor() {
        implementation = address(new PasskeySmartAccount());
    }

    function createAccount(AccountParams calldata params, PasskeySmartAccount.WebAuthnProof calldata proof)
        external
        payable
        returns (address account)
    {
        account = _createAccount(params);
        require(
            PasskeySmartAccount(payable(account)).validateSignature(getAccountCreateHash(params), abi.encode(proof)),
            "INVALID_CREATE_PROOF"
        );
    }

    function createAccountAndExecute(
        AccountParams calldata params,
        PasskeySmartAccount.Execution[] calldata executions,
        PasskeySmartAccount.WebAuthnProof calldata proof,
        PasskeySmartAccount.SponsorProof calldata sponsorProof
    ) external payable returns (address account, bytes[] memory returndata) {
        require(executions.length != 0, "MISSING_EXECUTION");
        account = _createAccount(params);
        returndata = PasskeySmartAccount(payable(account)).execute(executions, proof, sponsorProof);
    }

    mapping(bytes32 => address[]) internal accountsByPasskeyLookup;

    function getAccountCreateHash(AccountParams calldata params) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                PASSKEY_CREATE_TYPEHASH,
                block.chainid,
                getAccountAddress(params),
                params.credentialIdHash,
                params.passkeyX,
                params.passkeyY,
                params.rpIdHash,
                params.originHash,
                params.originLength,
                params.salt
            )
        );
    }

    function _createAccount(AccountParams calldata params) internal returns (address account) {
        bytes32 derivedSalt = _deriveSalt(params);
        bytes memory bytecode = _cloneCreationCode();

        assembly {
            account := create2(callvalue(), add(bytecode, 0x20), mload(bytecode), derivedSalt)
        }

        require(account != address(0), "ACCOUNT_DEPLOY_FAILED");
        PasskeySmartAccount(payable(account)).initialize(
            PasskeySmartAccount.AccountParams({
                passkeyX: params.passkeyX,
                passkeyY: params.passkeyY,
                credentialIdHash: params.credentialIdHash,
                rpIdHash: params.rpIdHash,
                originHash: params.originHash,
                originLength: params.originLength,
                salt: params.salt
            })
        );
        bytes32 lookupKey = _getAccountLookupKey(params);
        accountsByPasskeyLookup[lookupKey].push(account);
        emit AccountCreated(account, lookupKey, params.salt);
    }

    function getAccountAddress(AccountParams calldata params) public view returns (address) {
        bytes32 derivedSalt = _deriveSalt(params);
        bytes32 bytecodeHash = keccak256(_cloneCreationCode());

        return address(
            uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), address(this), derivedSalt, bytecodeHash))))
        );
    }

    function getAccountCountByPasskeyLookup(bytes32 lookupKey) external view returns (uint256) {
        return accountsByPasskeyLookup[lookupKey].length;
    }

    function getAccountsByPasskeyLookup(bytes32 lookupKey, uint256 offset, uint256 limit)
        external
        view
        returns (address[] memory accounts)
    {
        return _slice(accountsByPasskeyLookup[lookupKey], offset, limit);
    }

    function _deriveSalt(AccountParams calldata params) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                params.credentialIdHash,
                params.passkeyX,
                params.passkeyY,
                params.rpIdHash,
                params.originHash,
                params.originLength,
                params.salt
            )
        );
    }

    function _getAccountLookupKey(AccountParams calldata params) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                params.credentialIdHash,
                params.passkeyX,
                params.passkeyY,
                params.rpIdHash,
                params.originHash,
                params.originLength
            )
        );
    }

    function _cloneCreationCode() internal view returns (bytes memory) {
        return abi.encodePacked(
            hex"3d602d80600a3d3981f3",
            hex"363d3d373d3d3d363d73",
            implementation,
            hex"5af43d82803e903d91602b57fd5bf3"
        );
    }

    function _slice(address[] storage source, uint256 offset, uint256 limit)
        internal
        view
        returns (address[] memory result)
    {
        uint256 length = source.length;
        if (offset >= length || limit == 0) {
            return new address[](0);
        }

        uint256 end = offset + limit;
        if (end > length) {
            end = length;
        }

        result = new address[](end - offset);
        for (uint256 i = 0; i < result.length; ++i) {
            result[i] = source[offset + i];
        }
    }
}
