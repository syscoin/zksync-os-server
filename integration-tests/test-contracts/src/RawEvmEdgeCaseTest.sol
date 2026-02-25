// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Deploys raw EVM bytecode that Solidity's codegen can never produce.
///      Any divergence between zksync-os and REVM surfaces via the consistency checker.
///
/// Patterns tested (all impossible or unusual for Solidity):
///   - CALLCODE opcode (0xF2) vs DELEGATECALL comparison
///   - PC opcode (0x58) at multiple offsets
///   - INVALID opcode (0xFE) behavior
///   - JUMPDEST (0x5B) inside PUSH data (invalid jump target)
///   - Runtime-computed JUMP destination from calldata
///   - CODECOPY self-inspection
///   - EXTCODECOPY of self vs CODECOPY comparison
///   - BYTE opcode edge cases (positions 0, 31, 32, 255)
///   - RETURNDATACOPY out-of-bounds
///   - MSTORE8 single-byte memory writes
///   - Stack underflow (8 opcodes: POP, ADD, DUP1, SWAP1, MSTORE, RETURN, partial ADD, partial SWAP2)
///   - Stack overflow (infinite push loop exceeding 1024 depth limit)
contract RawEvmEdgeCaseTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    receive() external payable {}

    function runAll() external payable {
        require(msg.value >= 1 ether, "need >= 1 ETH");
        uint256 idx;

        idx = _callcodeVsDelegatecall(idx);
        idx = _pcOpcode(idx);
        idx = _invalidOpcode(idx);
        idx = _jumpdestInsidePush(idx);
        idx = _runtimeComputedJump(idx);
        idx = _codecopySelf(idx);
        idx = _extcodeVsCodecopy(idx);
        idx = _byteOpcodeEdgeCases(idx);
        idx = _returndatacopyOob(idx);
        idx = _mstore8Behavior(idx);
        idx = _stackUnderflow(idx);
        idx = _stackOverflow(idx);

        totalTests = idx;
    }

    // ======== Bytecode helpers ========

    function _initCodeFor(bytes memory runtime) internal pure returns (bytes memory) {
        require(runtime.length <= 255, "runtime too long");
        return abi.encodePacked(
            hex"60", uint8(runtime.length),
            hex"80600b6000396000f3",
            runtime
        );
    }

    function _deploy(bytes memory initCode) internal returns (address addr) {
        assembly {
            addr := create(0, add(initCode, 0x20), mload(initCode))
        }
    }

    function _deployValue(bytes memory initCode, uint256 val) internal returns (address addr) {
        assembly {
            addr := create(val, add(initCode, 0x20), mload(initCode))
        }
    }

    // ----------------------------------------------------------------
    // 1. CALLCODE (0xF2) vs DELEGATECALL (0xF4)
    //
    // Impl runtime (18 bytes):
    //   CALLER PUSH0 SSTORE           ; slot[0] = CALLER
    //   ADDRESS PUSH1_1 SSTORE        ; slot[1] = ADDRESS
    //   CALLER PUSH0 MSTORE           ; mem[0:32] = CALLER
    //   ADDRESS PUSH1_32 MSTORE       ; mem[32:64] = ADDRESS
    //   PUSH1_64 PUSH0 RETURN         ; return 64 bytes
    //   = 33 5f 55 30 60 01 55 33 5f 52 30 60 20 52 60 40 5f f3
    //
    // CALLCODE wrapper calls impl via F2 then reads storage back.
    // DELEGATECALL wrapper calls impl via F4 then reads storage back.
    //
    // Key difference: inside CALLCODE, CALLER = wrapper address.
    //                 inside DELEGATECALL, CALLER = main contract (preserved).
    // ----------------------------------------------------------------
    function _callcodeVsDelegatecall(uint256 s) internal returns (uint256) {
        // Deploy impl
        address impl = _deploy(_initCodeFor(
            hex"335f5530600155335f523060205260405ff3"
        ));
        require(impl != address(0), "impl deploy failed");

        // CALLCODE wrapper runtime:
        //   PUSH0x5 PUSH20<impl> GAS CALLCODE POP
        //   PUSH0 SLOAD PUSH0 MSTORE
        //   PUSH1_1 SLOAD PUSH1_32 MSTORE
        //   PUSH1_64 PUSH0 RETURN
        bytes memory ccRuntime = abi.encodePacked(
            hex"5f5f5f5f5f73",
            bytes20(uint160(impl)),
            hex"5af250",
            hex"5f545f52",
            hex"60015460205260405ff3"
        );
        address ccWrapper = _deploy(_initCodeFor(ccRuntime));
        require(ccWrapper != address(0), "cc wrapper deploy failed");

        // DELEGATECALL wrapper runtime (4 PUSH0s, no value arg):
        bytes memory dcRuntime = abi.encodePacked(
            hex"5f5f5f5f73",
            bytes20(uint160(impl)),
            hex"5af450",
            hex"5f545f52",
            hex"60015460205260405ff3"
        );
        address dcWrapper = _deploy(_initCodeFor(dcRuntime));
        require(dcWrapper != address(0), "dc wrapper deploy failed");

        // Call both wrappers via regular CALL
        (bool ccOk, bytes memory ccRet) = ccWrapper.call("");
        (bool dcOk, bytes memory dcRet) = dcWrapper.call("");

        results[s] = keccak256(abi.encodePacked(
            ccOk, keccak256(ccRet), ccRet.length,
            dcOk, keccak256(dcRet), dcRet.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 2. PC opcode (0x58)
    //
    // Runtime (19 bytes):
    //   PC PUSH0 MSTORE               ; mem[0:32]  = 0  (PC at offset 0)
    //   PC PUSH1_32 MSTORE            ; mem[32:64] = 3  (PC at offset 3)
    //   PC PUSH1_64 MSTORE            ; mem[64:96] = 7  (PC at offset 7)
    //   PC PUSH1_96 MSTORE            ; mem[96:128]= 11 (PC at offset 11)
    //   PUSH1_128 PUSH0 RETURN
    //   = 58 5f 52 58 60 20 52 58 60 40 52 58 60 60 52 60 80 5f f3
    // ----------------------------------------------------------------
    function _pcOpcode(uint256 s) internal returns (uint256) {
        address victim = _deploy(_initCodeFor(
            hex"585f5258602052586040525860605260805ff3"
        ));
        require(victim != address(0), "deploy failed");

        (bool ok, bytes memory ret) = victim.call("");

        results[s] = keccak256(abi.encodePacked(ok, ret));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 3. INVALID opcode (0xFE)
    //
    // Runtime: just FE (1 byte). Should revert consuming all forwarded gas.
    // Gas-capped to 100K to avoid draining the tx.
    // ----------------------------------------------------------------
    function _invalidOpcode(uint256 s) internal returns (uint256) {
        address victim = _deploy(_initCodeFor(hex"fe"));
        require(victim != address(0), "deploy failed");

        bool ok;
        uint256 retSize;
        assembly {
            ok := call(100000, victim, 0, 0, 0, 0, 0)
            retSize := returndatasize()
        }

        results[s] = keccak256(abi.encodePacked(
            ok, retSize, victim.code.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 4. JUMPDEST (0x5B) inside PUSH data
    //
    // Contract A (16 bytes) – invalid jump:
    //   PUSH2 0x5B07                  ; 0x5B at offset 1 is PUSH2 data
    //   POP
    //   PUSH1 0x01                    ; target = offset 1 (inside PUSH2)
    //   JUMP                          ; should FAIL
    //   JUMPDEST                      ; valid but unreachable
    //   PUSH1_1 PUSH0 MSTORE PUSH1_32 PUSH0 RETURN
    //   = 61 5b 07 50 60 01 56 5b 60 01 5f 52 60 20 5f f3
    //
    // Contract B (13 bytes) – valid jump for comparison:
    //   PUSH1 0x04 JUMP INVALID JUMPDEST
    //   PUSH1_1 PUSH0 MSTORE PUSH1_32 PUSH0 RETURN
    //   = 60 04 56 fe 5b 60 01 5f 52 60 20 5f f3
    // ----------------------------------------------------------------
    function _jumpdestInsidePush(uint256 s) internal returns (uint256) {
        // Invalid jump: JUMPDEST byte is inside PUSH2 operand
        address invalid_ = _deploy(_initCodeFor(
            hex"615b07506001565b60015f5260205ff3"
        ));
        require(invalid_ != address(0), "deploy A failed");

        // Valid jump for comparison
        address valid_ = _deploy(_initCodeFor(
            hex"600456fe5b60015f5260205ff3"
        ));
        require(valid_ != address(0), "deploy B failed");

        bool okInvalid;
        uint256 retSizeInvalid;
        assembly {
            okInvalid := call(100000, invalid_, 0, 0, 0, 0, 0)
            retSizeInvalid := returndatasize()
        }
        (bool okValid, bytes memory retValid) = valid_.call("");

        results[s] = keccak256(abi.encodePacked(
            okInvalid, retSizeInvalid, okValid, retValid
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 5. Runtime-computed JUMP destination from calldata
    //
    // Runtime (23 bytes):
    //   PUSH0 CALLDATALOAD JUMP       ; jump to calldata[0:32] as uint256
    //   INVALID INVALID               ; padding
    //   JUMPDEST                       ; target A at offset 5 → returns 0xAA
    //   PUSH1_0xAA PUSH0 MSTORE PUSH1_32 PUSH0 RETURN
    //   JUMPDEST                       ; target B at offset 14 → returns 0xBB
    //   PUSH1_0xBB PUSH0 MSTORE PUSH1_32 PUSH0 RETURN
    //   = 5f 35 56 fe fe
    //     5b 60 aa 5f 52 60 20 5f f3
    //     5b 60 bb 5f 52 60 20 5f f3
    // ----------------------------------------------------------------
    function _runtimeComputedJump(uint256 s) internal returns (uint256) {
        address jumper = _deploy(_initCodeFor(
            hex"5f3556fefe5b60aa5f5260205ff35b60bb5f5260205ff3"
        ));
        require(jumper != address(0), "deploy failed");

        // Jump to target A (offset 5)
        (bool okA, bytes memory retA) = jumper.call(abi.encode(uint256(5)));
        // Jump to target B (offset 14)
        (bool okB, bytes memory retB) = jumper.call(abi.encode(uint256(14)));
        // Jump to invalid destination (offset 3 = INVALID)
        bool okBad;
        uint256 retSizeBad;
        assembly {
            let cd := mload(0x40)
            mstore(cd, 3)
            okBad := call(100000, jumper, 0, cd, 32, 0, 0)
            retSizeBad := returndatasize()
        }

        results[s] = keccak256(abi.encodePacked(
            okA, retA, okB, retB, okBad, retSizeBad
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 6. CODECOPY self-inspection
    //
    // Runtime (7 bytes):
    //   CODESIZE PUSH0 PUSH0 CODECOPY ; copy own code to mem[0:codesize]
    //   CODESIZE PUSH0 RETURN         ; return own code
    //   = 38 5f 5f 39 38 5f f3
    // ----------------------------------------------------------------
    function _codecopySelf(uint256 s) internal returns (uint256) {
        bytes memory runtime = hex"385f5f39385ff3";
        address victim = _deploy(_initCodeFor(runtime));
        require(victim != address(0), "deploy failed");

        (bool ok, bytes memory ret) = victim.call("");

        // ret should equal the runtime bytecode
        results[s] = keccak256(abi.encodePacked(
            ok, keccak256(ret), ret.length, keccak256(runtime)
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 7. EXTCODECOPY of self vs CODECOPY
    //
    // EXTCODECOPY runtime (8 bytes):
    //   CODESIZE PUSH0 PUSH0 ADDRESS EXTCODECOPY
    //   CODESIZE PUSH0 RETURN
    //   = 38 5f 5f 30 3c 38 5f f3
    //
    // Compare with CODECOPY version (test 6 runtime).
    // Both should return their own bytecode.
    // ----------------------------------------------------------------
    function _extcodeVsCodecopy(uint256 s) internal returns (uint256) {
        bytes memory ccRuntime = hex"385f5f39385ff3";
        bytes memory ecRuntime = hex"385f5f303c385ff3";

        address ccAddr = _deploy(_initCodeFor(ccRuntime));
        address ecAddr = _deploy(_initCodeFor(ecRuntime));
        require(ccAddr != address(0) && ecAddr != address(0), "deploy failed");

        (bool okCC, bytes memory retCC) = ccAddr.call("");
        (bool okEC, bytes memory retEC) = ecAddr.call("");

        results[s] = keccak256(abi.encodePacked(
            okCC, keccak256(retCC), retCC.length,
            okEC, keccak256(retEC), retEC.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 8. BYTE opcode edge cases
    //
    // Runtime (39 bytes):
    //   PUSH32 0xFF00...00             ; x = 0xFF << 248
    //   PUSH0 BYTE                    ; BYTE(0, x) = 0xFF (MSB)
    //   PUSH0 MSTORE
    //
    //   PUSH1_0xAB PUSH0 MSTORE       ; store 0xAB at mem[0:32]
    //   ... actually this is complex. Let me use a different value.
    //
    // Simpler: test BYTE(i, 0x0102...1f20) for i=0,31,32
    // 0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20
    //   BYTE(0) = 0x01, BYTE(31) = 0x20, BYTE(32) = 0x00
    //
    // Runtime:
    //   PUSH32<value> DUP1 DUP1
    //   PUSH0 BYTE PUSH0 MSTORE        ; mem[0:32] = BYTE(0, val)
    //   PUSH1_31 BYTE PUSH1_32 MSTORE  ; mem[32:64] = BYTE(31, val)
    //   PUSH1_32 BYTE PUSH1_64 MSTORE  ; mem[64:96] = BYTE(32, val)
    //   PUSH1_96 PUSH0 RETURN
    // ----------------------------------------------------------------
    function _byteOpcodeEdgeCases(uint256 s) internal returns (uint256) {
        // Build runtime dynamically because of PUSH32
        // PUSH32 <32 bytes> DUP1 DUP1
        // PUSH0 BYTE PUSH0 MSTORE
        // PUSH1 31 BYTE PUSH1 32 MSTORE
        // PUSH1 32 BYTE PUSH1 64 MSTORE
        // PUSH1 96 PUSH0 RETURN
        bytes memory runtime = abi.encodePacked(
            hex"7f",  // PUSH32
            bytes32(0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20),
            hex"8080",          // DUP1 DUP1
            hex"5f1a5f52",      // PUSH0 BYTE PUSH0 MSTORE
            hex"601f1a602052",  // PUSH1_31 BYTE PUSH1_32 MSTORE
            hex"60201a604052",  // PUSH1_32 BYTE PUSH1_64 MSTORE
            hex"60605ff3"       // PUSH1_96 PUSH0 RETURN
        );

        address victim = _deploy(_initCodeFor(runtime));
        require(victim != address(0), "deploy failed");

        (bool ok, bytes memory ret) = victim.call("");

        results[s] = keccak256(abi.encodePacked(ok, ret));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 9. RETURNDATACOPY out-of-bounds
    //
    // Deploy a "returner" that returns 32 bytes (value 0x42).
    // Deploy an "oob copier" that CALLs returner then tries
    // RETURNDATACOPY with size=64 (but only 32 available) → revert.
    // Deploy a "valid copier" that copies exactly 32 bytes → success.
    //
    // Returner runtime (8 bytes):
    //   PUSH1_0x42 PUSH0 MSTORE PUSH1_32 PUSH0 RETURN
    //   = 60 42 5f 52 60 20 5f f3
    //
    // OOB copier runtime:
    //   PUSH0x5 PUSH20<returner> GAS CALL POP
    //   PUSH1_64 PUSH0 PUSH0 RETURNDATACOPY   ; OOB!
    //   PUSH1_32 PUSH0 RETURN                  ; unreachable
    //
    // Valid copier runtime:
    //   PUSH0x5 PUSH20<returner> GAS CALL POP
    //   PUSH1_32 PUSH0 PUSH0 RETURNDATACOPY   ; valid
    //   PUSH1_32 PUSH0 RETURN
    // ----------------------------------------------------------------
    function _returndatacopyOob(uint256 s) internal returns (uint256) {
        address returner = _deploy(_initCodeFor(hex"60425f5260205ff3"));
        require(returner != address(0), "returner deploy failed");

        // OOB copier: RETURNDATACOPY with size=64 but only 32 available
        bytes memory oobRuntime = abi.encodePacked(
            hex"5f5f5f5f5f73",
            bytes20(uint160(returner)),
            hex"5af150",              // GAS CALL POP
            hex"60405f5f3e",          // PUSH1_64 PUSH0 PUSH0 RETURNDATACOPY
            hex"60205ff3"             // PUSH1_32 PUSH0 RETURN (unreachable)
        );
        address oobCopier = _deploy(_initCodeFor(oobRuntime));
        require(oobCopier != address(0), "oob copier deploy failed");

        // Valid copier: RETURNDATACOPY with size=32
        bytes memory validRuntime = abi.encodePacked(
            hex"5f5f5f5f5f73",
            bytes20(uint160(returner)),
            hex"5af150",
            hex"60205f5f3e",          // PUSH1_32 PUSH0 PUSH0 RETURNDATACOPY
            hex"60205ff3"
        );
        address validCopier = _deploy(_initCodeFor(validRuntime));
        require(validCopier != address(0), "valid copier deploy failed");

        // Gas-cap the OOB call — exceptional halt consumes all forwarded gas
        bool okOob;
        uint256 retSizeOob;
        assembly {
            okOob := call(200000, oobCopier, 0, 0, 0, 0, 0)
            retSizeOob := returndatasize()
        }
        (bool okValid, bytes memory retValid) = validCopier.call("");

        results[s] = keccak256(abi.encodePacked(
            okOob, retSizeOob, okValid, keccak256(retValid), retValid.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 10. MSTORE8 single-byte memory writes
    //
    // Runtime (17 bytes):
    //   PUSH1_0xFF PUSH0 MSTORE8      ; mem[0] = 0xFF
    //   PUSH1_0xAB PUSH1_1 MSTORE8    ; mem[1] = 0xAB
    //   PUSH1_0xCD PUSH1_31 MSTORE8   ; mem[31] = 0xCD
    //   PUSH1_32 PUSH0 RETURN         ; return 32 bytes
    //   = 60 ff 5f 53 60 ab 60 01 53 60 cd 60 1f 53 60 20 5f f3
    // ----------------------------------------------------------------
    function _mstore8Behavior(uint256 s) internal returns (uint256) {
        address victim = _deploy(_initCodeFor(
            hex"60ff5f5360ab60015360cd601f5360205ff3"
        ));
        require(victim != address(0), "deploy failed");

        (bool ok, bytes memory ret) = victim.call("");

        results[s] = keccak256(abi.encodePacked(ok, ret));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 11. Stack underflow — 8 patterns
    //
    // Solidity's compiler guarantees correct stack depth at every opcode.
    // These raw bytecodes attempt operations with insufficient stack items,
    // causing an exceptional halt. Each should revert with no return data.
    //
    // Patterns:
    //   a) 50           POP on empty stack (needs 1, has 0)
    //   b) 01           ADD on empty stack (needs 2, has 0)
    //   c) 80           DUP1 on empty stack (needs 1, has 0)
    //   d) 90           SWAP1 on empty stack (needs 2, has 0)
    //   e) 52           MSTORE on empty stack (needs 2, has 0)
    //   f) f3           RETURN on empty stack (needs 2, has 0)
    //   g) 60 01 01     PUSH1 1 then ADD (needs 2, has 1)
    //   h) 60 01 60 02 91  PUSH1 PUSH1 then SWAP2 (needs 3, has 2)
    // ----------------------------------------------------------------
    function _stackUnderflow(uint256 s) internal returns (uint256) {
        bytes32 h;

        h = _testUnderflow(h, hex"50");             // POP, empty stack
        h = _testUnderflow(h, hex"01");             // ADD, empty stack
        h = _testUnderflow(h, hex"80");             // DUP1, empty stack
        h = _testUnderflow(h, hex"90");             // SWAP1, empty stack
        h = _testUnderflow(h, hex"52");             // MSTORE, empty stack
        h = _testUnderflow(h, hex"f3");             // RETURN, empty stack
        h = _testUnderflow(h, hex"600101");         // PUSH1 1, ADD (1 item, needs 2)
        h = _testUnderflow(h, hex"6001600291");     // PUSH1 PUSH1 SWAP2 (2 items, needs 3)

        results[s] = h;
        return s + 1;
    }

    function _testUnderflow(bytes32 h, bytes memory code) internal returns (bytes32) {
        address victim = _deploy(_initCodeFor(code));
        require(victim != address(0), "underflow deploy failed");

        bool ok;
        uint256 retSize;
        assembly {
            ok := call(100000, victim, 0, 0, 0, 0, 0)
            retSize := returndatasize()
        }

        return keccak256(abi.encodePacked(h, ok, retSize, victim.code.length));
    }

    // ----------------------------------------------------------------
    // 12. Stack overflow — exceed the 1024-item depth limit
    //
    // Runtime (5 bytes):
    //   JUMPDEST PUSH0 PUSH1_0 JUMP
    //   = 5b 5f 60 00 56
    //
    // Each iteration leaves one extra item on the stack (net +1).
    // After 1024 iterations the stack is full. The next PUSH0
    // triggers a stack overflow → exceptional halt.
    //
    // ~14 gas per iteration × 1024 ≈ 14K gas, well within 100K cap.
    // ----------------------------------------------------------------
    function _stackOverflow(uint256 s) internal returns (uint256) {
        address victim = _deploy(_initCodeFor(hex"5b5f600056"));
        require(victim != address(0), "overflow deploy failed");

        bool ok;
        uint256 retSize;
        assembly {
            ok := call(100000, victim, 0, 0, 0, 0, 0)
            retSize := returndatasize()
        }

        results[s] = keccak256(abi.encodePacked(ok, retSize, victim.code.length));
        return s + 1;
    }
}
