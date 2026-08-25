mod bounded_http;
mod latency;
mod metrics;
mod retry;
mod timeout;

use crate::config::ProviderConfig;
use alloy::network::EthereumWallet;
use alloy::providers::ProviderBuilder;
use alloy::rpc::client::RpcClient;
use alloy::signers::local::PrivateKeySigner;
pub(crate) use bounded_http::ProviderResponseByteBudget;
use std::time::Duration;
use tower::ServiceBuilder;
use vise::{EncodeLabelSet, EncodeLabelValue};
use zksync_os_provider::NodeProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EncodeLabelValue, EncodeLabelSet)]
#[metrics(label = "provider", rename_all = "snake_case")]
pub(crate) enum ProviderKind {
    L1,
    L1Archive,
    Gateway,
}

pub(crate) async fn build_node_provider(
    config: &ProviderConfig,
    latest_poll_interval: Duration,
    finalized_poll_interval: Duration,
    log_cache_capacity: usize,
    provider: ProviderKind,
    response_byte_budget: &ProviderResponseByteBudget,
) -> NodeProvider {
    let max_retries = config.max_retries;
    let retry_backoff = config.retry_backoff;
    let request_timeout = config.request_timeout;
    // Timeout is the innermost layer so that each retry attempt gets its own timeout.
    let provider_layers = ServiceBuilder::new()
        .layer_fn(move |inner| latency::LatencyService { inner, provider })
        .layer_fn(move |inner| retry::RetryService {
            inner,
            provider,
            max_retries,
            backoff: retry_backoff,
        })
        .layer_fn(move |inner| timeout::TimeoutService {
            inner,
            timeout: request_timeout,
        });

    // SYSCOIN: Alloy's stock reqwest transport calls `Response::bytes()` before JSON decoding.
    // Stream through our hard cap so a Byzantine or faulty L1/Gateway RPC cannot allocate an
    // unbounded body before the proof/event-level validators get control.
    let transport = bounded_http::BoundedHttpTransport::new(
        config
            .rpc_url
            .parse()
            .expect("L1/Gateway provider URL is invalid"),
        // SYSCOIN: All provider instances and their Alloy clones share one process-wide weighted
        // response-byte budget instead of multiplying the cap per L1/archive/Gateway transport.
        response_byte_budget.clone(),
    )
    .expect("L1/Gateway provider URL must use HTTP or HTTPS");
    let is_local = transport.is_local();
    let client = RpcClient::builder()
        .layer(provider_layers)
        .transport(transport, is_local)
        .with_poll_interval(config.rpc_poll_interval);
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(PrivateKeySigner::random()))
        .connect_client(client);
    NodeProvider::new_with_features(
        provider,
        latest_poll_interval,
        finalized_poll_interval,
        log_cache_capacity,
    )
    .await
    .expect("failed to initialize node provider features")
}
