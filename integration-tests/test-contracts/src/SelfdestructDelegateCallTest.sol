// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

// ======== Helper contracts ========

/// @dev Can receive ETH and selfdestruct to a beneficiary.
contract SdVictim {
    receive() external payable {}
    function destroy(address payable beneficiary) external {
        selfdestruct(beneficiary);
    }
}

/// @dev Selfdestruct function meant to be executed via DELEGATECALL.
contract SdImpl {
    function destroy(address payable beneficiary) external {
        selfdestruct(beneficiary);
    }
}

/// @dev Selfdestructs in constructor. Per EIP-6780, created + destroyed in
///      the same tx means actual destruction (code + storage wiped).
contract SdEphemeral {
    constructor(address payable beneficiary) payable {
        selfdestruct(beneficiary);
    }
}

/// @dev Selfdestructs then continues execution (post-Cancun: code not removed).
contract SdAndContinue {
    uint256 public storedValue;
    receive() external payable {}
    function destroyAndStore(address payable beneficiary) external returns (uint256) {
        selfdestruct(beneficiary);
        storedValue = 999;
        return 42;
    }
}

/// @dev Selfdestructs twice to two different beneficiaries in a single call.
contract SdDouble {
    receive() external payable {}
    function destroyTwice(address payable b1, address payable b2) external {
        selfdestruct(b1);
        // Post-Cancun: still alive, second selfdestruct executes (balance already 0)
        selfdestruct(b2);
    }
}

/// @dev Wrapper that CALLs a victim's destroy(), then reverts.
///      The revert should undo the selfdestruct's balance transfer.
contract SdRevertWrapper {
    function callDestroyThenRevert(SdVictim victim, address payable beneficiary) external {
        victim.destroy(beneficiary);
        revert("intentional revert after selfdestruct");
    }
}

/// @dev Wrapper for DELEGATECALL tests. Isolates balance from the main contract.
contract SdDelegateTester {
    receive() external payable {}

    function testDelegate(
        address impl,
        address payable beneficiary
    ) external returns (bool ok) {
        (ok,) = impl.delegatecall(
            abi.encodeWithSelector(SdImpl.destroy.selector, beneficiary)
        );
    }
}

/// @dev Middle relay for nested delegatecall chain: caller → this → impl.
contract SdMiddleRelay {
    function relayDestroy(address impl, address payable beneficiary) external {
        (bool ok,) = impl.delegatecall(
            abi.encodeWithSelector(SdImpl.destroy.selector, beneficiary)
        );
        assembly { mstore(0, ok) } // prevent optimizer removal
    }
}

/// @dev Wrapper for nested delegatecall tests.
contract SdNestedDelegateTester {
    receive() external payable {}

    function testNestedDelegate(
        address relay,
        address impl,
        address payable beneficiary
    ) external returns (bool ok) {
        (ok,) = relay.delegatecall(
            abi.encodeWithSelector(SdMiddleRelay.relayDestroy.selector, impl, beneficiary)
        );
    }
}

/// @dev Creates a SdVictim, funds it, selfdestructs it, then tries to call it
///      again. Used to test interaction with selfdestructed contracts.
///      Also: creates an ephemeral inside a reverting frame.
contract SdFactory {
    receive() external payable {}

    /// @dev Creates a victim, funds it, selfdestructs it to b1,
    ///      then re-funds and selfdestructs to b2 in the same tx.
    function createDestroyAndCallAgain(
        address payable b1,
        address payable b2
    ) external returns (uint256 b1Bal, uint256 b2Bal, uint256 victimBal, uint256 codeLen) {
        SdVictim v = new SdVictim();
        (bool ok1,) = address(v).call{value: 0.1 ether}("");
        require(ok1);
        v.destroy(b1);

        // Re-fund and call again (post-Cancun: code still there)
        (bool ok2,) = address(v).call{value: 0.05 ether}("");
        require(ok2);
        v.destroy(b2);

        b1Bal = b1.balance;
        b2Bal = b2.balance;
        victimBal = address(v).balance;
        codeLen = address(v).code.length;
    }

    /// @dev Creates an ephemeral child inside a reverting sub-call.
    ///      The CREATE + SELFDESTRUCT should be fully undone.
    function createEphemeralInRevertedFrame(
        address payable beneficiary
    ) external returns (bool callOk, uint256 benBal) {
        (callOk,) = address(this).call(
            abi.encodeWithSelector(this.revertingCreate.selector, beneficiary)
        );
        benBal = beneficiary.balance;
    }

    function revertingCreate(address payable beneficiary) external {
        new SdEphemeral{value: 0.1 ether}(beneficiary);
        revert("undo ephemeral");
    }
}

// ======== Main test contract ========

/// @dev Exercises SELFDESTRUCT and DELEGATECALL edge cases.
///      Stores keccak256 of observable state per test for REVM consistency checking.
contract SelfdestructDelegateCallTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    SdImpl public impl;
    SdMiddleRelay public relay;

    constructor() {
        impl = new SdImpl();
        relay = new SdMiddleRelay();
    }

    receive() external payable {}

    function runAll() external payable {
        require(msg.value >= 5 ether, "need >= 5 ETH");
        uint256 idx;

        idx = _selfdestructToSelf(idx);
        idx = _selfdestructToExisting(idx);
        idx = _selfdestructToNonExistent(idx);
        idx = _selfdestructZeroBalanceToEmpty(idx);
        idx = _doubleSelfdestruct(idx);
        idx = _selfdestructThenContinue(idx);
        idx = _selfdestructInRevertedCall(idx);
        idx = _createAndDestroySameTx(idx);
        idx = _createAndDestroySameTxZeroValue(idx);
        idx = _createEphemeralInRevertedFrame(idx);
        idx = _delegatecallSelfdestruct(idx);
        idx = _delegatecallSelfdestructToSelf(idx);
        idx = _nestedDelegatecallSelfdestruct(idx);
        idx = _callSelfdestructedContractAgain(idx);

        totalTests = idx;
    }

    // ----------------------------------------------------------------
    // 1. SELFDESTRUCT to self
    //    Post-Cancun: balance stays with the contract (sent to itself).
    // ----------------------------------------------------------------
    function _selfdestructToSelf(uint256 s) internal returns (uint256) {
        SdVictim v = new SdVictim();
        _fund(address(v), 0.1 ether);

        uint256 balBefore = address(v).balance;
        v.destroy(payable(address(v)));
        uint256 balAfter = address(v).balance;
        uint256 codeLen  = address(v).code.length;

        results[s] = keccak256(abi.encodePacked(balBefore, balAfter, codeLen));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 2. SELFDESTRUCT to an existing account
    // ----------------------------------------------------------------
    function _selfdestructToExisting(uint256 s) internal returns (uint256) {
        SdVictim v = new SdVictim();
        _fund(address(v), 0.1 ether);

        uint256 thisBefore = address(this).balance;
        uint256 vBefore    = address(v).balance;
        v.destroy(payable(address(this)));
        uint256 thisAfter = address(this).balance;
        uint256 vAfter    = address(v).balance;
        uint256 codeLen   = address(v).code.length;

        results[s] = keccak256(abi.encodePacked(
            thisBefore, thisAfter, vBefore, vAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 3. SELFDESTRUCT to a non-existent account (triggers account creation)
    // ----------------------------------------------------------------
    function _selfdestructToNonExistent(uint256 s) internal returns (uint256) {
        SdVictim v = new SdVictim();
        _fund(address(v), 0.1 ether);

        address payable fresh = payable(address(uint160(uint256(
            keccak256("sd_fresh_beneficiary_1")
        ))));
        uint256 freshBefore = fresh.balance;
        uint256 vBefore     = address(v).balance;
        v.destroy(fresh);
        uint256 freshAfter = fresh.balance;
        uint256 vAfter     = address(v).balance;

        results[s] = keccak256(abi.encodePacked(
            freshBefore, freshAfter, vBefore, vAfter
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 4. SELFDESTRUCT with zero balance to empty account
    // ----------------------------------------------------------------
    function _selfdestructZeroBalanceToEmpty(uint256 s) internal returns (uint256) {
        SdVictim v = new SdVictim(); // 0 balance

        address payable fresh = payable(address(uint160(uint256(
            keccak256("sd_fresh_zero_bal")
        ))));
        uint256 freshBefore = fresh.balance;
        v.destroy(fresh);
        uint256 freshAfter  = fresh.balance;
        uint256 codeLen     = address(v).code.length;

        results[s] = keccak256(abi.encodePacked(freshBefore, freshAfter, codeLen));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 5. Double SELFDESTRUCT to different beneficiaries in same call
    //    First sends entire balance; second sends 0 (already drained).
    // ----------------------------------------------------------------
    function _doubleSelfdestruct(uint256 s) internal returns (uint256) {
        SdDouble d = new SdDouble();
        _fund(address(d), 0.2 ether);

        address b1 = address(uint160(uint256(keccak256("sd_double_b1"))));
        address b2 = address(uint160(uint256(keccak256("sd_double_b2"))));

        uint256 b1Before = b1.balance;
        uint256 b2Before = b2.balance;
        uint256 dBefore  = address(d).balance;

        d.destroyTwice(payable(b1), payable(b2));

        uint256 b1After = b1.balance;
        uint256 b2After = b2.balance;
        uint256 dAfter  = address(d).balance;
        uint256 codeLen = address(d).code.length;

        results[s] = keccak256(abi.encodePacked(
            b1Before, b1After, b2Before, b2After, dBefore, dAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 6. SELFDESTRUCT then continue execution (post-Cancun)
    //    Verifies return value, storage write, and balance transfer.
    //    Uses low-level calls to handle potential behavioral differences.
    // ----------------------------------------------------------------
    function _selfdestructThenContinue(uint256 s) internal returns (uint256) {
        SdAndContinue c = new SdAndContinue();
        _fund(address(c), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_continue_ben")
        ))));
        uint256 benBefore = ben.balance;

        // Low-level call: avoids ABI-decode revert if execution halts at selfdestruct
        (bool callOk, bytes memory retData) = address(c).call(
            abi.encodeWithSelector(SdAndContinue.destroyAndStore.selector, ben)
        );

        uint256 benAfter = ben.balance;
        uint256 cBal     = address(c).balance;
        uint256 codeLen  = address(c).code.length;

        // Try reading storedValue (may fail if contract was destroyed)
        (bool readOk, bytes memory svData) = address(c).staticcall(
            abi.encodeWithSignature("storedValue()")
        );

        results[s] = keccak256(abi.encodePacked(
            callOk, retData, readOk, svData, benBefore, benAfter, cBal, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 7. SELFDESTRUCT inside a reverting CALL frame
    //    The revert undoes the balance transfer entirely.
    // ----------------------------------------------------------------
    function _selfdestructInRevertedCall(uint256 s) internal returns (uint256) {
        SdVictim v = new SdVictim();
        _fund(address(v), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_revert_ben")
        ))));
        uint256 vBefore   = address(v).balance;
        uint256 benBefore = ben.balance;

        SdRevertWrapper rw = new SdRevertWrapper();
        (bool ok,) = address(rw).call(
            abi.encodeWithSelector(
                SdRevertWrapper.callDestroyThenRevert.selector, v, ben
            )
        );

        uint256 vAfter   = address(v).balance;
        uint256 benAfter = ben.balance;
        uint256 codeLen  = address(v).code.length;

        results[s] = keccak256(abi.encodePacked(
            ok, vBefore, vAfter, benBefore, benAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 8. CREATE + SELFDESTRUCT in same tx (EIP-6780)
    //    The only case post-Cancun where the contract is actually destroyed.
    // ----------------------------------------------------------------
    function _createAndDestroySameTx(uint256 s) internal returns (uint256) {
        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_ephemeral_ben")
        ))));
        uint256 benBefore = ben.balance;

        SdEphemeral child = new SdEphemeral{value: 0.1 ether}(ben);

        uint256 benAfter      = ben.balance;
        uint256 childBal      = address(child).balance;
        uint256 childCodeLen  = address(child).code.length;

        results[s] = keccak256(abi.encodePacked(
            benBefore, benAfter, childBal, childCodeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 9. CREATE + SELFDESTRUCT with zero value (EIP-6780)
    // ----------------------------------------------------------------
    function _createAndDestroySameTxZeroValue(uint256 s) internal returns (uint256) {
        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_ephemeral_zero_ben")
        ))));

        SdEphemeral child = new SdEphemeral{value: 0}(ben);

        uint256 childBal     = address(child).balance;
        uint256 childCodeLen = address(child).code.length;
        uint256 benBal       = ben.balance;

        results[s] = keccak256(abi.encodePacked(childBal, childCodeLen, benBal));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 10. CREATE + SELFDESTRUCT inside a reverting frame
    //     Both the create and the selfdestruct should be fully undone.
    // ----------------------------------------------------------------
    function _createEphemeralInRevertedFrame(uint256 s) internal returns (uint256) {
        SdFactory factory = new SdFactory();
        _fund(address(factory), 0.2 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_ephemeral_reverted_ben")
        ))));
        uint256 benBefore = ben.balance;

        (bool callOk, uint256 benBal) =
            factory.createEphemeralInRevertedFrame(ben);

        results[s] = keccak256(abi.encodePacked(callOk, benBefore, benBal));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 11. DELEGATECALL + SELFDESTRUCT
    //     Selfdestruct operates on the *delegator's* context.
    //     Post-Cancun: delegator is NOT destroyed (not created this tx).
    // ----------------------------------------------------------------
    function _delegatecallSelfdestruct(uint256 s) internal returns (uint256) {
        SdDelegateTester dt = new SdDelegateTester();
        _fund(address(dt), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_delegate_ben")
        ))));
        uint256 dtBefore  = address(dt).balance;
        uint256 benBefore = ben.balance;

        bool ok = dt.testDelegate(address(impl), ben);

        uint256 dtAfter  = address(dt).balance;
        uint256 benAfter = ben.balance;
        uint256 codeLen  = address(dt).code.length;

        results[s] = keccak256(abi.encodePacked(
            ok, dtBefore, dtAfter, benBefore, benAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 12. DELEGATECALL + SELFDESTRUCT to self
    //     Beneficiary = address(this) in the delegator's context.
    //     Balance is sent to self → remains unchanged.
    // ----------------------------------------------------------------
    function _delegatecallSelfdestructToSelf(uint256 s) internal returns (uint256) {
        SdDelegateTester dt = new SdDelegateTester();
        _fund(address(dt), 0.1 ether);

        uint256 dtBefore = address(dt).balance;

        bool ok = dt.testDelegate(address(impl), payable(address(dt)));

        uint256 dtAfter = address(dt).balance;
        uint256 codeLen = address(dt).code.length;

        results[s] = keccak256(abi.encodePacked(ok, dtBefore, dtAfter, codeLen));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 13. Nested DELEGATECALL + SELFDESTRUCT
    //     tester → delegatecall → relay → delegatecall → impl.destroy()
    //     All in the tester's context.
    // ----------------------------------------------------------------
    function _nestedDelegatecallSelfdestruct(uint256 s) internal returns (uint256) {
        SdNestedDelegateTester ndt = new SdNestedDelegateTester();
        _fund(address(ndt), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("sd_nested_delegate_ben")
        ))));
        uint256 ndtBefore = address(ndt).balance;
        uint256 benBefore = ben.balance;

        bool ok = ndt.testNestedDelegate(address(relay), address(impl), ben);

        uint256 ndtAfter = address(ndt).balance;
        uint256 benAfter = ben.balance;
        uint256 codeLen  = address(ndt).code.length;

        results[s] = keccak256(abi.encodePacked(
            ok, ndtBefore, ndtAfter, benBefore, benAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 14. Call a selfdestructed contract again in the same tx
    //     Post-Cancun: code persists, second call should succeed.
    // ----------------------------------------------------------------
    function _callSelfdestructedContractAgain(uint256 s) internal returns (uint256) {
        SdFactory factory = new SdFactory();
        _fund(address(factory), 0.3 ether);

        address payable b1 = payable(address(uint160(uint256(
            keccak256("sd_again_b1")
        ))));
        address payable b2 = payable(address(uint160(uint256(
            keccak256("sd_again_b2")
        ))));

        (uint256 b1Bal, uint256 b2Bal, uint256 victimBal, uint256 codeLen) =
            factory.createDestroyAndCallAgain(b1, b2);

        results[s] = keccak256(abi.encodePacked(b1Bal, b2Bal, victimBal, codeLen));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // Internal helper
    // ----------------------------------------------------------------
    function _fund(address target, uint256 amount) internal {
        (bool ok,) = target.call{value: amount}("");
        require(ok, "fund failed");
    }
}
