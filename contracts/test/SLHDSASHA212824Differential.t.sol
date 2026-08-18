// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {SLHDSASHA212824Verifier} from "../src/pali/SLHDSASHA212824Verifier.sol";

/// @title SLH-DSA-SHA2-128-24 differential test (Solidity side)
/// @notice Runs the exact same SP 800-230 Initial Public Draft conformance
/// corpus and deterministic mutation sweep as the Rust precompile harness in
/// `tools/slh-dsa-difftest/tests/differential.rs`. Both sides must accept the
/// valid vectors and reject every mutated/random vector, detecting divergence
/// on this finite corpus. This is regression coverage, not coverage-guided
/// fuzzing or a general equivalence proof. Keep the mutation constants in sync
/// with the Rust harness.
contract SLHDSASHA212824DifferentialTest is Test {
    uint256 internal constant SIGNATURE_LENGTH = 3856;
    // Mutation scheme shared with the Rust harness.
    uint256 internal constant MUTATION_STRIDE = 31;
    uint256 internal constant RANDOM_VECTORS = 8;
    uint64 internal constant RANDOM_SEED = 0x5EED_5EED_5EED_5EED;

    struct CorpusVector {
        string label;
        bytes32 pkSeed;
        bytes32 pkRoot;
        bytes32 message;
        bytes signature;
    }

    SLHDSASHA212824Verifier internal verifier;
    CorpusVector[] internal vectors;

    function setUp() public {
        verifier = new SLHDSASHA212824Verifier();
        // SYSCOIN: Keep the historical regression fixture while making the
        // independently reproducible SP 800-230 IPD fixture canonical.
        _loadVector(
            "legacy-unreproducible",
            "slh_dsa_sha2_128_24_kat.json",
            "slh-dsa-sha2-128-24-legacy-regression-v1",
            "legacy-unreproducible-regression-only"
        );
        _loadVector(
            "canonical-sp800-230-ipd-counter0",
            "slh_dsa_sha2_128_24_sp800_230_ipd_counter0.json",
            "slh-dsa-sha2-128-24-sp800-230-ipd-counter0-v1",
            "canonical-reproducible-conformance"
        );
    }

    function _loadVector(
        string memory label,
        string memory filename,
        string memory expectedId,
        string memory expectedStatus
    ) internal {
        string memory json = vm.readFile(string.concat(vm.projectRoot(), "/test/vectors/", filename));
        assertEq(vm.parseJsonString(json, ".id"), expectedId, "unexpected conformance-vector id");
        assertEq(vm.parseJsonString(json, ".status"), expectedStatus, "unexpected conformance-vector status");

        bytes memory signature = vm.parseJsonBytes(json, ".signature");
        assertEq(signature.length, SIGNATURE_LENGTH, "bad conformance fixture");
        vectors.push(
            CorpusVector({
                label: label,
                pkSeed: vm.parseJsonBytes32(json, ".pkSeed"),
                pkRoot: vm.parseJsonBytes32(json, ".pkRoot"),
                message: vm.parseJsonBytes32(json, ".message"),
                signature: signature
            })
        );
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

    function test_ValidCorpusVectorsVerify() public view {
        for (uint256 v = 0; v < vectors.length; v++) {
            CorpusVector storage vector = vectors[v];
            assertTrue(
                verifier.verify(vector.pkSeed, vector.pkRoot, vector.message, vector.signature),
                string.concat("valid conformance vector rejected: ", vector.label)
            );
        }
    }

    function test_MutatedSignatureRejects() public {
        uint256[14] memory boundaries = _boundaryOffsets();
        uint8[2] memory masks = _mutationMasks();

        for (uint256 v = 0; v < vectors.length; v++) {
            CorpusVector storage vector = vectors[v];
            for (uint256 b = 0; b < boundaries.length; b++) {
                _assertSigMutationRejected(vector, boundaries[b], masks);
            }
            for (uint256 offset = 0; offset < SIGNATURE_LENGTH; offset += MUTATION_STRIDE) {
                _assertSigMutationRejected(vector, offset, masks);
            }
        }
    }

    function _assertSigMutationRejected(CorpusVector storage vector, uint256 offset, uint8[2] memory masks) internal {
        for (uint256 m = 0; m < masks.length; m++) {
            bytes1 original = vector.signature[offset];
            vector.signature[offset] = bytes1(uint8(original) ^ masks[m]);
            assertFalse(
                verifier.verify(vector.pkSeed, vector.pkRoot, vector.message, vector.signature),
                string.concat(vector.label, ": sig mutation accepted at offset ", vm.toString(offset))
            );
            vector.signature[offset] = original;
        }
    }

    function test_MutatedSeedRootMessageRejects() public view {
        uint8[2] memory masks = _mutationMasks();
        for (uint256 v = 0; v < vectors.length; v++) {
            CorpusVector storage vector = vectors[v];
            for (uint256 m = 0; m < masks.length; m++) {
                bytes32 flipLow = bytes32(uint256(masks[m]) << 248);
                bytes32 flipHigh = bytes32(uint256(masks[m]) << 128);
                // Flipping bits inside the 16 meaningful key bytes must reject.
                assertFalse(
                    verifier.verify(vector.pkSeed ^ flipLow, vector.pkRoot, vector.message, vector.signature),
                    string.concat(vector.label, ": seed[0] mutation accepted")
                );
                assertFalse(
                    verifier.verify(vector.pkSeed ^ flipHigh, vector.pkRoot, vector.message, vector.signature),
                    string.concat(vector.label, ": seed[15] mutation accepted")
                );
                assertFalse(
                    verifier.verify(vector.pkSeed, vector.pkRoot ^ flipLow, vector.message, vector.signature),
                    string.concat(vector.label, ": root[0] mutation accepted")
                );
                assertFalse(
                    verifier.verify(vector.pkSeed, vector.pkRoot ^ flipHigh, vector.message, vector.signature),
                    string.concat(vector.label, ": root[15] mutation accepted")
                );
                assertFalse(
                    verifier.verify(vector.pkSeed, vector.pkRoot, vector.message ^ flipLow, vector.signature),
                    string.concat(vector.label, ": msg[0] mutation accepted")
                );
                assertFalse(
                    verifier.verify(
                        vector.pkSeed, vector.pkRoot, vector.message ^ bytes32(uint256(masks[m])), vector.signature
                    ),
                    string.concat(vector.label, ": msg[31] mutation accepted")
                );
            }
        }
    }

    function test_NoncanonicalKeyReverts() public {
        // Nonzero low 16 bytes of pkSeed/pkRoot must revert (Rust returns 0).
        bytes32 dirtyPadding = bytes32(uint256(1));
        for (uint256 v = 0; v < vectors.length; v++) {
            CorpusVector storage vector = vectors[v];
            vm.expectRevert(bytes("Invalid public key"));
            verifier.verify(vector.pkSeed | dirtyPadding, vector.pkRoot, vector.message, vector.signature);
            vm.expectRevert(bytes("Invalid public key"));
            verifier.verify(vector.pkSeed, vector.pkRoot | dirtyPadding, vector.message, vector.signature);
        }
    }

    function test_WrongLengthReverts() public {
        for (uint256 v = 0; v < vectors.length; v++) {
            CorpusVector storage vector = vectors[v];
            bytes memory truncated = new bytes(SIGNATURE_LENGTH - 1);
            for (uint256 i = 0; i < truncated.length; i++) {
                truncated[i] = vector.signature[i];
            }
            vm.expectRevert(bytes("Invalid sig length"));
            verifier.verify(vector.pkSeed, vector.pkRoot, vector.message, truncated);

            bytes memory extended = bytes.concat(vector.signature, hex"00");
            vm.expectRevert(bytes("Invalid sig length"));
            verifier.verify(vector.pkSeed, vector.pkRoot, vector.message, extended);
        }
    }

    function test_RandomSignaturesReject() public view {
        for (uint256 corpusIndex = 0; corpusIndex < vectors.length; corpusIndex++) {
            CorpusVector storage vector = vectors[corpusIndex];
            uint64 state = RANDOM_SEED;
            for (uint256 v = 0; v < RANDOM_VECTORS; v++) {
                bytes memory randomSig = new bytes(SIGNATURE_LENGTH);
                for (uint256 i = 0; i < SIGNATURE_LENGTH; i++) {
                    uint64 output;
                    (state, output) = _xorshift64star(state);
                    randomSig[i] = bytes1(uint8(output >> 56));
                }
                assertFalse(
                    verifier.verify(vector.pkSeed, vector.pkRoot, vector.message, randomSig),
                    string.concat(vector.label, ": random signature accepted, vector ", vm.toString(v))
                );
            }
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
