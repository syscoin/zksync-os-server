// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// @title NIST SP 800-230 SLH-DSA-SHA2-128-24 verifier
/// @notice FIPS 205 external SLH-DSA.Verify with an empty context and fixed
/// 32-byte messages. Parameters: n=16, h=22, d=1, a=24, k=6, w=4, l=68.
contract SLHDSASHA212824Verifier {
    function verify(bytes32 pkSeed, bytes32 pkRoot, bytes32 message, bytes calldata sig)
        external
        view
        returns (bool valid)
    {
        assembly {
            let N_MASK := 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF00000000000000000000000000000000

            if iszero(eq(sig.length, 3856)) {
                mstore(0x00, 0x08c379a000000000000000000000000000000000000000000000000000000000)
                mstore(0x04, 0x20)
                mstore(0x24, 18)
                mstore(0x44, "Invalid sig length")
                revert(0x00, 0x64)
            }

            if or(iszero(eq(pkSeed, and(pkSeed, N_MASK))), iszero(eq(pkRoot, and(pkRoot, N_MASK)))) {
                mstore(0x00, 0x08c379a000000000000000000000000000000000000000000000000000000000)
                mstore(0x04, 0x20)
                mstore(0x24, 18)
                mstore(0x44, "Invalid public key")
                revert(0x00, 0x64)
            }

            let seed := pkSeed
            let root := pkRoot
            let sigBase := sig.offset

            // H_msg: M' = 0x00 || 0x00 || M for external SLH-DSA with empty context.
            mstore(0x00, calldataload(sigBase))
            mstore(0x10, seed)
            mstore(0x20, root)
            mstore(0x30, 0)
            mstore(0x32, message)
            if iszero(staticcall(gas(), 0x02, 0x00, 0x52, 0x20, 0x20)) { revert(0, 0) }

            mstore(0x40, 0)
            if iszero(staticcall(gas(), 0x02, 0x00, 0x44, 0x100, 0x20)) { revert(0, 0) }
            let dWord := mload(0x100)
            let leafIdx := and(shr(88, dWord), 0x3FFFFF)

            mstore(0x00, seed)
            mstore(0x20, 0)

            let forsBase := or(shl(176, 3), shl(144, leafIdx))
            let wotsBase := shl(144, leafIdx)
            let forsOff := 16

            for { let t := 0 } lt(t, 6) { t := add(t, 1) } {
                let mdT := and(shr(sub(232, mul(24, t)), dWord), 0xFFFFFF)
                let treeOff := add(forsOff, mul(t, 400))
                let sk := and(calldataload(add(sigBase, treeOff)), N_MASK)

                mstore(0x40, or(forsBase, shl(80, or(shl(24, t), mdT))))
                mstore(0x56, sk)
                if iszero(staticcall(gas(), 0x02, 0x00, 0x66, 0x80, 0x20)) { revert(0, 0) }
                let node := and(mload(0x80), N_MASK)

                let authPtr := add(sigBase, add(treeOff, 16))
                let pathIdx := mdT
                for { let j := 0 } lt(j, 24) { j := add(j, 1) } {
                    let sibling := and(calldataload(add(authPtr, shl(4, j))), N_MASK)
                    let parentIdx := shr(1, pathIdx)
                    let globalY := or(shl(sub(23, j), t), parentIdx)
                    mstore(0x40, or(forsBase, or(shl(112, add(j, 1)), shl(80, globalY))))
                    switch and(pathIdx, 1)
                    case 0 {
                        mstore(0x56, node)
                        mstore(0x66, sibling)
                    }
                    default {
                        mstore(0x56, sibling)
                        mstore(0x66, node)
                    }
                    if iszero(staticcall(gas(), 0x02, 0x00, 0x76, 0x80, 0x20)) { revert(0, 0) }
                    node := and(mload(0x80), N_MASK)
                    pathIdx := parentIdx
                }
                mstore(add(0x100, shl(5, t)), node)
            }

            mstore(0x40, or(shl(176, 4), shl(144, leafIdx)))
            for { let t := 0 } lt(t, 6) { t := add(t, 1) } {
                mstore(add(0x56, shl(4, t)), mload(add(0x100, shl(5, t))))
            }
            if iszero(staticcall(gas(), 0x02, 0x00, 0xB6, 0x80, 0x20)) { revert(0, 0) }
            let currentNode := and(mload(0x80), N_MASK)

            let wotsPtr := add(sigBase, add(forsOff, 2400))
            let csum := 0

            for { let i := 0 } lt(i, 64) { i := add(i, 1) } {
                let digit := and(shr(sub(254, shl(1, i)), currentNode), 3)
                csum := add(csum, sub(3, digit))

                let val := and(calldataload(add(wotsPtr, shl(4, i))), N_MASK)
                let chainBase := or(wotsBase, shl(112, i))
                let steps := sub(3, digit)
                for { let s := 0 } lt(s, steps) { s := add(s, 1) } {
                    mstore(0x40, or(chainBase, shl(80, add(digit, s))))
                    mstore(0x56, val)
                    if iszero(staticcall(gas(), 0x02, 0x00, 0x66, 0x80, 0x20)) { revert(0, 0) }
                    val := and(mload(0x80), N_MASK)
                }
                mstore(add(0x100, shl(5, i)), val)
            }

            for { let j := 0 } lt(j, 4) { j := add(j, 1) } {
                let digit := and(shr(sub(6, shl(1, j)), csum), 3)
                let i := add(64, j)
                let val := and(calldataload(add(wotsPtr, shl(4, i))), N_MASK)
                let chainBase := or(wotsBase, shl(112, i))
                let steps := sub(3, digit)
                for { let s := 0 } lt(s, steps) { s := add(s, 1) } {
                    mstore(0x40, or(chainBase, shl(80, add(digit, s))))
                    mstore(0x56, val)
                    if iszero(staticcall(gas(), 0x02, 0x00, 0x66, 0x80, 0x20)) { revert(0, 0) }
                    val := and(mload(0x80), N_MASK)
                }
                mstore(add(0x100, shl(5, i)), val)
            }

            mstore(0x40, or(shl(176, 1), shl(144, leafIdx)))
            for { let i := 0 } lt(i, 68) { i := add(i, 1) } {
                mstore(add(0x56, shl(4, i)), mload(add(0x100, shl(5, i))))
            }
            if iszero(staticcall(gas(), 0x02, 0x00, 0x496, 0x4A0, 0x20)) { revert(0, 0) }
            let wotsPk := and(mload(0x4A0), N_MASK)

            let authPtr := add(wotsPtr, 1088)
            let merkleNode := wotsPk
            let mIdx := leafIdx
            for { let hh := 0 } lt(hh, 22) { hh := add(hh, 1) } {
                let sibling := and(calldataload(add(authPtr, shl(4, hh))), N_MASK)
                let parentIdx := shr(1, mIdx)
                mstore(0x40, or(shl(176, 2), or(shl(112, add(hh, 1)), shl(80, parentIdx))))
                switch and(mIdx, 1)
                case 0 {
                    mstore(0x56, merkleNode)
                    mstore(0x66, sibling)
                }
                default {
                    mstore(0x56, sibling)
                    mstore(0x66, merkleNode)
                }
                if iszero(staticcall(gas(), 0x02, 0x00, 0x76, 0x80, 0x20)) { revert(0, 0) }
                merkleNode := and(mload(0x80), N_MASK)
                mIdx := parentIdx
            }

            valid := eq(merkleNode, root)
            mstore(0x00, valid)
            return(0x00, 0x20)
        }
    }
}
