// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

// ======== Helper contracts for combo tests ========

/// @dev Selfdestruct impl (reusable via delegatecall).
contract ComboSdImpl {
    receive() external payable {}
    function destroy(address payable beneficiary) external {
        selfdestruct(beneficiary);
    }
}

/// @dev Constructor that delegatecalls to a selfdestruct implementation.
///      EIP-6780: contract is created in same tx, SD via delegatecall
///      operates on the new contract's context -> should be fully destroyed.
///      SELFDESTRUCT halts the delegatecall frame; constructor continues.
contract ConstructorDelegateSd {
    bool public delegateResult;

    constructor(address sdImpl, address payable beneficiary) payable {
        (bool ok,) = sdImpl.delegatecall(
            abi.encodeWithSignature("destroy(address)", beneficiary)
        );
        delegateResult = ok;
    }
}

/// @dev Constructor delegatecalls SD, then continues storing values.
///      Tests that constructor execution continues after delegatecall
///      frame halts at SELFDESTRUCT.
contract ConstructorDelegateSdThenStore {
    uint256 public storedA;
    uint256 public storedB;
    bool public delegateResult;

    constructor(address sdImpl, address payable beneficiary) payable {
        (bool ok,) = sdImpl.delegatecall(
            abi.encodeWithSignature("destroy(address)", beneficiary)
        );
        delegateResult = ok;
        storedA = 42;
        storedB = 777;
    }
}

/// @dev Selfdestructs in constructor. Separate from SdEphemeral for isolation.
contract ComboSdGrandchild {
    constructor(address payable beneficiary) payable {
        selfdestruct(beneficiary);
    }
}

/// @dev Child that creates a grandchild which selfdestructs in its constructor.
contract NestingChild {
    address public grandchild;

    constructor(address payable sdBeneficiary) payable {
        ComboSdGrandchild gc = new ComboSdGrandchild{value: msg.value}(sdBeneficiary);
        grandchild = address(gc);
    }
}

/// @dev Factory for CREATE2 tests. Can deploy ComboSdGrandchild at a
///      deterministic address and predict that address.
contract Create2Factory {
    function deploy(bytes32 salt, address payable beneficiary) external payable returns (address) {
        bytes memory bytecode = abi.encodePacked(
            type(ComboSdGrandchild).creationCode,
            abi.encode(beneficiary)
        );
        address addr;
        assembly {
            addr := create2(callvalue(), add(bytecode, 0x20), mload(bytecode), salt)
        }
        return addr;
    }

    function predictAddress(bytes32 salt, address payable beneficiary) external view returns (address) {
        bytes memory bytecode = abi.encodePacked(
            type(ComboSdGrandchild).creationCode,
            abi.encode(beneficiary)
        );
        bytes32 hash = keccak256(abi.encodePacked(
            bytes1(0xff),
            address(this),
            salt,
            keccak256(bytecode)
        ));
        return address(uint160(uint256(hash)));
    }

    receive() external payable {}
}

/// @dev Impl that creates a child which selfdestructs in its constructor.
///      Used inside a DELEGATECALL: the CREATE runs in the delegator's context.
contract CreateChildSdImpl {
    function createEphemeral(address payable beneficiary) external {
        new ComboSdGrandchild(beneficiary);
    }
}

/// @dev Wrapper for delegatecall-CREATE tests.
contract DelegateCreateTester {
    receive() external payable {}

    function doDelegate(address createImpl, address payable beneficiary) external {
        (bool ok,) = createImpl.delegatecall(
            abi.encodeWithSignature("createEphemeral(address)", beneficiary)
        );
        assembly { mstore(0, ok) }
    }
}

/// @dev Can selfdestruct to any address specified by uint160.
contract SdToAnyAddress {
    receive() external payable {}

    function destroyTo(address payable target) external {
        selfdestruct(target);
    }
}

/// @dev Selfdestructs twice via two delegatecalls in one call.
contract DoubleDelegateSdTester {
    receive() external payable {}

    function doubleDelegateSd(
        address sdImpl,
        address payable ben1,
        address payable ben2
    ) external returns (bool ok1, bool ok2) {
        (ok1,) = sdImpl.delegatecall(
            abi.encodeWithSignature("destroy(address)", ben1)
        );
        (ok2,) = sdImpl.delegatecall(
            abi.encodeWithSignature("destroy(address)", ben2)
        );
    }
}

/// @dev Contract with recursive selfdestruct: calls itself with lower depth,
///      then selfdestructs. Inner calls are regular CALLs (not delegatecall).
contract RecursiveSd {
    uint256 public depth;
    receive() external payable {}

    function recursiveDestroy(address payable beneficiary, uint256 maxDepth) external {
        depth = maxDepth;
        if (maxDepth > 0) {
            (bool ok,) = address(this).call(
                abi.encodeWithSignature(
                    "recursiveDestroy(address,uint256)",
                    beneficiary,
                    maxDepth - 1
                )
            );
            assembly { mstore(0, ok) }
        }
        selfdestruct(beneficiary);
    }
}

// ======== Main test contract ========

/// @dev Exercises novel SELFDESTRUCT + DELEGATECALL + constructor combos.
///      Stores keccak256 of observable state per test for REVM consistency checking.
contract SelfdestructComboTest {
    mapping(uint256 => bytes32) public results;
    uint256 public totalTests;

    ComboSdImpl public sdImpl;

    constructor() {
        sdImpl = new ComboSdImpl();
    }

    receive() external payable {}

    function runAll() external payable {
        require(msg.value >= 5 ether, "need >= 5 ETH");
        uint256 idx;

        idx = _constructorDelegatecallSd(idx);
        idx = _constructorDelegatecallSdThenStore(idx);
        idx = _nestedCreateChainSd(idx);
        idx = _create2DestroyAndRedeploy(idx);
        idx = _delegatecallCreateEphemeral(idx);
        idx = _selfdestructToPrecompile(idx);
        idx = _doubleDelegatecallSd(idx);
        idx = _recursiveSelfdestructThenRead(idx);
        idx = _preFundViaSdThenCreate2(idx);
        idx = _implSurvivesDelegatecallSd(idx);

        totalTests = idx;
    }

    // ----------------------------------------------------------------
    // 1. Constructor DELEGATECALL to SELFDESTRUCT
    //    New contract's constructor delegatecalls sdImpl.destroy().
    //    EIP-6780: contract created in same tx, SD via delegatecall
    //    operates on it -> marked for full destruction.
    //    But SELFDESTRUCT only halts the delegatecall frame;
    //    the constructor continues and sets delegateResult.
    // ----------------------------------------------------------------
    function _constructorDelegatecallSd(uint256 s) internal returns (uint256) {
        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_ctor_dc_sd_ben")
        ))));
        uint256 benBefore = ben.balance;

        ConstructorDelegateSd child = new ConstructorDelegateSd{value: 0.1 ether}(
            address(sdImpl), ben
        );

        uint256 benAfter = ben.balance;
        uint256 childBal = address(child).balance;
        uint256 codeLen = address(child).code.length;

        (bool readOk, bytes memory data) = address(child).staticcall(
            abi.encodeWithSignature("delegateResult()")
        );

        results[s] = keccak256(abi.encodePacked(
            benBefore, benAfter, childBal, codeLen, readOk, data
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 2. Constructor DELEGATECALL to SD, then continues storing values
    //    Delegatecall frame halts at selfdestruct, constructor continues.
    //    Writes storedA=42, storedB=777, then deploys runtime code.
    //    EIP-6780 should wipe code+storage at end-of-tx.
    // ----------------------------------------------------------------
    function _constructorDelegatecallSdThenStore(uint256 s) internal returns (uint256) {
        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_ctor_dc_sd_store_ben")
        ))));
        uint256 benBefore = ben.balance;

        ConstructorDelegateSdThenStore child =
            new ConstructorDelegateSdThenStore{value: 0.1 ether}(
                address(sdImpl), ben
            );

        bytes32 part1;
        bytes32 part2;

        {
            uint256 benAfter = ben.balance;
            uint256 childBal = address(child).balance;
            uint256 codeLen = address(child).code.length;
            part1 = keccak256(abi.encodePacked(benBefore, benAfter, childBal, codeLen));
        }

        {
            (bool okA, bytes memory dataA) = address(child).staticcall(
                abi.encodeWithSignature("storedA()")
            );
            (bool okB, bytes memory dataB) = address(child).staticcall(
                abi.encodeWithSignature("storedB()")
            );
            (bool okDR, bytes memory dataDR) = address(child).staticcall(
                abi.encodeWithSignature("delegateResult()")
            );
            part2 = keccak256(abi.encodePacked(
                okA, dataA, okB, dataB, okDR, dataDR
            ));
        }

        results[s] = keccak256(abi.encodePacked(part1, part2));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 3. Nested CREATE chain with selfdestruct
    //    Parent -> creates NestingChild -> child constructor creates
    //    ComboSdGrandchild -> grandchild selfdestructs (EIP-6780).
    //    Child should survive. Grandchild should be destroyed.
    // ----------------------------------------------------------------
    function _nestedCreateChainSd(uint256 s) internal returns (uint256) {
        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_nested_create_sd_ben")
        ))));
        uint256 benBefore = ben.balance;

        NestingChild child = new NestingChild{value: 0.1 ether}(ben);

        uint256 benAfter = ben.balance;
        address gcAddr = child.grandchild();
        uint256 gcBal = gcAddr.balance;
        uint256 gcCodeLen = gcAddr.code.length;
        uint256 childBal = address(child).balance;
        uint256 childCodeLen = address(child).code.length;

        results[s] = keccak256(abi.encodePacked(
            benBefore, benAfter, gcBal, gcCodeLen, childBal, childCodeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 4. CREATE2 + EIP-6780 destroy + re-CREATE2
    //    First CREATE2: constructor selfdestructs (EIP-6780 destroys).
    //    Second CREATE2 at same address: tests nonce reset behavior.
    //    Within the same tx, nonce may still be 1 -> second CREATE2 fails.
    //    Any difference between implementations surfaces here.
    // ----------------------------------------------------------------
    function _create2DestroyAndRedeploy(uint256 s) internal returns (uint256) {
        Create2Factory factory = new Create2Factory();
        _fund(address(factory), 0.3 ether);

        bytes32 salt = bytes32(uint256(0x1234));
        bytes32 part1;
        bytes32 part2;

        // First deployment: selfdestructs in constructor (EIP-6780)
        {
            address payable ben1 = payable(address(uint160(uint256(
                keccak256("combo_c2_sd_ben1")
            ))));
            address addr1 = factory.deploy{value: 0.1 ether}(salt, ben1);
            part1 = keccak256(abi.encodePacked(
                addr1, ben1.balance, addr1.code.length, addr1.balance
            ));
        }

        // Attempt re-deploy at same address
        {
            address payable ben2 = payable(address(uint160(uint256(
                keccak256("combo_c2_sd_ben2")
            ))));
            (bool ok2, bytes memory ret2) = address(factory).call{value: 0.05 ether}(
                abi.encodeWithSelector(Create2Factory.deploy.selector, salt, ben2)
            );
            address addr2;
            if (ok2 && ret2.length >= 32) {
                addr2 = abi.decode(ret2, (address));
            }
            part2 = keccak256(abi.encodePacked(
                ok2, addr2, addr2.code.length, ben2.balance
            ));
        }

        results[s] = keccak256(abi.encodePacked(part1, part2));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 5. DELEGATECALL to code that CREATEs a child that selfdestructs
    //    CREATE inside delegatecall uses the delegator's nonce.
    //    Child selfdestructs in constructor (EIP-6780).
    // ----------------------------------------------------------------
    function _delegatecallCreateEphemeral(uint256 s) internal returns (uint256) {
        DelegateCreateTester tester = new DelegateCreateTester();
        _fund(address(tester), 0.2 ether);

        CreateChildSdImpl createImpl = new CreateChildSdImpl();

        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_dc_create_sd_ben")
        ))));
        uint256 benBefore = ben.balance;
        uint256 testerBalBefore = address(tester).balance;

        (bool ok,) = address(tester).call(
            abi.encodeWithSelector(
                DelegateCreateTester.doDelegate.selector,
                address(createImpl), ben
            )
        );

        uint256 benAfter = ben.balance;
        uint256 testerBalAfter = address(tester).balance;
        uint256 testerCodeLen = address(tester).code.length;

        results[s] = keccak256(abi.encodePacked(
            ok, benBefore, benAfter, testerBalBefore, testerBalAfter, testerCodeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 6. SELFDESTRUCT to a precompile address (address(1) = ecrecover)
    //    Edge case beneficiary. Balance should still be transferred.
    //    Tests whether precompile account balance tracking works.
    // ----------------------------------------------------------------
    function _selfdestructToPrecompile(uint256 s) internal returns (uint256) {
        SdToAnyAddress victim = new SdToAnyAddress();
        _fund(address(victim), 0.1 ether);

        uint256 precompileBefore = address(1).balance;
        uint256 victimBefore = address(victim).balance;

        victim.destroyTo(payable(address(1)));

        uint256 precompileAfter = address(1).balance;
        uint256 victimAfter = address(victim).balance;
        uint256 codeLen = address(victim).code.length;

        results[s] = keccak256(abi.encodePacked(
            precompileBefore, precompileAfter, victimBefore, victimAfter, codeLen
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 7. Double DELEGATECALL selfdestruct from same contract
    //    First delegatecall sends entire balance to ben1.
    //    Second delegatecall sends 0 (already drained) to ben2.
    //    Both delegatecalls halt their frames but the caller continues.
    // ----------------------------------------------------------------
    function _doubleDelegatecallSd(uint256 s) internal returns (uint256) {
        DoubleDelegateSdTester tester = new DoubleDelegateSdTester();
        _fund(address(tester), 0.1 ether);

        address payable ben1 = payable(address(uint160(uint256(
            keccak256("combo_dbl_dc_sd_ben1")
        ))));
        address payable ben2 = payable(address(uint160(uint256(
            keccak256("combo_dbl_dc_sd_ben2")
        ))));

        bytes32 part1;
        {
            uint256 testerBefore = address(tester).balance;
            uint256 ben1Before = ben1.balance;
            uint256 ben2Before = ben2.balance;
            part1 = keccak256(abi.encodePacked(testerBefore, ben1Before, ben2Before));
        }

        (bool ok1, bool ok2) = tester.doubleDelegateSd(
            address(sdImpl), ben1, ben2
        );

        results[s] = keccak256(abi.encodePacked(
            part1, ok1, ok2,
            address(tester).balance, ben1.balance, ben2.balance,
            address(tester).code.length
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 8. Recursive selfdestruct then read storage
    //    Contract calls itself (regular CALL, not delegatecall) with
    //    decreasing depth, then selfdestructs at each level.
    //    After the top-level call, read storage via STATICCALL.
    //    Post-Cancun: code persists, storage may or may not survive
    //    depending on whether EIP-6780 applies (created in same tx).
    // ----------------------------------------------------------------
    function _recursiveSelfdestructThenRead(uint256 s) internal returns (uint256) {
        RecursiveSd rsv = new RecursiveSd();
        _fund(address(rsv), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_recursive_sd_ben")
        ))));
        uint256 benBefore = ben.balance;

        (bool callOk,) = address(rsv).call(
            abi.encodeWithSignature(
                "recursiveDestroy(address,uint256)",
                ben, uint256(2)
            )
        );

        uint256 benAfter = ben.balance;
        uint256 rsvBal = address(rsv).balance;
        uint256 codeLen = address(rsv).code.length;

        (bool readOk, bytes memory depthData) = address(rsv).staticcall(
            abi.encodeWithSignature("depth()")
        );

        results[s] = keccak256(abi.encodePacked(
            callOk, benBefore, benAfter, rsvBal, codeLen, readOk, depthData
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 9. Pre-fund address via selfdestruct, then CREATE2 at that address
    //    Use SELFDESTRUCT to send ETH to a predictable CREATE2 address.
    //    Then CREATE2 at that address. The new contract should see the
    //    pre-existing balance. Tests CREATE2 with pre-funded address.
    // ----------------------------------------------------------------
    function _preFundViaSdThenCreate2(uint256 s) internal returns (uint256) {
        Create2Factory factory = new Create2Factory();
        _fund(address(factory), 0.3 ether);

        address payable dummyBen = payable(address(uint160(uint256(
            keccak256("combo_prefund_dummy_ben")
        ))));
        bytes32 salt = bytes32(uint256(0xbeef));

        // Predict the address
        address predicted = factory.predictAddress(salt, dummyBen);

        // Pre-fund the predicted address via selfdestruct
        {
            SdToAnyAddress prefunder = new SdToAnyAddress();
            _fund(address(prefunder), 0.05 ether);
            prefunder.destroyTo(payable(predicted));
        }

        uint256 predictedBalBefore = predicted.balance;

        // CREATE2 at the pre-funded address
        address deployed = factory.deploy{value: 0.01 ether}(salt, dummyBen);

        results[s] = keccak256(abi.encodePacked(
            predictedBalBefore, deployed.balance, deployed.code.length,
            dummyBen.balance, deployed == predicted
        ));
        return s + 1;
    }

    // ----------------------------------------------------------------
    // 10. Impl contract survives after being used via delegatecall SD
    //     After delegatecall to sdImpl.destroy(), the impl contract
    //     must be unaffected: its code, storage, balance are untouched
    //     (delegatecall operates on the caller's context, not the impl's).
    // ----------------------------------------------------------------
    function _implSurvivesDelegatecallSd(uint256 s) internal returns (uint256) {
        ComboSdImpl freshImpl = new ComboSdImpl();
        _fund(address(freshImpl), 0.05 ether);

        DoubleDelegateSdTester tester = new DoubleDelegateSdTester();
        _fund(address(tester), 0.1 ether);

        address payable ben = payable(address(uint160(uint256(
            keccak256("combo_impl_survives_ben")
        ))));

        bytes32 implBefore = keccak256(abi.encodePacked(
            address(freshImpl).balance, address(freshImpl).code.length
        ));

        (bool ok1, bool ok2) = tester.doubleDelegateSd(
            address(freshImpl), ben, ben
        );

        results[s] = keccak256(abi.encodePacked(
            implBefore, ok1, ok2,
            address(freshImpl).balance, address(freshImpl).code.length,
            address(tester).balance, address(tester).code.length,
            ben.balance
        ));
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
