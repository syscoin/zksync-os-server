// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Tests EIP-2929 address warming by measuring gas for BALANCE opcode.
///
/// Per EIP-2929, tx.origin, tx.to, precompiles (1-9), and coinbase (EIP-3651)
/// must be warm before execution begins.
///   - Warm BALANCE: 100 gas
///   - Cold BALANCE: 2,600 gas
///
/// Bug: zksync-os treats the P256 precompile (address 0x100) as always-warm,
/// but REVM does not pre-warm it. The P256 precompile is a non-standard
/// extension (RIP-7212) and is NOT in the EIP-2929 precompile set (1-9).
/// As a result, the stored p256AccessGas value differs:
///   - zksync-os: ~100 (warm)
///   - REVM:      ~2,600 (cold)
contract AddressWarmingTest {
    uint256 public originAccessGas;
    uint256 public selfAccessGas;
    uint256 public coldAccessGas;
    uint256 public coinbaseAccessGas;
    uint256 public precompileAccessGas;
    uint256 public p256AccessGas;

    /// @dev Measure gas for BALANCE on all address categories and store results.
    /// Uses mstore(0, ...) to prevent the Yul optimizer from removing balance() calls.
    function measureAll(address coldAddr) external {
        // tx.origin — should be warm (EIP-2929)
        {
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(origin())) }
            originAccessGas = gasBefore - gasleft();
        }

        // self (tx.to) — should be warm (EIP-2929)
        {
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(address())) }
            selfAccessGas = gasBefore - gasleft();
        }

        // random cold address — should cost 2,600
        {
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(coldAddr)) }
            coldAccessGas = gasBefore - gasleft();
        }

        // coinbase — should be warm (EIP-3651)
        {
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(coinbase())) }
            coinbaseAccessGas = gasBefore - gasleft();
        }

        // standard precompile (ecrecover at address(1)) — should be warm (EIP-2929)
        {
            address precompile = address(1);
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(precompile)) }
            precompileAccessGas = gasBefore - gasleft();
        }

        // P256 precompile (address 0x100) — should be cold per EIP-2929
        // (not in the standard 1-9 precompile set), but zksync-os warms it
        {
            address p256 = address(0x100);
            uint256 gasBefore = gasleft();
            assembly { mstore(0, balance(p256)) }
            p256AccessGas = gasBefore - gasleft();
        }
    }
}
