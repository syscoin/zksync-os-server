//! Gas adjuster metrics.
//!
//! SYSCOIN: Canonical DA pricing is measured per published Bitcoin-DA byte rather than per
//! Ethereum blob gas unit.

use vise::{Gauge, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "server_gas_adjuster")]
pub(super) struct GasAdjusterMetrics {
    pub current_base_fee_per_gas: Gauge<u64>,
    pub current_da_fee_per_byte: Gauge<u64>,
    pub current_pubdata_price_per_byte: Gauge<u64>,
    pub median_base_fee_per_gas: Gauge<u64>,
    pub median_da_fee_per_byte: Gauge<u64>,
    pub median_pubdata_price_per_byte: Gauge<u64>,
}

#[vise::register]
pub(super) static METRICS: vise::Global<GasAdjusterMetrics> = vise::Global::new();
