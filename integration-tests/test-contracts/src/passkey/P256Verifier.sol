// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

library P256Verifier {
    address internal constant P256_VERIFY_PRECOMPILE = address(0x100);
    uint256 internal constant P256_N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551;
    uint256 internal constant P256_HALF_N = 0x7FFFFFFF800000007FFFFFFFFFFFFFFFDE737D56D38BCF4279DCE5617E3192A8;

    error P256InvalidSignature();
    error P256PrecompileFailure();

    function verify(bytes32 digest, bytes32 r, bytes32 s, bytes32 x, bytes32 y) internal view {
        if (!isValid(digest, r, s, x, y)) {
            revert P256InvalidSignature();
        }
    }

    function isValid(bytes32 digest, bytes32 r, bytes32 s, bytes32 x, bytes32 y) internal view returns (bool) {
        if (!_isCanonicalSignature(r, s)) {
            return false;
        }

        bytes memory input = abi.encodePacked(digest, r, s, x, y);

        (bool success, bytes memory result) = P256_VERIFY_PRECOMPILE.staticcall(input);
        if (!success) {
            revert P256PrecompileFailure();
        }

        return result.length == 32 && abi.decode(result, (uint256)) == 1;
    }

    function _isCanonicalSignature(bytes32 r, bytes32 s) private pure returns (bool) {
        return uint256(r) > 0 && uint256(r) < P256_N && uint256(s) > 0 && uint256(s) <= P256_HALF_N;
    }
}
