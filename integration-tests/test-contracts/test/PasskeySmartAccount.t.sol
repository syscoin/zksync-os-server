// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {Base64Url} from "../src/passkey/Base64Url.sol";
import {PasskeySmartAccount} from "../src/passkey/PasskeySmartAccount.sol";
import {PasskeySmartAccountFactory} from "../src/passkey/PasskeySmartAccountFactory.sol";

interface Vm {
    function addr(uint256 privateKey) external returns (address);
    function deal(address who, uint256 newBalance) external;
    function etch(address where, bytes calldata code) external;
    function expectRevert(bytes calldata revertData) external;
    function expectRevert(bytes4 revertData) external;
    function sign(uint256 privateKey, bytes32 digest) external returns (uint8 v, bytes32 r, bytes32 s);
}

contract P256MockOk {
    fallback() external {
        assembly {
            mstore(0, 1)
            return(0, 32)
        }
    }
}

contract P256MockInvalid {
    fallback() external {
        assembly {
            return(0, 0)
        }
    }
}

contract Receiver {
    uint256 public received;

    receive() external payable {
        received += msg.value;
    }
}

contract PasskeySmartAccountTest {
    Vm internal constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    bytes32 internal constant PASSKEY_X = bytes32(uint256(1));
    bytes32 internal constant PASSKEY_Y = bytes32(uint256(2));
    bytes32 internal constant CREDENTIAL_ID_HASH = keccak256("credential");
    bytes32 internal constant RP_ID_HASH = bytes32(0);
    string internal constant ORIGIN = "chrome-extension://pali";
    bytes32 internal constant ORIGIN_HASH = keccak256(bytes(ORIGIN));
    uint256 internal constant ORIGIN_LENGTH = 23;
    bytes32 internal constant URL_HASH = keccak256("https://sponsor.example/user/123");
    bytes32 internal constant HIGH_S =
        bytes32(uint256(0x8000000000000000000000000000000000000000000000000000000000000000));

    PasskeySmartAccount internal account;
    Receiver internal receiver;
    address internal sponsor;
    uint256 internal sponsorKey;

    function setUp() public {
        vm.etch(address(uint160(0x100)), address(new P256MockOk()).code);
        sponsorKey = 0xA11CE;
        sponsor = vm.addr(sponsorKey);
        account = new PasskeySmartAccount(
            PASSKEY_X,
            PASSKEY_Y,
            CREDENTIAL_ID_HASH,
            RP_ID_HASH,
            ORIGIN_HASH,
            ORIGIN_LENGTH,
            PasskeySmartAccount.SponsorMode.None,
            address(0),
            bytes32(0)
        );
        receiver = new Receiver();
        vm.deal(address(account), 10 ether);
    }

    function testExecuteWithValidWebAuthnProof() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));

        account.execute(execution, proof, _emptySponsorProof());

        require(receiver.received() == 1 ether, "receiver value");
        require(account.nonce() == 1, "nonce");
    }

    function testReplayFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        account.execute(execution, proof, _emptySponsorProof());

        vm.expectRevert(abi.encodeWithSelector(PasskeySmartAccount.BadNonce.selector, 1, 0));
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testWrongChallengeFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(keccak256("wrong action"));

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testChallengeSubstringFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        bytes memory expectedChallenge = Base64Url.encode32(account.getActionHash(execution));
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"AAAA');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        proof.clientDataJSON = bytes.concat(prefix, expectedChallenge, suffix);
        proof.challengeOffset = prefix.length - 4;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testWrongOriginFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        bytes memory challenge = Base64Url.encode32(account.getActionHash(execution));
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"');
        bytes memory between = bytes('","origin":"https://evil.example"}');
        proof.clientDataJSON = bytes.concat(prefix, challenge, between);
        proof.originOffset = prefix.length + challenge.length + bytes('","origin":"').length;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testWrongTypeFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        bytes memory challenge = Base64Url.encode32(account.getActionHash(execution));
        bytes memory prefix = bytes('{"type":"webauthn.create","challenge":"');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        proof.clientDataJSON = bytes.concat(prefix, challenge, suffix);
        proof.typeOffset = bytes('{"type":"').length;
        proof.challengeOffset = prefix.length;
        proof.originOffset = prefix.length + challenge.length + bytes('","origin":"').length;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testInvalidP256ProofFails() public {
        vm.etch(address(uint160(0x100)), address(new P256MockInvalid()).code);
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));

        vm.expectRevert(P256InvalidSignatureSelector());
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testHighSP256ProofFailsBeforePrecompile() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        proof.s = HIGH_S;

        vm.expectRevert(P256InvalidSignatureSelector());
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testMissingUserVerificationFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        proof.authenticatorData[32] = 0x01;

        vm.expectRevert(PasskeySmartAccount.BadWebAuthnAuthenticatorData.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testWrongRpIdHashFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(execution));
        proof.authenticatorData[31] = 0x01;

        vm.expectRevert(PasskeySmartAccount.BadWebAuthnRpIdHash.selector);
        account.execute(execution, proof, _emptySponsorProof());
    }

    function testEip1271ValidSignature() public {
        bytes32 messageHash = keccak256("login to dapp");
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(messageHash);

        bytes4 magic = account.isValidSignature(messageHash, abi.encode(proof));

        require(magic == 0x1626ba7e, "1271 magic");
    }

    function testEip1271InvalidSignatureReturnsInvalidMagic() public {
        bytes4 magic = account.isValidSignature(keccak256("login to dapp"), hex"1234");

        require(magic == 0xffffffff, "1271 invalid");
    }

    function testEip1271ChallengeSubstringReturnsInvalidMagic() public {
        bytes32 messageHash = keccak256("login to dapp");
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(messageHash);
        bytes memory expectedChallenge = Base64Url.encode32(messageHash);
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"AAAA');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        proof.clientDataJSON = bytes.concat(prefix, expectedChallenge, suffix);
        proof.challengeOffset = prefix.length - 4;

        bytes4 magic = account.isValidSignature(messageHash, abi.encode(proof));

        require(magic == 0xffffffff, "1271 substring invalid");
    }

    function testEip1271HighSReturnsInvalidMagic() public {
        bytes32 messageHash = keccak256("login to dapp");
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(messageHash);
        proof.s = HIGH_S;

        bytes4 magic = account.isValidSignature(messageHash, abi.encode(proof));

        require(magic == 0xffffffff, "1271 high-s invalid");
    }

    function testRequiredSponsorMustSignSameAction() public {
        PasskeySmartAccount sponsored = new PasskeySmartAccount(
            PASSKEY_X,
            PASSKEY_Y,
            CREDENTIAL_ID_HASH,
            RP_ID_HASH,
            ORIGIN_HASH,
            ORIGIN_LENGTH,
            PasskeySmartAccount.SponsorMode.Required,
            sponsor,
            URL_HASH
        );
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(sponsored.getActionHash(execution));
        PasskeySmartAccount.SponsorProof memory sponsorProof = _sponsorProof(sponsored.getActionHash(execution));

        sponsored.execute(execution, proof, sponsorProof);

        require(receiver.received() == 1 ether, "sponsored value");
    }

    function testRequiredSponsorMissingFails() public {
        PasskeySmartAccount sponsored = new PasskeySmartAccount(
            PASSKEY_X,
            PASSKEY_Y,
            CREDENTIAL_ID_HASH,
            RP_ID_HASH,
            ORIGIN_HASH,
            ORIGIN_LENGTH,
            PasskeySmartAccount.SponsorMode.Required,
            sponsor,
            URL_HASH
        );
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(sponsored.getActionHash(execution));

        vm.expectRevert(PasskeySmartAccount.InvalidSponsor.selector);
        sponsored.execute(execution, proof, _emptySponsorProof());
    }

    function testFactoryPredictsAndDeploysAccount() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        bytes32 salt = keccak256("device one");
        address predicted = factory.getAccountAddress(
            address(this),
            PASSKEY_X,
            PASSKEY_Y,
            CREDENTIAL_ID_HASH,
            RP_ID_HASH,
            ORIGIN_HASH,
            ORIGIN_LENGTH,
            PasskeySmartAccount.SponsorMode.GasOnly,
            sponsor,
            URL_HASH,
            salt
        );

        address deployed = factory.createAccount(
            PASSKEY_X,
            PASSKEY_Y,
            CREDENTIAL_ID_HASH,
            RP_ID_HASH,
            ORIGIN_HASH,
            ORIGIN_LENGTH,
            PasskeySmartAccount.SponsorMode.GasOnly,
            sponsor,
            URL_HASH,
            salt
        );

        require(deployed == predicted, "predicted address");
    }

    function _execution(address target, uint256 value, bytes memory data, uint256 executionNonce)
        internal
        view
        returns (PasskeySmartAccount.Execution memory)
    {
        return PasskeySmartAccount.Execution({
            target: target,
            value: value,
            data: data,
            nonce: executionNonce,
            deadline: block.timestamp + 1 hours
        });
    }

    function _proof(bytes32 actionHash) internal pure returns (PasskeySmartAccount.WebAuthnProof memory) {
        bytes memory challenge = Base64Url.encode32(actionHash);
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        bytes memory authenticatorData = hex"00000000000000000000000000000000000000000000000000000000000000000500000000";

        return PasskeySmartAccount.WebAuthnProof({
            authenticatorData: authenticatorData,
            clientDataJSON: bytes.concat(prefix, challenge, suffix),
            typeOffset: bytes('{"type":"').length,
            challengeOffset: prefix.length,
            originOffset: prefix.length + challenge.length + bytes('","origin":"').length,
            r: bytes32(uint256(3)),
            s: bytes32(uint256(4))
        });
    }

    function _sponsorProof(bytes32 actionHash) internal returns (PasskeySmartAccount.SponsorProof memory) {
        bytes32 digest = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", actionHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(sponsorKey, digest);
        return PasskeySmartAccount.SponsorProof({v: v, r: r, s: s});
    }

    function _emptySponsorProof() internal pure returns (PasskeySmartAccount.SponsorProof memory) {
        return PasskeySmartAccount.SponsorProof({v: 0, r: bytes32(0), s: bytes32(0)});
    }

    function P256InvalidSignatureSelector() internal pure returns (bytes4) {
        return bytes4(keccak256("P256InvalidSignature()"));
    }
}
