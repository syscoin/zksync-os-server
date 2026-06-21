// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {P256Verifier} from "contracts/src/pali/P256Verifier.sol";

contract P256VerifierHarness {
    function isValid(bytes32 digest, bytes32 r, bytes32 s, bytes32 x, bytes32 y) external view returns (bool) {
        return P256Verifier.isValid(digest, r, s, x, y);
    }
}

contract SucceedingP256Precompile {
    fallback() external {
        assembly {
            mstore(0x00, 1)
            return(0x00, 0x20)
        }
    }
}

contract RevertingP256Precompile {
    fallback() external {
        revert("precompile failure");
    }
}

contract P256VerifierTest is Test {
    bytes32 internal constant DIGEST = keccak256("pali");
    bytes32 internal constant CANONICAL_R = bytes32(uint256(1));
    bytes32 internal constant CANONICAL_S = bytes32(uint256(1));
    bytes32 internal constant NON_CANONICAL_S =
        bytes32(uint256(0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632550));
    bytes32 internal constant PUBLIC_KEY_X = bytes32(uint256(2));
    bytes32 internal constant PUBLIC_KEY_Y = bytes32(uint256(3));

    P256VerifierHarness private harness;

    function setUp() public {
        harness = new P256VerifierHarness();
    }

    function testValidWhenPrecompileReturnsOne() public {
        vm.etch(address(0x100), address(new SucceedingP256Precompile()).code);

        assertTrue(harness.isValid(DIGEST, CANONICAL_R, CANONICAL_S, PUBLIC_KEY_X, PUBLIC_KEY_Y));
    }

    function testReturnsFalseWhenPrecompileReverts() public {
        vm.etch(address(0x100), address(new RevertingP256Precompile()).code);

        assertFalse(harness.isValid(DIGEST, CANONICAL_R, CANONICAL_S, PUBLIC_KEY_X, PUBLIC_KEY_Y));
    }

    function testReturnsFalseWhenPrecompileIsAbsent() public view {
        // No code at 0x100: the staticcall succeeds with empty returndata, which
        // must be treated as an invalid signature.
        assertFalse(harness.isValid(DIGEST, CANONICAL_R, CANONICAL_S, PUBLIC_KEY_X, PUBLIC_KEY_Y));
    }

    function testRejectsNonCanonicalSignatureBeforeCallingPrecompile() public {
        // A reverting precompile proves the canonicality check short-circuits.
        vm.etch(address(0x100), address(new RevertingP256Precompile()).code);

        assertFalse(harness.isValid(DIGEST, CANONICAL_R, NON_CANONICAL_S, PUBLIC_KEY_X, PUBLIC_KEY_Y));
    }
}
