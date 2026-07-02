// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {SLHDSASHA212824Verifier} from "../src/pali/SLHDSASHA212824Verifier.sol";

/// @title SLH-DSA-SHA2-128-24 differential test (Solidity side)
/// @notice Runs the exact same known-answer vector and deterministic mutation
/// sweep as the Rust precompile harness in
/// `tools/slh-dsa-difftest/slh_dsa_difftest.rs`. Both sides must accept the
/// valid vector and reject every mutated/random vector, so the ZKsync OS
/// precompile at 0x101 and this Solidity fallback verifier cannot silently
/// diverge on these inputs. Keep the mutation constants in sync with the Rust
/// harness.
contract SLHDSASHA212824DifferentialTest is Test {
    uint256 internal constant SIGNATURE_LENGTH = 3856;
    // Mutation scheme shared with the Rust harness.
    uint256 internal constant MUTATION_STRIDE = 31;
    uint256 internal constant RANDOM_VECTORS = 8;
    uint64 internal constant RANDOM_SEED = 0x5EED_5EED_5EED_5EED;

    SLHDSASHA212824Verifier internal verifier;
    bytes32 internal pkSeed;
    bytes32 internal pkRoot;
    bytes32 internal message;
    bytes internal signature;

    function setUp() public {
        verifier = new SLHDSASHA212824Verifier();
        string memory json =
            vm.readFile(string.concat(vm.projectRoot(), "/test/vectors/slh_dsa_sha2_128_24_kat.json"));
        pkSeed = vm.parseJsonBytes32(json, ".pkSeed");
        pkRoot = vm.parseJsonBytes32(json, ".pkRoot");
        message = vm.parseJsonBytes32(json, ".message");
        signature = vm.parseJsonBytes(json, ".signature");
        assertEq(signature.length, SIGNATURE_LENGTH, "bad KAT fixture");
    }

    /// Signature component boundaries: R randomizer [0..16), FORS trees
    /// [16..2416) (6 trees x (16-byte sk + 24 x 16-byte auth path)), WOTS
    /// chains [2416..3504) (68 x 16), Merkle auth path [3504..3856) (22 x 16).
    function _boundaryOffsets() internal pure returns (uint256[14] memory offsets) {
        offsets = [uint256(0), 15, 16, 31, 32, 415, 416, 2415, 2416, 2431, 3503, 3504, 3519, 3855];
    }

    function _mutationMasks() internal pure returns (uint8[2] memory masks) {
        masks = [0x01, 0x80];
    }

    function test_ValidKatVerifies() public view {
        assertTrue(verifier.verify(pkSeed, pkRoot, message, signature), "valid KAT rejected");
    }

    function test_MutatedSignatureRejects() public {
        uint256[14] memory boundaries = _boundaryOffsets();
        uint8[2] memory masks = _mutationMasks();

        for (uint256 b = 0; b < boundaries.length; b++) {
            _assertSigMutationRejected(boundaries[b], masks);
        }
        for (uint256 offset = 0; offset < SIGNATURE_LENGTH; offset += MUTATION_STRIDE) {
            _assertSigMutationRejected(offset, masks);
        }
    }

    function _assertSigMutationRejected(uint256 offset, uint8[2] memory masks) internal {
        for (uint256 m = 0; m < masks.length; m++) {
            bytes1 original = signature[offset];
            signature[offset] = bytes1(uint8(original) ^ masks[m]);
            assertFalse(
                verifier.verify(pkSeed, pkRoot, message, signature),
                string.concat("sig mutation accepted at offset ", vm.toString(offset))
            );
            signature[offset] = original;
        }
    }

    function test_MutatedSeedRootMessageRejects() public view {
        uint8[2] memory masks = _mutationMasks();
        for (uint256 m = 0; m < masks.length; m++) {
            bytes32 flipLow = bytes32(uint256(masks[m]) << 248);
            bytes32 flipHigh = bytes32(uint256(masks[m]) << 128);
            // Flipping bits inside the 16 meaningful key bytes must reject.
            assertFalse(verifier.verify(pkSeed ^ flipLow, pkRoot, message, signature), "seed[0] mutation accepted");
            assertFalse(verifier.verify(pkSeed ^ flipHigh, pkRoot, message, signature), "seed[15] mutation accepted");
            assertFalse(verifier.verify(pkSeed, pkRoot ^ flipLow, message, signature), "root[0] mutation accepted");
            assertFalse(verifier.verify(pkSeed, pkRoot ^ flipHigh, message, signature), "root[15] mutation accepted");
            assertFalse(verifier.verify(pkSeed, pkRoot, message ^ flipLow, signature), "msg[0] mutation accepted");
            assertFalse(
                verifier.verify(pkSeed, pkRoot, message ^ bytes32(uint256(masks[m])), signature),
                "msg[31] mutation accepted"
            );
        }
    }

    function test_NoncanonicalKeyReverts() public {
        // Nonzero low 16 bytes of pkSeed/pkRoot must revert (Rust returns 0).
        bytes32 dirtyPadding = bytes32(uint256(1));
        vm.expectRevert(bytes("Invalid public key"));
        verifier.verify(pkSeed | dirtyPadding, pkRoot, message, signature);
        vm.expectRevert(bytes("Invalid public key"));
        verifier.verify(pkSeed, pkRoot | dirtyPadding, message, signature);
    }

    function test_WrongLengthReverts() public {
        bytes memory truncated = new bytes(SIGNATURE_LENGTH - 1);
        for (uint256 i = 0; i < truncated.length; i++) {
            truncated[i] = signature[i];
        }
        vm.expectRevert(bytes("Invalid sig length"));
        verifier.verify(pkSeed, pkRoot, message, truncated);

        bytes memory extended = bytes.concat(signature, hex"00");
        vm.expectRevert(bytes("Invalid sig length"));
        verifier.verify(pkSeed, pkRoot, message, extended);
    }

    function test_RandomSignaturesReject() public view {
        uint64 state = RANDOM_SEED;
        for (uint256 v = 0; v < RANDOM_VECTORS; v++) {
            bytes memory randomSig = new bytes(SIGNATURE_LENGTH);
            for (uint256 i = 0; i < SIGNATURE_LENGTH; i++) {
                uint64 output;
                (state, output) = _xorshift64star(state);
                randomSig[i] = bytes1(uint8(output >> 56));
            }
            assertFalse(
                verifier.verify(pkSeed, pkRoot, message, randomSig),
                string.concat("random signature accepted, vector ", vm.toString(v))
            );
        }
    }

    /// xorshift64* PRNG; identical byte stream in the Rust harness.
    function _xorshift64star(uint64 x) internal pure returns (uint64 newState, uint64 output) {
        unchecked {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            newState = x;
            output = x * 0x2545F4914F6CDD1D;
        }
    }
}
