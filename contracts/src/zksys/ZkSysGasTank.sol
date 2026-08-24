// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IZkSysGasToken {
    function transferFrom(address _from, address _to, uint256 _amount) external returns (bool);
    function transfer(address _to, uint256 _amount) external returns (bool);
    function balanceOf(address _account) external view returns (uint256);
    function burn(address _from, uint256 _amount) external returns (bool);
    function decimals() external view returns (uint8);
}

// SYSCOIN: prepaid zkSYS gas ledger read and written directly by the patched
// ZKsync OS bootloader (see zksync-os basic_bootloader
// `transaction_flow/zk/syscoin_gas_tank.rs`).
//
// Funding the tank is the opt-in for paying L2 fees in zkSYS: whenever the
// sender's credit covers the full fee prepayment of a transaction, the
// bootloader debits `credit[sender]` 1:1 instead of the native SYS balance.
// `totalCredits` is intentionally NOT reduced by the precharge: it still backs
// the pending refund and operator tip while the transaction executes, so the
// precharge can never appear as burnable surplus mid-transaction. After
// execution the bootloader credits the gas refund and the operator tip to
// their `credit` entries and reduces `totalCredits` exactly once, by the
// burned portion of the fee (precharge minus refund minus tip). The backing
// zkSYS stays in this contract, so `token.balanceOf(this) - totalCredits`
// accumulates as surplus that anyone can destroy via `burnSurplus()`.
//
// STORAGE LAYOUT IS CONSENSUS-CRITICAL. The bootloader hardcodes:
//   slot 0: mapping(address => uint256) credit
//   slot 1: uint256 totalCredits
// Never reorder, remove, or prepend state variables, and never deploy this
// contract behind an upgradeable proxy.
contract ZkSysGasTank {
    /// @dev Slot 0. Read/written by the bootloader during fee charging.
    mapping(address => uint256) private _credit;
    /// @dev Slot 1. Read/written by the bootloader during fee charging.
    uint256 private _totalCredits;

    IZkSysGasToken public immutable token;

    // SYSCOIN: transient reentrancy state deliberately consumes no persistent
    // storage slot, preserving the consensus-critical slot 0/1 layout above.
    // ZKsync OS implements Cancun TLOAD/TSTORE and clears this state per tx.
    bool private transient _tokenCallEntered;

    event Funded(address indexed funder, address indexed account, uint256 amount);
    event Withdrawn(address indexed account, uint256 amount);
    event SurplusBurned(address indexed caller, uint256 amount);

    error ZeroAddress();
    error ZeroAmount();
    error TokenDecimalsMismatch(uint8 decimals);
    error InsufficientCredit(uint256 credit, uint256 requested);
    error TotalCreditsUnderflow(uint256 totalCredits, uint256 requested);
    error InsufficientBacking(uint256 balance, uint256 outstanding);
    error InsufficientFundingBalance(uint256 balance, uint256 requested);
    error TokenBalanceMismatch(uint256 expectedBalance, uint256 actualBalance);
    error ReentrantCall();
    error NoSurplus();
    error TransferFailed();

    modifier nonReentrantTokenCall() {
        if (_tokenCallEntered) {
            revert ReentrantCall();
        }
        _tokenCallEntered = true;
        _;
        _tokenCallEntered = false;
    }

    constructor(IZkSysGasToken _token) {
        if (address(_token) == address(0)) {
            revert ZeroAddress();
        }
        // The bootloader debits the tank 1:1 against native (18-decimals) fee
        // amounts, so the token must use native decimals.
        uint8 tokenDecimals = _token.decimals();
        if (tokenDecimals != 18) {
            revert TokenDecimalsMismatch(tokenDecimals);
        }
        token = _token;
    }

    function creditOf(address _account) external view returns (uint256) {
        return _credit[_account];
    }

    function totalCredits() external view returns (uint256) {
        return _totalCredits;
    }

    /// @notice zkSYS held by the tank in excess of outstanding credits, i.e.
    /// base fees burned via tank payments that are pending `burnSurplus()`.
    function surplus() external view returns (uint256) {
        uint256 balance = token.balanceOf(address(this));
        uint256 outstanding = _totalCredits;
        return balance > outstanding ? balance - outstanding : 0;
    }

    /// @notice Prepay gas for yourself by depositing zkSYS.
    function fund(uint256 _amount) external nonReentrantTokenCall {
        _fundFor(msg.sender, _amount);
    }

    /// @notice Prepay gas for `_account` (gas sponsorship). Note that funding
    /// an account switches its fee payment to the tank for as long as the
    /// credit covers each transaction's fee prepayment; the recipient can
    /// always withdraw the credit.
    function fundFor(address _account, uint256 _amount) external nonReentrantTokenCall {
        if (_account == address(0)) {
            revert ZeroAddress();
        }
        _fundFor(_account, _amount);
    }

    /// @notice Withdraw prepaid gas credit back to zkSYS.
    function withdraw(uint256 _amount) external nonReentrantTokenCall {
        if (_amount == 0) {
            revert ZeroAmount();
        }
        uint256 credit = _credit[msg.sender];
        if (credit < _amount) {
            revert InsufficientCredit(credit, _amount);
        }
        uint256 total = _totalCredits;
        if (total < _amount) {
            // SYSCOIN: direct bootloader writes make this invariant
            // consensus-sensitive. Fail closed instead of wrapping corrupted
            // totalCredits to a near-uint256-max value.
            revert TotalCreditsUnderflow(total, _amount);
        }
        uint256 tankBalanceBefore = token.balanceOf(address(this));
        if (tankBalanceBefore < total) {
            // SYSCOIN: never let one withdrawal consume backing belonging to
            // another account if a token upgrade or corrupted STF write has
            // already made the tank insolvent.
            revert InsufficientBacking(tankBalanceBefore, total);
        }
        uint256 accountBalanceBefore = token.balanceOf(msg.sender);
        unchecked {
            _credit[msg.sender] = credit - _amount;
            _totalCredits = total - _amount;
        }
        if (!token.transfer(msg.sender, _amount)) {
            revert TransferFailed();
        }
        // SYSCOIN: an upgradeable token must debit exactly the requested tank
        // amount and deliver all of it. Fee-on-transfer, sender-side fees, or
        // callback balance mutation reverts both token and ledger atomically.
        uint256 tankBalanceAfter = token.balanceOf(address(this));
        uint256 expectedTankBalance;
        unchecked {
            // Proven safe by tankBalanceBefore >= total >= _amount above.
            expectedTankBalance = tankBalanceBefore - _amount;
        }
        if (tankBalanceAfter != expectedTankBalance) {
            revert TokenBalanceMismatch(expectedTankBalance, tankBalanceAfter);
        }
        uint256 expectedAccountBalance = accountBalanceBefore + _amount;
        uint256 accountBalanceAfter = token.balanceOf(msg.sender);
        if (accountBalanceAfter != expectedAccountBalance) {
            revert TokenBalanceMismatch(expectedAccountBalance, accountBalanceAfter);
        }
        emit Withdrawn(msg.sender, _amount);
    }

    /// @notice Burn the zkSYS backing already-burned base fees. Callable by
    /// anyone; requires this contract to hold BURNER_ROLE on the token.
    function burnSurplus() external nonReentrantTokenCall returns (uint256 amount) {
        uint256 balance = token.balanceOf(address(this));
        uint256 outstanding = _totalCredits;
        if (balance <= outstanding) {
            revert NoSurplus();
        }
        unchecked {
            amount = balance - outstanding;
        }
        if (!token.burn(address(this), amount)) {
            revert TransferFailed();
        }
        // SYSCOIN: reject over-debiting / partial-burn token upgrades. The
        // exact post-burn balance is all outstanding user credit, no less.
        uint256 balanceAfter = token.balanceOf(address(this));
        if (balanceAfter != outstanding) {
            revert TokenBalanceMismatch(outstanding, balanceAfter);
        }
        emit SurplusBurned(msg.sender, amount);
    }

    function _fundFor(address _account, uint256 _amount) internal {
        if (_amount == 0) {
            revert ZeroAmount();
        }

        // SYSCOIN: the immutable token is currently an exact-transfer ERC-20
        // proxy, but future compatible implementations must preserve that
        // property. Pull and verify the full backing before publishing credit,
        // so fee-on-transfer behavior cannot undercollateralize other users and
        // a token callback cannot observe newly granted credit.
        uint256 balanceBefore = token.balanceOf(address(this));
        uint256 outstanding = _totalCredits;
        if (balanceBefore < outstanding) {
            // SYSCOIN: an exact inbound delta must not merely preserve a
            // pre-existing deficit and expose the new funder to old losses.
            // A direct token donation can restore backing before funding.
            revert InsufficientBacking(balanceBefore, outstanding);
        }
        uint256 funderBalanceBefore = token.balanceOf(msg.sender);
        if (funderBalanceBefore < _amount) {
            revert InsufficientFundingBalance(funderBalanceBefore, _amount);
        }
        uint256 expectedBalance = balanceBefore + _amount;
        if (!token.transferFrom(msg.sender, address(this), _amount)) {
            revert TransferFailed();
        }
        uint256 balanceAfter = token.balanceOf(address(this));
        if (balanceAfter != expectedBalance) {
            revert TokenBalanceMismatch(expectedBalance, balanceAfter);
        }
        uint256 expectedFunderBalance;
        unchecked {
            // Proven safe by funderBalanceBefore >= _amount above.
            expectedFunderBalance = funderBalanceBefore - _amount;
        }
        uint256 funderBalanceAfter = token.balanceOf(msg.sender);
        if (funderBalanceAfter != expectedFunderBalance) {
            revert TokenBalanceMismatch(expectedFunderBalance, funderBalanceAfter);
        }

        _credit[_account] += _amount;
        _totalCredits += _amount;
        emit Funded(msg.sender, _account, _amount);
    }
}
