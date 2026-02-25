// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Delegate-calls every precompile with various inputs and stores
/// keccak256(success ++ gasUsed ++ returnData) per call in storage.
/// Any difference in precompile behavior between EVMs surfaces as a storage mismatch.
///
/// Uses a fixed gas cap per delegatecall so that precompiles which consume all
/// forwarded gas on invalid input (BN254 add/mul/pairing) don't kill the tx.
contract PrecompileDelegateCallTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    /// @dev Gas budget forwarded to each precompile delegatecall.
    uint256 constant GAS_CAP = 100_000;

    function runAll() external {
        uint256 idx;

        // ========== ecrecover (address 1) ==========
        // Empty input
        idx = _run(idx, address(1), "");
        // 128 zero bytes (valid length, invalid signature)
        idx = _run(idx, address(1), new bytes(128));
        // hash=0, v=27, r=1, s=1
        idx = _run(idx, address(1), abi.encodePacked(
            bytes32(0),
            bytes32(uint256(27)),
            bytes32(uint256(1)),
            bytes32(uint256(1))
        ));
        // hash=1, v=28, r=1, s=1
        idx = _run(idx, address(1), abi.encodePacked(
            bytes32(uint256(1)),
            bytes32(uint256(28)),
            bytes32(uint256(1)),
            bytes32(uint256(1))
        ));
        // Invalid v value (99)
        idx = _run(idx, address(1), abi.encodePacked(
            bytes32(uint256(1)),
            bytes32(uint256(99)),
            bytes32(uint256(1)),
            bytes32(uint256(1))
        ));
        // Short input (64 bytes)
        idx = _run(idx, address(1), new bytes(64));

        // ========== SHA-256 (address 2) ==========
        idx = _run(idx, address(2), "");
        idx = _run(idx, address(2), hex"00");
        idx = _run(idx, address(2), hex"ff");
        idx = _run(idx, address(2), abi.encodePacked(bytes32(uint256(42))));
        idx = _run(idx, address(2), "hello world");

        // ========== RIPEMD-160 (address 3) ==========
        idx = _run(idx, address(3), "");
        idx = _run(idx, address(3), hex"00");
        idx = _run(idx, address(3), hex"ff");
        idx = _run(idx, address(3), "hello world");

        // ========== Identity (address 4) ==========
        idx = _run(idx, address(4), "");
        idx = _run(idx, address(4), hex"deadbeef");
        idx = _run(idx, address(4), abi.encodePacked(bytes32(uint256(123456))));

        // ========== ModExp (address 5) ==========
        // Empty input
        idx = _run(idx, address(5), "");
        // 3^2 mod 10 = 9
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(32)), bytes32(uint256(32)), bytes32(uint256(32)),
            bytes32(uint256(3)), bytes32(uint256(2)), bytes32(uint256(10))
        ));
        // 2^10 mod 1000000007 = 1024
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(32)), bytes32(uint256(32)), bytes32(uint256(32)),
            bytes32(uint256(2)), bytes32(uint256(10)), bytes32(uint256(1000000007))
        ));
        // 0^0 mod 1 = 0
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(32)), bytes32(uint256(32)), bytes32(uint256(32)),
            bytes32(0), bytes32(0), bytes32(uint256(1))
        ));
        // 2^255 mod 1000000007
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(32)), bytes32(uint256(32)), bytes32(uint256(32)),
            bytes32(uint256(2)), bytes32(uint256(255)), bytes32(uint256(1000000007))
        ));
        // Modulus = 0
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(32)), bytes32(uint256(32)), bytes32(uint256(32)),
            bytes32(uint256(2)), bytes32(uint256(3)), bytes32(0)
        ));
        // Only sizes, no operand data
        idx = _run(idx, address(5), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(1)), bytes32(uint256(1))
        ));

        // ========== BN254 Add (address 6) ==========
        // Empty input
        idx = _run(idx, address(6), "");
        // Zero points (infinity + infinity)
        idx = _run(idx, address(6), new bytes(128));
        // G1=(1,2) + infinity
        idx = _run(idx, address(6), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(0), bytes32(0)
        ));
        // G1 + G1 = 2*G1
        idx = _run(idx, address(6), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(uint256(1)), bytes32(uint256(2))
        ));
        // Short input (one point only)
        idx = _run(idx, address(6), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2))
        ));
        // Invalid point (1,1) — not on curve (gas-capped so it won't drain tx)
        idx = _run(idx, address(6), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(1)),
            bytes32(0), bytes32(0)
        ));

        // ========== BN254 Scalar Mul (address 7) ==========
        // Empty input
        idx = _run(idx, address(7), "");
        // G1 * 0 = infinity
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(0)
        ));
        // G1 * 1 = G1
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(uint256(1))
        ));
        // G1 * 2
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(uint256(2))
        ));
        // G1 * large scalar
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(2)),
            bytes32(uint256(0xdeadbeefcafebabe))
        ));
        // Infinity * 5
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(0), bytes32(0),
            bytes32(uint256(5))
        ));
        // Invalid point (1,1) * 1 — not on curve (gas-capped)
        idx = _run(idx, address(7), abi.encodePacked(
            bytes32(uint256(1)), bytes32(uint256(1)),
            bytes32(uint256(1))
        ));

        // ========== BN254 Pairing (address 8) ==========
        // Empty input (vacuous pairing → returns 1)
        idx = _run(idx, address(8), "");
        // Invalid length (not a multiple of 192)
        idx = _run(idx, address(8), new bytes(64));
        // One pair of zeros (192 bytes)
        idx = _run(idx, address(8), new bytes(192));

        // ========== P256Verify (address 0x100) ==========
        // Empty input
        idx = _run(idx, address(0x100), "");
        // 160 zero bytes (valid length, invalid signature)
        idx = _run(idx, address(0x100), new bytes(160));
        // Non-zero deterministic input
        idx = _run(idx, address(0x100), abi.encodePacked(
            bytes32(uint256(1)),
            bytes32(uint256(2)),
            bytes32(uint256(3)),
            bytes32(uint256(4)),
            bytes32(uint256(5))
        ));
        // Short input (64 bytes)
        idx = _run(idx, address(0x100), new bytes(64));

        totalTests = idx;
    }

    /// @dev Delegatecall `target` with `input`, forwarding at most GAS_CAP gas.
    ///      Stores keccak256(success ++ gasUsed ++ returnData) into results[slot].
    function _run(
        uint256 slot,
        address target,
        bytes memory input
    ) internal returns (uint256) {
        bool success;
        bytes memory ret;
        uint256 gasBefore = gasleft();

        assembly {
            // delegatecall(gas, addr, argsOffset, argsLength, retOffset, retLength)
            let cap := GAS_CAP
            let available := gas()
            if gt(cap, available) { cap := available }

            success := delegatecall(
                cap,
                target,
                add(input, 0x20),
                mload(input),
                0,
                0
            )

            // Allocate return data
            let retSize := returndatasize()
            ret := mload(0x40)
            mstore(ret, retSize)
            returndatacopy(add(ret, 0x20), 0, retSize)
            // Update free memory pointer (32-byte aligned)
            mstore(0x40, add(add(ret, 0x20), and(add(retSize, 31), not(31))))
        }

        uint256 gasUsed = gasBefore - gasleft();
        results[slot] = keccak256(abi.encodePacked(success, gasUsed, ret));
        return slot + 1;
    }
}
