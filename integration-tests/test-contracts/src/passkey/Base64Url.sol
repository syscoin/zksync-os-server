// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

library Base64Url {
    bytes internal constant ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    function encode32(bytes32 value) internal pure returns (bytes memory encoded) {
        encoded = new bytes(43);
        uint256 raw = uint256(value);

        for (uint256 i = 0; i < 10; ++i) {
            uint256 chunk = (raw >> (256 - 24 * (i + 1))) & 0xffffff;
            encoded[i * 4] = ALPHABET[(chunk >> 18) & 0x3f];
            encoded[i * 4 + 1] = ALPHABET[(chunk >> 12) & 0x3f];
            encoded[i * 4 + 2] = ALPHABET[(chunk >> 6) & 0x3f];
            encoded[i * 4 + 3] = ALPHABET[chunk & 0x3f];
        }

        uint256 last = raw & 0xffff;
        encoded[40] = ALPHABET[(last >> 10) & 0x3f];
        encoded[41] = ALPHABET[(last >> 4) & 0x3f];
        encoded[42] = ALPHABET[(last & 0x0f) << 2];
    }
}
