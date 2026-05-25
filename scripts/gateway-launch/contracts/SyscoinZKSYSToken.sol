// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {AccessControl} from "@openzeppelin/contracts-v4/access/AccessControl.sol";
import {ERC20} from "@openzeppelin/contracts-v4/token/ERC20/ERC20.sol";

// SYSCOIN: Mainnet ZKSYS token used by the Gateway launcher. Unlike the
// testnet token, minting is gated by AccessControl.
contract SyscoinZKSYSToken is ERC20, AccessControl {
    bytes32 public constant MINTER_ROLE = keccak256("MINTER_ROLE");

    uint8 private immutable _customDecimals;

    constructor(string memory _name, string memory _symbol, uint8 _decimals, address _admin) ERC20(_name, _symbol) {
        require(_admin != address(0), "admin is zero");

        _customDecimals = _decimals;
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(MINTER_ROLE, _admin);
    }

    function decimals() public view override returns (uint8) {
        return _customDecimals;
    }

    function mint(address _to, uint256 _amount) external onlyRole(MINTER_ROLE) returns (bool) {
        _mint(_to, _amount);
        return true;
    }
}
