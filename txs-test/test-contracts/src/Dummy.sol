// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

contract Dummy {
    address public owner;
    uint256 public stored;

    constructor(address _owner, uint256 _value) {
        owner = _owner;
        stored = _value;
    }
}
