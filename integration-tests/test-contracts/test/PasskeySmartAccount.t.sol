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

    bytes32 internal constant PASSKEY_EXECUTE_TYPEHASH = keccak256("PALI_PASSKEY_SMART_ACCOUNT_EXECUTE_V1");
    bytes32 internal constant PASSKEY_X = bytes32(uint256(1));
    bytes32 internal constant PASSKEY_Y = bytes32(uint256(2));
    bytes32 internal constant CREDENTIAL_ID_HASH = keccak256("credential");
    bytes32 internal constant RP_ID_HASH = bytes32(0);
    string internal constant ORIGIN = "chrome-extension://pali";
    bytes32 internal constant ORIGIN_HASH = keccak256(bytes(ORIGIN));
    uint256 internal constant ORIGIN_LENGTH = 23;
    string internal constant SPONSOR_URL = "https://sponsor.example/pali";
    bytes32 internal constant HIGH_S =
        bytes32(uint256(0x8000000000000000000000000000000000000000000000000000000000000000));
    uint256 internal constant SECP256K1_ORDER =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    PasskeySmartAccount internal account;
    Receiver internal receiver;
    address internal sponsor;
    uint256 internal sponsorKey;

    function setUp() public {
        vm.etch(address(uint160(0x100)), address(new P256MockOk()).code);
        sponsorKey = 0xA11CE;
        sponsor = vm.addr(sponsorKey);
        account = _newAccount(keccak256("default account"));
        receiver = new Receiver();
        vm.deal(address(account), 10 ether);
    }

    function testExecuteWithValidWebAuthnProof() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));

        account.execute(_single(execution), proof, _emptySponsorProof());

        require(receiver.received() == 1 ether, "receiver value");
        require(account.nonce() == 1, "nonce");
    }

    function testRecoveryMetadataReturnsAccountState() public {
        PasskeySmartAccount.RecoveryMetadata memory metadata = account.getRecoveryMetadata();

        require(metadata.passkeyX == PASSKEY_X, "passkey x");
        require(metadata.passkeyY == PASSKEY_Y, "passkey y");
        require(metadata.credentialIdHash == CREDENTIAL_ID_HASH, "credential");
        require(metadata.rpIdHash == RP_ID_HASH, "rp id");
        require(metadata.originHash == ORIGIN_HASH, "origin hash");
        require(metadata.originLength == ORIGIN_LENGTH, "origin length");
        require(metadata.sponsorMode == PasskeySmartAccount.SponsorMode.None, "sponsor mode");
        require(metadata.sponsorSigner == address(0), "sponsor signer");
        require(bytes(metadata.sponsorUrl).length == 0, "sponsor url");
    }

    function testReplayFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        account.execute(_single(execution), proof, _emptySponsorProof());

        vm.expectRevert(abi.encodeWithSelector(PasskeySmartAccount.BadNonce.selector, 1, 0));
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testWrongChallengeFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(keccak256("wrong action"));

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testChallengeSubstringFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        bytes memory expectedChallenge = Base64Url.encode32(account.getActionHash(_single(execution)));
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"AAAA');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        proof.clientDataJSON = bytes.concat(prefix, expectedChallenge, suffix);
        proof.challengeOffset = prefix.length - 4;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testWrongOriginFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        bytes memory challenge = Base64Url.encode32(account.getActionHash(_single(execution)));
        bytes memory prefix = bytes('{"type":"webauthn.get","challenge":"');
        bytes memory between = bytes('","origin":"https://evil.example"}');
        proof.clientDataJSON = bytes.concat(prefix, challenge, between);
        proof.originOffset = prefix.length + challenge.length + bytes('","origin":"').length;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testWrongTypeFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        bytes memory challenge = Base64Url.encode32(account.getActionHash(_single(execution)));
        bytes memory prefix = bytes('{"type":"webauthn.create","challenge":"');
        bytes memory suffix = bytes('","origin":"chrome-extension://pali"}');
        proof.clientDataJSON = bytes.concat(prefix, challenge, suffix);
        proof.typeOffset = bytes('{"type":"').length;
        proof.challengeOffset = prefix.length;
        proof.originOffset = prefix.length + challenge.length + bytes('","origin":"').length;

        vm.expectRevert(PasskeySmartAccount.BadChallenge.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testInvalidP256ProofFails() public {
        vm.etch(address(uint160(0x100)), address(new P256MockInvalid()).code);
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));

        vm.expectRevert(P256InvalidSignatureSelector());
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testHighSP256ProofFailsBeforePrecompile() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        proof.s = HIGH_S;

        vm.expectRevert(P256InvalidSignatureSelector());
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testMissingUserVerificationFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        proof.authenticatorData[32] = 0x01;

        vm.expectRevert(PasskeySmartAccount.BadWebAuthnAuthenticatorData.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testWrongRpIdHashFails() public {
        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", 0);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(account.getActionHash(_single(execution)));
        proof.authenticatorData[31] = 0x01;

        vm.expectRevert(PasskeySmartAccount.BadWebAuthnRpIdHash.selector);
        account.execute(_single(execution), proof, _emptySponsorProof());
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
        PasskeySmartAccount sponsored = _newAccount(keccak256("sponsored account"));
        _setSponsorByPasskey(sponsored, PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL);
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", sponsored.nonce());
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(sponsored.getActionHash(_single(execution)));
        PasskeySmartAccount.SponsorProof memory sponsorProof =
            _sponsorProof(sponsored.getActionHash(_single(execution)));

        sponsored.execute(_single(execution), proof, sponsorProof);

        require(receiver.received() == 1 ether, "sponsored value");
    }

    function testRequiredSponsorMissingFails() public {
        PasskeySmartAccount sponsored = _newAccount(keccak256("sponsor missing account"));
        _setSponsorByPasskey(sponsored, PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL);
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", sponsored.nonce());
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(sponsored.getActionHash(_single(execution)));

        vm.expectRevert(PasskeySmartAccount.InvalidSponsor.selector);
        sponsored.execute(_single(execution), proof, _emptySponsorProof());
    }

    function testRequiredSponsorHighSSignatureFails() public {
        PasskeySmartAccount sponsored = _newAccount(keccak256("sponsor high-s account"));
        _setSponsorByPasskey(sponsored, PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL);
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", sponsored.nonce());
        bytes32 actionHash = sponsored.getActionHash(_single(execution));
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);
        PasskeySmartAccount.SponsorProof memory sponsorProof = _sponsorProof(actionHash);
        sponsorProof.s = bytes32(SECP256K1_ORDER - uint256(sponsorProof.s));
        sponsorProof.v = sponsorProof.v == 27 ? 28 : 27;

        vm.expectRevert(PasskeySmartAccount.InvalidSponsor.selector);
        sponsored.execute(_single(execution), proof, sponsorProof);
    }

    function testRequiredSponsorInvalidVSignatureFails() public {
        PasskeySmartAccount sponsored = _newAccount(keccak256("sponsor invalid-v account"));
        _setSponsorByPasskey(sponsored, PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL);
        vm.deal(address(sponsored), 10 ether);

        PasskeySmartAccount.Execution memory execution = _execution(address(receiver), 1 ether, "", sponsored.nonce());
        bytes32 actionHash = sponsored.getActionHash(_single(execution));
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);
        PasskeySmartAccount.SponsorProof memory sponsorProof = _sponsorProof(actionHash);
        sponsorProof.v = 0;

        vm.expectRevert(PasskeySmartAccount.InvalidSponsor.selector);
        sponsored.execute(_single(execution), proof, sponsorProof);
    }

    function testFactoryPredictsAndDeploysAccount() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        bytes32 salt = keccak256("device one");
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(salt);
        address predicted = factory.getAccountAddress(params);

        address deployed = _createAccount(factory, params);

        require(deployed == predicted, "predicted address");
        _assertDefaultRecoveryMetadata(PasskeySmartAccount(payable(deployed)));
    }

    function testFactoryCreateRequiresPasskeyProof() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("proof required device"));

        vm.expectRevert(bytes("INVALID_CREATE_PROOF"));
        factory.createAccount(params, _proof(keccak256("wrong create hash")));

        require(factory.getAccountCountByPasskeyLookup(_accountLookupKey(params)) == 0, "lookup count");
    }

    function testFactoryCreateAndExecuteRejectsEmptyBatch() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("empty batch device"));
        PasskeySmartAccount.Execution[] memory executions = new PasskeySmartAccount.Execution[](0);

        vm.expectRevert(bytes("MISSING_EXECUTION"));
        factory.createAccountAndExecute(params, executions, _proof(keccak256("unused")), _emptySponsorProof());

        require(factory.getAccountCountByPasskeyLookup(_accountLookupKey(params)) == 0, "lookup count");
    }

    function testImplementationIsLocked() public {
        PasskeySmartAccount implementation = new PasskeySmartAccount();

        vm.expectRevert(PasskeySmartAccount.AlreadyInitialized.selector);
        implementation.initialize(_accountInitParams(keccak256("implementation")));
    }

    function testCloneInitializesOnlyOnce() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("device one"));
        address deployed = _createAccount(factory, params);

        vm.expectRevert(PasskeySmartAccount.AlreadyInitialized.selector);
        PasskeySmartAccount(payable(deployed)).initialize(_accountInitParams(params.salt));
    }

    function testFactoryForwardsValueToClone() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("funded device"));

        address deployed = _createAccountWithValue(factory, params, 2 ether);

        require(deployed.balance == 2 ether, "clone balance");
    }

    function testFactoryAddressCommitsToPasskeyIdentity() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        bytes32 salt = keccak256("same device");
        PasskeySmartAccountFactory.AccountParams memory userParams = _accountParams(salt);
        PasskeySmartAccountFactory.AccountParams memory attackerParams = _accountParams(salt);
        attackerParams.passkeyX = bytes32(uint256(999));

        address userPredicted = factory.getAccountAddress(userParams);
        address attackerPredicted = factory.getAccountAddress(attackerParams);

        require(userPredicted != attackerPredicted, "identity must affect address");
        address attackerDeployed = _createAccount(factory, attackerParams);
        address userDeployed = _createAccount(factory, userParams);
        require(attackerDeployed == attackerPredicted, "attacker predicted");
        require(userDeployed == userPredicted, "user predicted");
        PasskeySmartAccount.RecoveryMetadata memory userMetadata =
            PasskeySmartAccount(payable(userDeployed)).getRecoveryMetadata();
        PasskeySmartAccount.RecoveryMetadata memory attackerMetadata =
            PasskeySmartAccount(payable(attackerDeployed)).getRecoveryMetadata();
        require(userMetadata.passkeyX == PASSKEY_X, "user passkey");
        require(attackerMetadata.passkeyX == attackerParams.passkeyX, "attacker passkey");
    }

    function testFactoryAddressExcludesSponsorPolicy() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("policy device"));
        address predicted = factory.getAccountAddress(params);
        PasskeySmartAccount accountWithPolicy = PasskeySmartAccount(payable(_createAccount(factory, params)));

        _setSponsorByPasskey(accountWithPolicy, PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL);

        require(address(accountWithPolicy) == predicted, "predicted address");
        require(factory.getAccountAddress(params) == predicted, "policy excluded");
    }

    function testFactoryRegistryLookupAndPagination() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params0 = _accountParams(keccak256("device zero"));
        PasskeySmartAccountFactory.AccountParams memory params1 = _accountParams(keccak256("device one"));
        PasskeySmartAccountFactory.AccountParams memory params2 = _accountParams(keccak256("device two"));

        address account0 = _createAccount(factory, params0);
        address account1 = _createAccount(factory, params1);
        address account2 = _createAccount(factory, params2);

        bytes32 lookupKey = _accountLookupKey(params0);
        require(lookupKey == _accountLookupKey(params1), "same lookup key");
        require(factory.getAccountCountByPasskeyLookup(lookupKey) == 3, "lookup count");

        address[] memory lookupPage = factory.getAccountsByPasskeyLookup(lookupKey, 0, 2);
        require(lookupPage.length == 2, "lookup page length");
        require(lookupPage[0] == account0, "lookup page account 0");
        require(lookupPage[1] == account1, "lookup page account 1");

        address[] memory nextPage = factory.getAccountsByPasskeyLookup(lookupKey, 2, 2);
        require(nextPage.length == 1, "next page length");
        require(nextPage[0] == account2, "next page account");

        address[] memory emptyPage = factory.getAccountsByPasskeyLookup(lookupKey, 3, 2);
        require(emptyPage.length == 0, "empty page");
    }

    function testFactoryRegistryLookupCommitsToPasskeyIdentity() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory userParams = _accountParams(keccak256("user device"));
        PasskeySmartAccountFactory.AccountParams memory attackerParams = _accountParams(keccak256("attacker device"));
        attackerParams.passkeyX = bytes32(uint256(999));

        bytes32 userLookupKey = _accountLookupKey(userParams);
        bytes32 attackerLookupKey = _accountLookupKey(attackerParams);
        require(userLookupKey != attackerLookupKey, "identity must affect lookup");

        address attackerAccount = _createAccount(factory, attackerParams);
        require(factory.getAccountCountByPasskeyLookup(userLookupKey) == 0, "user lookup polluted");
        require(factory.getAccountCountByPasskeyLookup(attackerLookupKey) == 1, "attacker lookup count");

        address[] memory attackerPage = factory.getAccountsByPasskeyLookup(attackerLookupKey, 0, 1);
        require(attackerPage.length == 1, "attacker page length");
        require(attackerPage[0] == attackerAccount, "attacker page account");
    }

    function testDuplicateCreateDoesNotPolluteRegistry() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(keccak256("duplicate device"));
        _createAccount(factory, params);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(factory.getAccountCreateHash(params));

        vm.expectRevert(bytes("ACCOUNT_DEPLOY_FAILED"));
        factory.createAccount(params, proof);

        require(factory.getAccountCountByPasskeyLookup(_accountLookupKey(params)) == 1, "lookup count");
    }

    function testFactoryDeploysAndExecutesPasskeyAuthorizedPolicy() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        bytes32 salt = keccak256("device one");
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(salt);
        address predicted = factory.getAccountAddress(params);
        PasskeySmartAccount.Execution memory execution = PasskeySmartAccount.Execution({
            target: predicted,
            value: 0,
            data: abi.encodeCall(
                PasskeySmartAccount.setSponsor, (PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL)
            ),
            nonce: 0,
            deadline: block.timestamp + 1 hours
        });
        bytes32 actionHash = _initialActionHash(predicted, _single(execution));
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);

        (address deployed,) =
            factory.createAccountAndExecute(params, _single(execution), proof, _sponsorProof(actionHash));

        require(deployed == predicted, "predicted address");
        _assertSponsorMetadata(PasskeySmartAccount(payable(deployed)));
        require(PasskeySmartAccount(payable(deployed)).nonce() == 1, "nonce");
    }

    function testFactoryDeploysAndExecutesPasskeyAuthorizedPolicyAndSendBatch() public {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        bytes32 salt = keccak256("device one");
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(salt);
        address predicted = factory.getAccountAddress(params);
        PasskeySmartAccount.Execution[] memory executions = new PasskeySmartAccount.Execution[](2);
        executions[0] = PasskeySmartAccount.Execution({
            target: predicted,
            value: 0,
            data: abi.encodeCall(
                PasskeySmartAccount.setSponsor, (PasskeySmartAccount.SponsorMode.Required, sponsor, SPONSOR_URL)
            ),
            nonce: 0,
            deadline: block.timestamp + 1 hours
        });
        executions[1] = PasskeySmartAccount.Execution({
            target: address(receiver),
            value: 1 ether,
            data: "",
            nonce: 1,
            deadline: block.timestamp + 1 hours
        });
        bytes32 actionHash = _initialActionHash(predicted, executions);
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);

        (address deployed,) =
            factory.createAccountAndExecute{value: 1 ether}(params, executions, proof, _sponsorProof(actionHash));

        require(deployed == predicted, "predicted address");
        _assertSponsorMetadata(PasskeySmartAccount(payable(deployed)));
        require(PasskeySmartAccount(payable(deployed)).nonce() == 2, "nonce");
        require(receiver.received() == 1 ether, "receiver value");
    }

    function testSponsorUrlLengthIsCapped() public {
        bytes memory longUrlBytes = new bytes(129);
        for (uint256 i = 0; i < longUrlBytes.length; ++i) {
            longUrlBytes[i] = bytes1(uint8(97));
        }
        PasskeySmartAccount.Execution memory execution = _execution(
            address(account),
            0,
            abi.encodeCall(
                PasskeySmartAccount.setSponsor,
                (PasskeySmartAccount.SponsorMode.Required, sponsor, string(longUrlBytes))
            ),
            account.nonce()
        );
        bytes32 actionHash = account.getActionHash(_single(execution));
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);
        PasskeySmartAccount.SponsorProof memory sponsorProof = _sponsorProof(actionHash);

        vm.expectRevert(
            abi.encodeWithSelector(
                PasskeySmartAccount.CallFailed.selector,
                abi.encodeWithSelector(PasskeySmartAccount.SponsorUrlTooLong.selector)
            )
        );
        account.execute(_single(execution), proof, sponsorProof);
    }

    function _setSponsorByPasskey(
        PasskeySmartAccount target,
        PasskeySmartAccount.SponsorMode mode,
        address signer,
        string memory url
    ) internal {
        PasskeySmartAccount.Execution memory execution = _execution(
            address(target), 0, abi.encodeCall(PasskeySmartAccount.setSponsor, (mode, signer, url)), target.nonce()
        );
        bytes32 actionHash = target.getActionHash(_single(execution));
        PasskeySmartAccount.WebAuthnProof memory proof = _proof(actionHash);
        PasskeySmartAccount.SponsorProof memory sponsorProof =
            mode == PasskeySmartAccount.SponsorMode.Required ? _sponsorProof(actionHash) : _emptySponsorProof();
        target.execute(_single(execution), proof, sponsorProof);
    }

    function _assertDefaultRecoveryMetadata(PasskeySmartAccount target) internal view {
        PasskeySmartAccount.RecoveryMetadata memory metadata = target.getRecoveryMetadata();
        require(metadata.passkeyX == PASSKEY_X, "passkey x");
        require(metadata.passkeyY == PASSKEY_Y, "passkey y");
        require(metadata.credentialIdHash == CREDENTIAL_ID_HASH, "credential hash");
        require(metadata.rpIdHash == RP_ID_HASH, "rp id hash");
        require(metadata.originHash == ORIGIN_HASH, "origin hash");
        require(metadata.originLength == ORIGIN_LENGTH, "origin length");
        require(metadata.sponsorMode == PasskeySmartAccount.SponsorMode.None, "sponsor mode");
        require(metadata.sponsorSigner == address(0), "sponsor signer");
        require(bytes(metadata.sponsorUrl).length == 0, "sponsor url");
    }

    function _assertSponsorMetadata(PasskeySmartAccount target) internal view {
        PasskeySmartAccount.RecoveryMetadata memory metadata = target.getRecoveryMetadata();
        require(metadata.sponsorMode == PasskeySmartAccount.SponsorMode.Required, "sponsor mode");
        require(metadata.sponsorSigner == sponsor, "sponsor signer");
        require(keccak256(bytes(metadata.sponsorUrl)) == keccak256(bytes(SPONSOR_URL)), "sponsor url");
    }

    function _initialActionHash(address targetAccount, PasskeySmartAccount.Execution[] memory executions)
        internal
        view
        returns (bytes32)
    {
        bytes32[] memory executionHashes = new bytes32[](executions.length);
        for (uint256 i = 0; i < executions.length; ++i) {
            executionHashes[i] = keccak256(
                abi.encode(
                    executions[i].target,
                    executions[i].value,
                    keccak256(executions[i].data),
                    executions[i].nonce,
                    executions[i].deadline
                )
            );
        }

        return keccak256(
            abi.encode(
                PASSKEY_EXECUTE_TYPEHASH,
                block.chainid,
                targetAccount,
                keccak256(abi.encodePacked(executionHashes)),
                PasskeySmartAccount.SponsorMode.None,
                address(0)
            )
        );
    }

    function _single(PasskeySmartAccount.Execution memory execution)
        internal
        pure
        returns (PasskeySmartAccount.Execution[] memory executions)
    {
        executions = new PasskeySmartAccount.Execution[](1);
        executions[0] = execution;
    }

    function _newAccount(bytes32 salt) internal returns (PasskeySmartAccount) {
        PasskeySmartAccountFactory factory = new PasskeySmartAccountFactory();
        PasskeySmartAccountFactory.AccountParams memory params = _accountParams(salt);
        return PasskeySmartAccount(payable(_createAccount(factory, params)));
    }

    function _createAccount(PasskeySmartAccountFactory factory, PasskeySmartAccountFactory.AccountParams memory params)
        internal
        returns (address)
    {
        return factory.createAccount(params, _proof(factory.getAccountCreateHash(params)));
    }

    function _createAccountWithValue(
        PasskeySmartAccountFactory factory,
        PasskeySmartAccountFactory.AccountParams memory params,
        uint256 value
    ) internal returns (address) {
        return factory.createAccount{value: value}(params, _proof(factory.getAccountCreateHash(params)));
    }

    function _accountParams(bytes32 salt) internal pure returns (PasskeySmartAccountFactory.AccountParams memory) {
        return PasskeySmartAccountFactory.AccountParams({
            passkeyX: PASSKEY_X,
            passkeyY: PASSKEY_Y,
            credentialIdHash: CREDENTIAL_ID_HASH,
            rpIdHash: RP_ID_HASH,
            originHash: ORIGIN_HASH,
            originLength: ORIGIN_LENGTH,
            salt: salt
        });
    }

    function _accountInitParams(bytes32 salt) internal pure returns (PasskeySmartAccount.AccountParams memory) {
        return PasskeySmartAccount.AccountParams({
            passkeyX: PASSKEY_X,
            passkeyY: PASSKEY_Y,
            credentialIdHash: CREDENTIAL_ID_HASH,
            rpIdHash: RP_ID_HASH,
            originHash: ORIGIN_HASH,
            originLength: ORIGIN_LENGTH,
            salt: salt
        });
    }

    function _accountLookupKey(PasskeySmartAccountFactory.AccountParams memory params) internal pure returns (bytes32) {
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
