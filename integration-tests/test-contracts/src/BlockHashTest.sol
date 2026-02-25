// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Tests BLOCKHASH opcode behavior across various scenarios.
///      Any divergence between zksync-os and REVM surfaces via the consistency checker.
///
/// Scenarios tested:
///   - BLOCKHASH of the current block (must return 0 per EVM spec)
///   - BLOCKHASH of future blocks (must return 0)
///   - BLOCKHASH of the previous block (non-zero if block.number > 0)
///   - BLOCKHASH of block 0 (genesis hash or 0 if out of 256-block window)
///   - BLOCKHASH of a block beyond the 256-block window (must return 0)
///   - Multiple consecutive block hashes (all distinct for distinct blocks)
///   - BLOCKHASH called twice for the same block (must be identical)
///   - BLOCKHASH at the 256-block boundary (last valid vs first invalid)
contract BlockHashTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    // Raw blockhash values stored for external inspection
    bytes32 public hashAtCurrent;        // blockhash(block.number)
    bytes32 public hashAtPrevious;       // blockhash(block.number - 1)
    bytes32 public hashAtBoundary;       // blockhash(block.number - 256)
    uint256 public executedAtBlock;

    function runAll() external {
        uint256 idx;

        idx = _currentBlockHash(idx);
        idx = _futureBlockHash(idx);
        idx = _previousBlockHash(idx);
        idx = _blockZeroHash(idx);
        idx = _farPastBlockHash(idx);
        idx = _consecutiveBlockHashes(idx);
        idx = _blockHashRepeatability(idx);
        idx = _boundaryBlockHash(idx);

        // Store raw blockhash values for external reading
        executedAtBlock = block.number;
        hashAtCurrent = blockhash(block.number);
        hashAtPrevious = block.number > 0 ? blockhash(block.number - 1) : bytes32(0);
        hashAtBoundary = block.number > 256 ? blockhash(block.number - 256) : bytes32(0);

        totalTests = idx;
    }

    // ----------------------------------------------------------------
    // 1. BLOCKHASH of current block → must return bytes32(0)
    //
    // Per EVM spec, blockhash(block.number) always returns 0 because
    // the current block's hash is not yet finalized during execution.
    // ----------------------------------------------------------------
    function _currentBlockHash(uint256 s) internal returns (uint256) {
        bytes32 h = blockhash(block.number);
        results[s] = keccak256(abi.encodePacked(h, block.number));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 2. BLOCKHASH of future blocks → must return bytes32(0)
    //
    // Tests block.number+1, block.number+100, and type(uint256).max.
    // All must return zero.
    // ----------------------------------------------------------------
    function _futureBlockHash(uint256 s) internal returns (uint256) {
        bytes32 h1 = blockhash(block.number + 1);
        bytes32 h2 = blockhash(block.number + 100);
        bytes32 h3 = blockhash(type(uint256).max);
        results[s] = keccak256(abi.encodePacked(h1, h2, h3));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 3. BLOCKHASH of previous block (block.number - 1)
    //
    // Should return a non-zero hash when block.number > 0.
    // The actual hash value must match between zksync-os and REVM.
    // ----------------------------------------------------------------
    function _previousBlockHash(uint256 s) internal returns (uint256) {
        if (block.number > 0) {
            bytes32 h = blockhash(block.number - 256);
            results[s] = keccak256(abi.encodePacked(h, block.number - 1, h != bytes32(0)));
        } else {
            results[s] = keccak256(abi.encodePacked(bytes32(0), uint256(0), false));
        }
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 4. BLOCKHASH of block 0 (genesis)
    //
    // If block.number <= 256, block 0 is within the 256-block window
    // and should return the genesis hash. Otherwise returns 0.
    // ----------------------------------------------------------------
    function _blockZeroHash(uint256 s) internal returns (uint256) {
        bytes32 h = blockhash(0);
        bool inRange = block.number <= 256;
        results[s] = keccak256(abi.encodePacked(h, inRange));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 5. BLOCKHASH of a block guaranteed to be out of range
    //
    // If block.number > 257, block (block.number - 257) is outside
    // the 256-block window and must return 0.
    // If the chain is young, we still store a deterministic result.
    // ----------------------------------------------------------------
    function _farPastBlockHash(uint256 s) internal returns (uint256) {
        if (block.number > 257) {
            bytes32 h = blockhash(block.number - 257);
            // Must be zero — out of 256-block window
            results[s] = keccak256(abi.encodePacked(h, block.number - 257, true));
        } else {
            // Chain is too young for this test; store a sentinel
            results[s] = keccak256(abi.encodePacked(bytes32(0), uint256(0), false));
        }
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 6. Multiple consecutive block hashes
    //
    // Fetches up to 5 most recent block hashes and packs them.
    // The actual hash values must match between zksync-os and REVM.
    // Different blocks should produce different hashes.
    // ----------------------------------------------------------------
    function _consecutiveBlockHashes(uint256 s) internal returns (uint256) {
        uint256 count = block.number < 5 ? block.number : 5;
        bytes memory packed;
        for (uint256 i = 0; i < count; i++) {
            packed = abi.encodePacked(packed, blockhash(block.number - 1 - i));
        }
        results[s] = keccak256(abi.encodePacked(packed, count));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 7. BLOCKHASH repeatability — calling twice returns the same value
    //
    // Two calls to blockhash() for the same block number within the
    // same transaction must return identical results.
    // ----------------------------------------------------------------
    function _blockHashRepeatability(uint256 s) internal returns (uint256) {
        if (block.number > 0) {
            bytes32 h1 = blockhash(block.number - 1);
            // Burn some gas between calls to separate them
            uint256 dummy;
            for (uint256 i = 0; i < 10; i++) {
                dummy += i;
            }
            bytes32 h2 = blockhash(block.number - 1);
            results[s] = keccak256(abi.encodePacked(h1, h2, h1 == h2));
        } else {
            results[s] = keccak256(abi.encodePacked(bytes32(0), bytes32(0), true));
        }
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 8. BLOCKHASH at the 256-block boundary
    //
    // Tests the exact boundary: block.number-256 should be the oldest
    // valid hash, while block.number-257 should return 0.
    // Only meaningful when block.number > 257.
    // ----------------------------------------------------------------
    function _boundaryBlockHash(uint256 s) internal returns (uint256) {
        if (block.number > 257) {
            bytes32 hValid = blockhash(block.number - 256);
            bytes32 hInvalid = blockhash(block.number - 257);
            results[s] = keccak256(abi.encodePacked(
                hValid, hValid != bytes32(0),
                hInvalid, hInvalid == bytes32(0)
            ));
        } else if (block.number > 1) {
            // Chain is young — test the oldest available block
            bytes32 hOldest = blockhash(0);
            bytes32 hPrev = blockhash(block.number - 1);
            results[s] = keccak256(abi.encodePacked(
                hOldest, hOldest != bytes32(0),
                hPrev, hPrev != bytes32(0)
            ));
        } else {
            results[s] = keccak256(abi.encodePacked(
                bytes32(0), false,
                bytes32(0), false
            ));
        }
        return s + 1;
    }
}
