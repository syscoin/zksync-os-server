// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {PasskeySmartAccount} from "./PasskeySmartAccount.sol";

contract PasskeySmartAccountFactory {
    event AccountCreated(address indexed account, bytes32 indexed credentialIdHash, bytes32 salt);

    function createAccount(
        bytes32 passkeyX,
        bytes32 passkeyY,
        bytes32 credentialIdHash,
        bytes32 rpIdHash,
        PasskeySmartAccount.SponsorMode sponsorMode,
        address sponsorSigner,
        bytes32 sponsorUrlHash,
        bytes32 salt
    ) external payable returns (address account) {
        bytes32 derivedSalt = keccak256(abi.encode(msg.sender, credentialIdHash, salt));
        bytes memory bytecode = abi.encodePacked(
            type(PasskeySmartAccount).creationCode,
            abi.encode(passkeyX, passkeyY, credentialIdHash, rpIdHash, sponsorMode, sponsorSigner, sponsorUrlHash)
        );

        assembly {
            account := create2(callvalue(), add(bytecode, 0x20), mload(bytecode), derivedSalt)
        }

        require(account != address(0), "ACCOUNT_DEPLOY_FAILED");
        emit AccountCreated(account, credentialIdHash, derivedSalt);
    }

    function getAccountAddress(
        address creator,
        bytes32 passkeyX,
        bytes32 passkeyY,
        bytes32 credentialIdHash,
        bytes32 rpIdHash,
        PasskeySmartAccount.SponsorMode sponsorMode,
        address sponsorSigner,
        bytes32 sponsorUrlHash,
        bytes32 salt
    ) external view returns (address) {
        bytes32 derivedSalt = keccak256(abi.encode(creator, credentialIdHash, salt));
        bytes32 bytecodeHash = keccak256(
            abi.encodePacked(
                type(PasskeySmartAccount).creationCode,
                abi.encode(passkeyX, passkeyY, credentialIdHash, rpIdHash, sponsorMode, sponsorSigner, sponsorUrlHash)
            )
        );

        return address(
            uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), address(this), derivedSalt, bytecodeHash))))
        );
    }
}
