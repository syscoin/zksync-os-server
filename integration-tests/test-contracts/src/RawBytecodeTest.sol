// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

/// @dev Deploys raw EVM bytecode contracts (impossible to generate via Solidity)
///      and exercises them. Any divergence in storage, balance, nonce, or code
///      between zksync-os and REVM will surface via the consistency checker.
///
/// IMPORTANT: Raw bytecode contracts have no Solidity-style payable checks,
/// so ANY call (including ETH transfers) triggers their code. We deploy
/// them with value via CREATE to set initial balance without executing runtime.
///
/// Bytecode patterns tested (all impossible in Solidity):
///   - Minimal 2-byte contracts (CALLER SELFDESTRUCT, PUSH0 SELFDESTRUCT)
///   - Dead code after SELFDESTRUCT opcode
///   - SELFBALANCE as selfdestruct beneficiary
///   - Raw init code that selfdestructs (no RETURN, no runtime code)
///   - Raw init code that SSTOREs then selfdestructs
///   - SELFDESTRUCT inside STATICCALL context (gas-capped)
///   - EXTCODE ops on selfdestructed contracts within same tx
///   - Double SELFDESTRUCT in raw bytecode (two consecutive FFs)
///   - CREATE2 with SD init code + re-CREATE2 at same address
contract RawBytecodeTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    receive() external payable {}

    function runAll() external payable {
        require(msg.value >= 3 ether, "need >= 3 ETH");
        uint256 idx;

        idx = _minimalSelfdestruct(idx);
        idx = _selfdestructToZero(idx);
        idx = _deadCodeAfterSd(idx);
        idx = _selfbalanceSelfdestruct(idx);
        idx = _initCodeSelfdestruct(idx);
        idx = _initCodeSstoreThenSd(idx);
        idx = _selfdestructInStaticcall(idx);
        idx = _extcodeOpsAfterSd(idx);
        idx = _doubleSelfdestructRaw(idx);
        idx = _create2SdInitThenRedeploy(idx);

        totalTests = idx;
    }

    // ======== Bytecode helpers ========

    /// @dev Builds init code that deploys the given runtime bytecode.
    ///      Prefix is 11 bytes, runtime starts at offset 0x0b.
    function _initCodeFor(bytes memory runtime) internal pure returns (bytes memory) {
        require(runtime.length <= 255, "runtime too long");
        return abi.encodePacked(
            hex"60", uint8(runtime.length),
            hex"80600b6000396000f3",
            runtime
        );
    }

    function _deployValue(bytes memory initCode, uint256 val) internal returns (address addr) {
        assembly {
            addr := create(val, add(initCode, 0x20), mload(initCode))
        }
    }

    function _deploy2Value(bytes memory initCode, bytes32 salt, uint256 val) internal returns (address addr) {
        assembly {
            addr := create2(val, add(initCode, 0x20), mload(initCode), salt)
        }
    }

    // ----------------------------------------------------------------
    // 1. Minimal SELFDESTRUCT (CALLER SELFDESTRUCT = 0x33FF)
    //    Just 2 bytes of runtime code. Selfdestructs to msg.sender.
    //    Solidity never generates such minimal contracts.
    // ----------------------------------------------------------------
    function _minimalSelfdestruct(uint256 s) internal returns (uint256) {
        // Deploy with 0.1 ETH (don't _fund — it would trigger the code)
        address victim = _deployValue(_initCodeFor(hex"33ff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        uint256 victimBal = victim.balance;
        uint256 thisBefore = address(this).balance;

        // Call triggers: CALLER(=this) SELFDESTRUCT → balance to this
        (bool ok,) = victim.call("");

        results[s] = keccak256(abi.encodePacked(
            ok, victimBal, victim.balance, victim.code.length,
            thisBefore, address(this).balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 2. SELFDESTRUCT to address(0) (PUSH0 SELFDESTRUCT = 0x5FFF)
    //    Sends balance to the zero address.
    // ----------------------------------------------------------------
    function _selfdestructToZero(uint256 s) internal returns (uint256) {
        address victim = _deployValue(_initCodeFor(hex"5fff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        uint256 victimBal = victim.balance;
        uint256 zeroBefore = address(0).balance;

        (bool ok,) = victim.call("");

        results[s] = keccak256(abi.encodePacked(
            ok, victimBal, victim.balance, victim.code.length,
            zeroBefore, address(0).balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 3. Dead code after SELFDESTRUCT
    //    Runtime: CALLER SELFDESTRUCT PUSH1 0x2a PUSH1 0 MSTORE
    //             PUSH1 0x20 PUSH1 0 RETURN
    //    Code after FF should never execute. If it does, the call
    //    would return 42 (0x2a) as return data.
    // ----------------------------------------------------------------
    function _deadCodeAfterSd(uint256 s) internal returns (uint256) {
        // 33 FF 60 2a 60 00 52 60 20 60 00 F3
        address victim = _deployValue(
            _initCodeFor(hex"33ff602a60005260206000f3"),
            0.1 ether
        );
        require(victim != address(0), "deploy failed");

        uint256 thisBefore = address(this).balance;
        (bool ok, bytes memory retData) = victim.call("");

        results[s] = keccak256(abi.encodePacked(
            ok, retData, victim.balance, victim.code.length,
            thisBefore, address(this).balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 4. SELFBALANCE SELFDESTRUCT (0x47FF)
    //    Selfdestructs to address(selfbalance). The beneficiary IS
    //    the contract's own balance value interpreted as an address.
    //    Completely impossible in Solidity.
    //    With 0.1 ETH, beneficiary = address(100000000000000000).
    // ----------------------------------------------------------------
    function _selfbalanceSelfdestruct(uint256 s) internal returns (uint256) {
        address victim = _deployValue(_initCodeFor(hex"47ff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        uint256 victimBal = victim.balance;
        address beneficiary = address(uint160(victimBal));
        uint256 benBefore = beneficiary.balance;

        (bool ok,) = victim.call("");

        results[s] = keccak256(abi.encodePacked(
            ok, victimBal, victim.balance, victim.code.length,
            beneficiary, benBefore, beneficiary.balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 5. Init code that selfdestructs (no runtime deployed)
    //    Init code: CALLER SELFDESTRUCT (0x33FF)
    //    SELFDESTRUCT halts init code → CREATE produces account with
    //    empty code. EIP-6780 marks for full destruction.
    // ----------------------------------------------------------------
    function _initCodeSelfdestruct(uint256 s) internal returns (uint256) {
        uint256 thisBefore = address(this).balance;

        // Init code is just: CALLER SELFDESTRUCT
        address created = _deployValue(hex"33ff", 0.1 ether);

        bool deployOk = (created != address(0));

        results[s] = keccak256(abi.encodePacked(
            deployOk, created, created.balance, created.code.length,
            thisBefore, address(this).balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 6. Init code: SSTORE then SELFDESTRUCT
    //    Init code: PUSH1 0x42 PUSH1 0 SSTORE ORIGIN SELFDESTRUCT
    //    = 60 42 60 00 55 32 FF
    //    Writes 0x42 to storage slot 0, then selfdestructs to tx.origin.
    //    No RETURN → empty runtime code. EIP-6780 marks for destruction.
    //    The consistency checker will see the SSTORE in storage diffs.
    // ----------------------------------------------------------------
    function _initCodeSstoreThenSd(uint256 s) internal returns (uint256) {
        address created = _deployValue(hex"604260005532ff", 0.1 ether);

        bool deployOk = (created != address(0));

        results[s] = keccak256(abi.encodePacked(
            deployOk, created, created.balance, created.code.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 7. SELFDESTRUCT inside STATICCALL (gas-capped)
    //    SELFDESTRUCT is state-modifying → exceptional halt in static
    //    context, consuming all forwarded gas. Cap gas to 100K via
    //    assembly to avoid draining the entire tx gas budget.
    // ----------------------------------------------------------------
    function _selfdestructInStaticcall(uint256 s) internal returns (uint256) {
        address victim = _deployValue(_initCodeFor(hex"33ff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        uint256 victimBal = victim.balance;
        bool ok;
        uint256 retSize;
        assembly {
            ok := staticcall(100000, victim, 0, 0, 0, 0)
            retSize := returndatasize()
        }

        results[s] = keccak256(abi.encodePacked(
            ok, retSize, victimBal, victim.balance, victim.code.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 8. EXTCODE ops on selfdestructed contract within same tx
    //    Deploy a minimal SD contract, call it (selfdestructs), then
    //    check EXTCODESIZE, EXTCODEHASH, and BALANCE of the victim.
    //    EIP-6780: created in same tx, code cleared at end-of-tx.
    //    Within the tx: code should still be visible.
    // ----------------------------------------------------------------
    function _extcodeOpsAfterSd(uint256 s) internal returns (uint256) {
        address victim = _deployValue(_initCodeFor(hex"33ff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        // Call to trigger selfdestruct
        (bool ok,) = victim.call("");

        // Check EXTCODE ops on the selfdestructed victim
        uint256 extSize;
        bytes32 extHash;
        uint256 bal;
        assembly {
            extSize := extcodesize(victim)
            extHash := extcodehash(victim)
            bal := balance(victim)
        }

        results[s] = keccak256(abi.encodePacked(
            ok, extSize, extHash, bal
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 9. Double SELFDESTRUCT in raw bytecode
    //    Runtime: CALLER SELFDESTRUCT CALLER SELFDESTRUCT = 33 FF 33 FF
    //    First SELFDESTRUCT halts execution. Second never runs.
    //    The VM must correctly handle the dead code bytes.
    // ----------------------------------------------------------------
    function _doubleSelfdestructRaw(uint256 s) internal returns (uint256) {
        address victim = _deployValue(_initCodeFor(hex"33ff33ff"), 0.1 ether);
        require(victim != address(0), "deploy failed");

        uint256 victimBal = victim.balance;
        uint256 thisBefore = address(this).balance;

        (bool ok,) = victim.call("");

        results[s] = keccak256(abi.encodePacked(
            ok, victimBal, victim.balance, victim.code.length,
            thisBefore, address(this).balance
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 10. CREATE2 with SD init code + re-CREATE2 at same address
    //     First CREATE2: init code = ORIGIN SELFDESTRUCT (32FF).
    //     Selfdestructs to tx.origin during init. EIP-6780 destroys.
    //     Second CREATE2 with same salt: tests if address is reusable
    //     within the same tx (nonce/code state after EIP-6780).
    // ----------------------------------------------------------------
    function _create2SdInitThenRedeploy(uint256 s) internal returns (uint256) {
        bytes memory initCode = hex"32ff"; // ORIGIN SELFDESTRUCT
        bytes32 salt = bytes32(uint256(0xdead));

        address addr1 = _deploy2Value(initCode, salt, 0.1 ether);
        bool deploy1Ok = (addr1 != address(0));
        uint256 addr1Bal = addr1.balance;
        uint256 addr1Code = addr1.code.length;

        // Second CREATE2 at same address (may fail due to nonce/collision)
        address addr2 = _deploy2Value(initCode, salt, 0.05 ether);
        bool deploy2Ok = (addr2 != address(0));

        results[s] = keccak256(abi.encodePacked(
            deploy1Ok, addr1, addr1Bal, addr1Code,
            deploy2Ok, addr2
        ));
        return s + 1;
    }
}
