// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Dummy} from "./Dummy.sol";

contract DummyFactory {
    event DummyCreated(address dummy, address owner, uint256 value);

    function createDummy(uint256 _value) external returns (address) {
        Dummy d = new Dummy(msg.sender, _value);
        emit DummyCreated(address(d), msg.sender, _value);
        return address(d);
    }
}
