pub struct TxValidatorConfig {
    /// Max input size of a transaction to be accepted by mempool
    pub max_input_bytes: usize,
    /// SYSCOIN: Maximum total transaction fee accepted by the reth mempool validator.
    /// Set to 0 to disable reth's fee-cap check for chains with a cheap base token.
    pub tx_fee_cap: u128,
}
