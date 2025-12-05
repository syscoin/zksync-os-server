// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// Modexp precompile is at address 0x0000000000000000000000000000000000000005
contract ModExp {
    /**
     * @dev Computes base^exp % mod using the modexp precompile.
     */
    function modexp(bytes memory base, bytes memory exponent, bytes memory modulus)
    external returns (bytes memory result)
    {
        uint256 baseLen = base.length;
        uint256 expLen = exponent.length;
        uint256 modLen = modulus.length;

        bytes memory input = abi.encodePacked(
            uint256(baseLen),
            uint256(expLen),
            uint256(modLen),
            base,
            exponent,
            modulus
        );

        // Output has the size of modulus
        result = new bytes(modLen);

        bool success;
        assembly {
        // Call precompile 0x05
        // gas: not specified, so pass all gas
            success := staticcall(
                gas(),
                5,
                add(input, 0x20),
                mload(input),
                add(result, 0x20),
                modLen
            )
        }

        require(success, "modexp failed");
    }
}
