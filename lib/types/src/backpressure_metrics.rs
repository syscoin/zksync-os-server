use vise::{Counter, Gauge, LabeledFamily, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "backpressure")]
pub struct BackpressureMetrics {
    /// 1 when this backpressure cause is currently active, 0 when clear.
    /// Grafana alert: backpressure_active{cause="prover_queue_full"} == 1 for > 5m
    #[metrics(labels = ["cause"])]
    pub active: LabeledFamily<&'static str, Gauge<u64>>,

    /// Total eth_sendRawTransaction calls rejected due to backpressure, labeled by cause.
    /// Grafana: rate(backpressure_tx_rejected_total[5m]) for rejections/sec per subsystem.
    #[metrics(labels = ["cause"])]
    pub tx_rejected_total: LabeledFamily<&'static str, Counter>,
}

#[vise::register]
pub static BACKPRESSURE_METRICS: vise::Global<BackpressureMetrics> = vise::Global::new();
