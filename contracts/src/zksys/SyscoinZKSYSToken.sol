// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/access/AccessControlUpgradeable.sol";
import {ERC20VotesUpgradeable} from
    "@openzeppelin/contracts-upgradeable-v4/token/ERC20/extensions/ERC20VotesUpgradeable.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable-v4/proxy/utils/Initializable.sol";
import {SafeCastUpgradeable} from "@openzeppelin/contracts-upgradeable-v4/utils/math/SafeCastUpgradeable.sol";
import {SignatureChecker} from "@openzeppelin/contracts-v4/utils/cryptography/SignatureChecker.sol";

// SYSCOIN: Canonical L2 zkSYS token. L1/NEVM should use the standard bridge
// representation created for this L2-origin asset, matching ZK's token model.
// Minting is reserved for scheduled issuance contracts; burning is reserved for
// explicit burn paths such as the zkSYS paymaster.
contract SyscoinZKSYSToken is Initializable, ERC20VotesUpgradeable, AccessControlUpgradeable {
    using SignatureChecker for address;

    bytes32 public constant MINTER_ADMIN_ROLE = keccak256("MINTER_ADMIN_ROLE");
    bytes32 public constant BURNER_ADMIN_ROLE = keccak256("BURNER_ADMIN_ROLE");
    bytes32 public constant MINTER_ROLE = keccak256("MINTER_ROLE");
    bytes32 public constant BURNER_ROLE = keccak256("BURNER_ROLE");
    bytes32 public constant DELEGATION_TYPEHASH =
        keccak256("Delegation(address owner,address delegatee,uint256 nonce,uint256 expiry)");

    error ERC6372InconsistentClock();
    error DelegateSignatureExpired(uint256 expiry);
    error DelegateSignatureIsInvalid();
    error MaxSupplyExceeded(uint256 supply, uint256 maxSupply);

    uint8 private _customDecimals;
    uint256 public maxSupply;
    uint256[48] private __gap;

    constructor() {
        _disableInitializers();
    }

    function initialize(string memory _name, string memory _symbol, uint8 _decimals, address _admin)
        external
        initializer
    {
        require(_admin != address(0), "admin is zero");

        __ERC20_init(_name, _symbol);
        __ERC20Permit_init(_name);
        __ERC20Votes_init();

        uint256 maxSupply_ = 210_000_000 * 10 ** uint256(_decimals);
        require(maxSupply_ <= type(uint224).max, "max supply exceeds votes");

        _customDecimals = _decimals;
        maxSupply = maxSupply_;

        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(MINTER_ADMIN_ROLE, _admin);
        _grantRole(BURNER_ADMIN_ROLE, _admin);
        _setRoleAdmin(MINTER_ROLE, MINTER_ADMIN_ROLE);
        _setRoleAdmin(BURNER_ROLE, BURNER_ADMIN_ROLE);
    }

    function decimals() public view override returns (uint8) {
        return _customDecimals;
    }

    function clock() public view override returns (uint48) {
        return SafeCastUpgradeable.toUint48(block.timestamp);
    }

    // solhint-disable-next-line func-name-mixedcase
    function CLOCK_MODE() public view override returns (string memory) {
        if (clock() != block.timestamp) {
            revert ERC6372InconsistentClock();
        }
        return "mode=timestamp";
    }

    function mint(address _to, uint256 _amount) external onlyRole(MINTER_ROLE) returns (bool) {
        uint256 newSupply = totalSupply() + _amount;
        if (newSupply > maxSupply) {
            revert MaxSupplyExceeded(newSupply, maxSupply);
        }
        _mint(_to, _amount);
        return true;
    }

    function burn(address _from, uint256 _amount) external onlyRole(BURNER_ROLE) returns (bool) {
        _burn(_from, _amount);
        return true;
    }

    function delegateOnBehalf(address _signer, address _delegatee, uint256 _expiry, bytes calldata _signature)
        external
    {
        if (block.timestamp > _expiry) {
            revert DelegateSignatureExpired(_expiry);
        }

        bool isSignatureValid = _signer.isValidSignatureNow(
            _hashTypedDataV4(
                keccak256(abi.encode(DELEGATION_TYPEHASH, _signer, _delegatee, _useNonce(_signer), _expiry))
            ),
            _signature
        );
        if (!isSignatureValid) {
            revert DelegateSignatureIsInvalid();
        }

        _delegate(_signer, _delegatee);
    }

    function _afterTokenTransfer(address _from, address _to, uint256 _amount)
        internal
        override(ERC20VotesUpgradeable)
    {
        super._afterTokenTransfer(_from, _to, _amount);
    }

    function _mint(address _to, uint256 _amount) internal override(ERC20VotesUpgradeable) {
        super._mint(_to, _amount);
    }

    function _burn(address _account, uint256 _amount) internal override(ERC20VotesUpgradeable) {
        super._burn(_account, _amount);
    }

    function _maxSupply() internal view override returns (uint224) {
        return uint224(maxSupply);
    }
}
