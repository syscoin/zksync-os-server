// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {
    ITransparentUpgradeableProxy,
    TransparentUpgradeableProxy
} from "@openzeppelin/contracts-v4/proxy/transparent/TransparentUpgradeableProxy.sol";
import {Test} from "forge-std/Test.sol";
import {ZkSysProxyAdmin} from "contracts/src/zksys/ZkSysProxyAdmin.sol";
import {SyscoinZKSYSToken} from "contracts/src/zksys/SyscoinZKSYSToken.sol";

contract MockERC1271Wallet is IERC1271 {
    address public immutable owner;

    constructor(address owner_) {
        owner = owner_;
    }

    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4) {
        (bytes32 r, bytes32 s, uint8 v) = abi.decode(signature, (bytes32, bytes32, uint8));
        return ecrecover(hash, v, r, s) == owner ? IERC1271.isValidSignature.selector : bytes4(0xffffffff);
    }
}

contract SyscoinZKSYSTokenV2 is SyscoinZKSYSToken {
    function version() external pure returns (uint256) {
        return 2;
    }
}

contract SyscoinZKSYSTokenTest is Test {
    uint256 private constant HOLDER_KEY = 0xA11CE;
    uint256 private constant SMART_ACCOUNT_OWNER_KEY = 0xB0B;
    uint256 private constant MAX_SUPPLY = 210_000_000 ether;

    address private admin = address(0xAD);
    address private holder = vm.addr(HOLDER_KEY);
    address private smartAccountOwner = vm.addr(SMART_ACCOUNT_OWNER_KEY);
    address private delegatee = address(0xD1E6A7E);
    SyscoinZKSYSToken private implementation;
    ZkSysProxyAdmin private proxyAdmin;
    ITransparentUpgradeableProxy private proxy;
    SyscoinZKSYSToken private token;

    function setUp() public {
        implementation = new SyscoinZKSYSToken();
        proxyAdmin = new ZkSysProxyAdmin(admin);
        proxy = ITransparentUpgradeableProxy(
            address(
                new TransparentUpgradeableProxy(
                    address(implementation),
                    address(proxyAdmin),
                    abi.encodeCall(SyscoinZKSYSToken.initialize, ("ZKSYS", "ZKSYS", uint8(18), admin))
                )
            )
        );
        token = SyscoinZKSYSToken(address(proxy));
    }

    function testImplementationInitializationIsDisabled() public {
        vm.expectRevert(bytes("Initializable: contract is already initialized"));
        implementation.initialize("ZKSYS", "ZKSYS", 18, admin);
    }

    function testInitialSupplyIsZero() public view {
        assertEq(token.totalSupply(), 0);
    }

    function testProxyAdminOwnerCanUpgradeToken() public {
        SyscoinZKSYSTokenV2 upgradedImplementation = new SyscoinZKSYSTokenV2();

        vm.expectRevert(bytes("Ownable: caller is not the owner"));
        proxyAdmin.upgrade(proxy, address(upgradedImplementation));

        vm.prank(admin);
        proxyAdmin.upgrade(proxy, address(upgradedImplementation));

        assertEq(SyscoinZKSYSTokenV2(address(token)).version(), 2);
        assertEq(proxyAdmin.getProxyImplementation(proxy), address(upgradedImplementation));
        assertEq(proxyAdmin.getProxyAdmin(proxy), address(proxyAdmin));
    }

    function testInitialRolesAndRoleAdminsMatchZkTokenModel() public view {
        assertTrue(token.hasRole(token.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(token.hasRole(token.MINTER_ADMIN_ROLE(), admin));
        assertTrue(token.hasRole(token.BURNER_ADMIN_ROLE(), admin));
        assertFalse(token.hasRole(token.MINTER_ROLE(), admin));
        assertFalse(token.hasRole(token.BURNER_ROLE(), admin));
        assertEq(token.getRoleAdmin(token.MINTER_ROLE()), token.MINTER_ADMIN_ROLE());
        assertEq(token.getRoleAdmin(token.BURNER_ROLE()), token.BURNER_ADMIN_ROLE());
    }

    function testTimestampClockModeMatchesZkToken() public view {
        assertEq(token.clock(), uint48(block.timestamp));
        assertEq(token.CLOCK_MODE(), "mode=timestamp");
    }

    function testMintCapAndRoleGatedBurn() public {
        vm.startPrank(admin);
        token.grantRole(token.MINTER_ROLE(), admin);
        token.mint(holder, MAX_SUPPLY);
        assertEq(token.totalSupply(), MAX_SUPPLY);

        vm.expectRevert(
            abi.encodeWithSelector(SyscoinZKSYSToken.MaxSupplyExceeded.selector, MAX_SUPPLY + 1, MAX_SUPPLY)
        );
        token.mint(holder, 1);
        vm.stopPrank();

        vm.expectRevert(_accessControlRevert(holder, token.BURNER_ROLE()));
        vm.prank(holder);
        token.burn(holder, 1 ether);

        vm.startPrank(admin);
        token.grantRole(token.BURNER_ROLE(), admin);
        token.burn(holder, 1 ether);
        vm.stopPrank();
        assertEq(token.totalSupply(), MAX_SUPPLY - 1 ether);
    }

    function testDelegateOnBehalfWithEoaSignatureSharesNonceWithDelegateBySig() public {
        vm.startPrank(admin);
        token.grantRole(token.MINTER_ROLE(), admin);
        token.mint(holder, 100 ether);
        vm.stopPrank();

        bytes memory signature = _signDelegateOnBehalfPacked(HOLDER_KEY, holder, delegatee, 1 days);
        token.delegateOnBehalf(holder, delegatee, 1 days, signature);

        assertEq(token.delegates(holder), delegatee);
        assertEq(token.getVotes(delegatee), 100 ether);
        assertEq(token.nonces(holder), 1);

        (uint8 v, bytes32 r, bytes32 s) = vm.sign(HOLDER_KEY, _delegateBySigDigest(delegatee, 0, 2 days));
        vm.expectRevert(bytes("ERC20Votes: invalid nonce"));
        token.delegateBySig(delegatee, 0, 2 days, v, r, s);
    }

    function testDelegateOnBehalfSupportsEip1271SmartAccounts() public {
        MockERC1271Wallet wallet = new MockERC1271Wallet(smartAccountOwner);

        vm.startPrank(admin);
        token.grantRole(token.MINTER_ROLE(), admin);
        token.mint(address(wallet), 25 ether);
        vm.stopPrank();

        bytes memory signature = _signDelegateOnBehalfAbi(SMART_ACCOUNT_OWNER_KEY, address(wallet), delegatee, 1 days);
        token.delegateOnBehalf(address(wallet), delegatee, 1 days, signature);

        assertEq(token.delegates(address(wallet)), delegatee);
        assertEq(token.getVotes(delegatee), 25 ether);
        assertEq(token.nonces(address(wallet)), 1);
    }

    function _signDelegateOnBehalfPacked(uint256 privateKey, address signer, address delegatee_, uint256 expiry)
        private
        view
        returns (bytes memory)
    {
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(privateKey, _delegateOnBehalfDigest(signer, delegatee_, token.nonces(signer), expiry));
        return abi.encodePacked(r, s, v);
    }

    function _signDelegateOnBehalfAbi(uint256 privateKey, address signer, address delegatee_, uint256 expiry)
        private
        view
        returns (bytes memory)
    {
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(privateKey, _delegateOnBehalfDigest(signer, delegatee_, token.nonces(signer), expiry));
        return abi.encode(r, s, v);
    }

    function _delegateOnBehalfDigest(address signer, address delegatee_, uint256 nonce, uint256 expiry)
        private
        view
        returns (bytes32)
    {
        return keccak256(
            abi.encodePacked(
                "\x19\x01",
                token.DOMAIN_SEPARATOR(),
                keccak256(abi.encode(token.DELEGATION_TYPEHASH(), signer, delegatee_, nonce, expiry))
            )
        );
    }

    function _delegateBySigDigest(address delegatee_, uint256 nonce, uint256 expiry) private view returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "\x19\x01",
                token.DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        keccak256("Delegation(address delegatee,uint256 nonce,uint256 expiry)"),
                        delegatee_,
                        nonce,
                        expiry
                    )
                )
            )
        );
    }

    function _accessControlRevert(address account, bytes32 role) private pure returns (bytes memory) {
        return abi.encodePacked("AccessControl: account ", _toLowerHex(account), " is missing role ", _toLowerHex(role));
    }

    function _toLowerHex(address account) private pure returns (bytes memory) {
        return _toLowerHex(bytes32(uint256(uint160(account))), 20);
    }

    function _toLowerHex(bytes32 value) private pure returns (bytes memory) {
        return _toLowerHex(value, 32);
    }

    function _toLowerHex(bytes32 value, uint256 length) private pure returns (bytes memory result) {
        bytes16 symbols = "0123456789abcdef";
        result = new bytes(2 + length * 2);
        result[0] = "0";
        result[1] = "x";
        for (uint256 i = 0; i < length; ++i) {
            uint8 b = uint8(value[i + 32 - length]);
            result[2 + i * 2] = symbols[b >> 4];
            result[3 + i * 2] = symbols[b & 0x0f];
        }
    }
}
