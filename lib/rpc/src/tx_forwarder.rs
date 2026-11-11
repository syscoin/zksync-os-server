use alloy::primitives::{B256, Bytes, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::client::BuiltInConnectionString;
use alloy::transports::http::reqwest;
use alloy::transports::{RpcError, TransportErrorKind};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch;
use zksync_os_raft::RaftConsensusStatus;
use zksync_os_rpc_api::types::ZkTransactionReceipt;

const FORWARDING_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum caller-provided receipt wait, shared with the forwarding deadline calculation.
pub(crate) const SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT: Duration = Duration::from_secs(30);

/// ENs use `main_node_rpc_url`; with consensus they may hit any node, which forwards to leader.
#[derive(Clone)]
pub struct TxForwarder {
    target: TxForwardTarget,
    transport_timeout: Duration,
}

#[derive(Clone)]
enum TxForwardTarget {
    /// Used on ENs: forwards to `main_node_rpc_url`.
    StaticTarget(TxForwardEndpoint),
    /// Used on consensus nodes: forwards to the current leader from Raft status.
    ConsensusLeader {
        node_id: String,
        status_rx: watch::Receiver<Option<RaftConsensusStatus>>,
        providers: HashMap<String, TxForwardEndpoint>,
    },
}

#[derive(Clone)]
pub struct TxForwardEndpoint {
    rpc_url: String,
    provider: DynProvider,
}

impl TxForwardEndpoint {
    pub async fn connect(rpc_url: &str) -> Result<Self, alloy::transports::TransportError> {
        let provider = match rpc_url.parse::<BuiltInConnectionString>()? {
            BuiltInConnectionString::Http(url) => {
                // A failed submission may already have reached the leader. Never replay it.
                let client = reqwest::Client::builder()
                    .retry(reqwest::retry::never())
                    .build()
                    .map_err(TransportErrorKind::custom)?;
                ProviderBuilder::new().connect_reqwest(client, url).erased()
            }
            _ => ProviderBuilder::new().connect(rpc_url).await?.erased(),
        };
        Ok(Self::new(rpc_url.to_owned(), provider))
    }

    pub fn new(rpc_url: String, provider: DynProvider) -> Self {
        Self { rpc_url, provider }
    }
}

impl TxForwarder {
    pub fn static_target(endpoint: TxForwardEndpoint) -> Self {
        Self {
            target: TxForwardTarget::StaticTarget(endpoint),
            transport_timeout: FORWARDING_TRANSPORT_TIMEOUT,
        }
    }

    pub fn consensus_leader(
        node_id: String,
        status_rx: watch::Receiver<Option<RaftConsensusStatus>>,
        providers: HashMap<String, TxForwardEndpoint>,
    ) -> Self {
        Self {
            target: TxForwardTarget::ConsensusLeader {
                node_id,
                status_rx,
                providers,
            },
            transport_timeout: FORWARDING_TRANSPORT_TIMEOUT,
        }
    }

    pub(crate) async fn forward_raw_transaction(
        &self,
        tx_hash: B256,
        tx_bytes: &Bytes,
    ) -> Result<(), TxForwardError> {
        self.forward(tx_hash, tx_bytes, TxForwardCall::SendRawTransaction)
            .await
            .map(|_| ())
    }

    pub(crate) async fn forward_raw_transaction_sync(
        &self,
        tx_hash: B256,
        tx_bytes: &Bytes,
        max_wait_ms: Option<U256>,
        default_wait: Duration,
    ) -> Result<Option<ZkTransactionReceipt>, TxForwardError> {
        self.forward(
            tx_hash,
            tx_bytes,
            TxForwardCall::SendRawTransactionSync {
                max_wait_ms,
                default_wait,
            },
        )
        .await
    }

    async fn forward(
        &self,
        tx_hash: B256,
        tx_bytes: &Bytes,
        call: TxForwardCall,
    ) -> Result<Option<ZkTransactionReceipt>, TxForwardError> {
        match &self.target {
            TxForwardTarget::StaticTarget(endpoint) => {
                self.forward_to_endpoint(call, tx_hash, tx_bytes, None, endpoint)
                    .await
            }
            TxForwardTarget::ConsensusLeader {
                node_id,
                status_rx,
                providers,
            } => {
                let status = status_rx.borrow().clone();
                if status.as_ref().is_some_and(|status| status.is_leader) {
                    Self::log_not_forwarding(call, tx_hash);
                    return Ok(None);
                }

                let leader = status
                    .and_then(|status| status.current_leader)
                    .ok_or(TxForwardError::NoKnownLeader)?;
                if leader == *node_id {
                    return Err(TxForwardError::NoKnownLeader);
                }
                let endpoint = providers
                    .get(&leader)
                    .ok_or_else(|| TxForwardError::NoProvider(leader.clone()))?;

                self.forward_to_endpoint(call, tx_hash, tx_bytes, Some(&leader), endpoint)
                    .await
            }
        }
    }

    async fn forward_to_endpoint(
        &self,
        call: TxForwardCall,
        tx_hash: B256,
        tx_bytes: &Bytes,
        leader: Option<&str>,
        endpoint: &TxForwardEndpoint,
    ) -> Result<Option<ZkTransactionReceipt>, TxForwardError> {
        Self::log_forwarding(call, tx_hash, leader, endpoint);

        let timeout = call.timeout(self.transport_timeout);
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or(TxForwardError::Timeout { tx_hash })?;
        let result: Result<Option<ZkTransactionReceipt>, TxForwardError> =
            tokio::time::timeout_at(deadline, async {
                match call {
                    TxForwardCall::SendRawTransaction => {
                        let _ = endpoint.provider.send_raw_transaction(tx_bytes).await?;
                        Ok(None)
                    }
                    TxForwardCall::SendRawTransactionSync { max_wait_ms, .. } => Ok(Some(
                        endpoint
                            .provider
                            // SYSCOIN: preserve caller-provided EIP-7966 timeout when forwarding sync sends.
                            .raw_request(
                                "eth_sendRawTransactionSync".into(),
                                (tx_bytes.clone(), max_wait_ms),
                            )
                            .await?,
                    )),
                }
            })
            .await
            .map_err(|_| TxForwardError::Timeout { tx_hash })?;
        result.map_err(|err| {
            if let TxForwardError::Rpc(RpcError::Transport(TransportErrorKind::Custom(source))) =
                &err
                && source
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
            {
                TxForwardError::Timeout { tx_hash }
            } else {
                err
            }
        })
    }

    fn log_not_forwarding(call: TxForwardCall, tx_hash: B256) {
        match call {
            TxForwardCall::SendRawTransaction => {
                tracing::debug!(%tx_hash, "not forwarding transaction: node is leader");
            }
            TxForwardCall::SendRawTransactionSync { .. } => {
                tracing::debug!(%tx_hash, "not forwarding sync transaction: node is leader");
            }
        }
    }

    fn log_forwarding(
        call: TxForwardCall,
        tx_hash: B256,
        leader: Option<&str>,
        endpoint: &TxForwardEndpoint,
    ) {
        // SYSCOIN: avoid leaking RPC credentials or signed query params into logs.
        let rpc_url = redacted_rpc_url(&endpoint.rpc_url);
        match (call, leader) {
            (TxForwardCall::SendRawTransaction, Some(leader)) => {
                tracing::debug!(
                    %tx_hash,
                    leader = %leader,
                    rpc_url = %rpc_url,
                    "forwarding transaction to consensus leader"
                );
            }
            (TxForwardCall::SendRawTransaction, None) => {
                tracing::debug!(%tx_hash, rpc_url = %rpc_url, "forwarding transaction");
            }
            (TxForwardCall::SendRawTransactionSync { .. }, Some(leader)) => {
                tracing::debug!(
                    %tx_hash,
                    leader = %leader,
                    rpc_url = %rpc_url,
                    "forwarding sync transaction to consensus leader"
                );
            }
            (TxForwardCall::SendRawTransactionSync { .. }, None) => {
                tracing::debug!(%tx_hash, rpc_url = %rpc_url, "forwarding sync transaction");
            }
        }
    }
}

// SYSCOIN: keep endpoint logs useful without exposing credentials or request tokens.
fn redacted_rpc_url(rpc_url: &str) -> String {
    let mut end = rpc_url.len();
    for delimiter in ['?', '#'] {
        if let Some(index) = rpc_url.find(delimiter) {
            end = end.min(index);
        }
    }
    let rpc_url = &rpc_url[..end];

    let Some((scheme, rest)) = rpc_url.split_once("://") else {
        return rpc_url.to_owned();
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);

    format!("{scheme}://{authority}{path}")
}

#[derive(Clone, Copy)]
enum TxForwardCall {
    SendRawTransaction,
    SendRawTransactionSync {
        max_wait_ms: Option<U256>,
        default_wait: Duration,
    },
}

impl TxForwardCall {
    fn timeout(self, transport_margin: Duration) -> Duration {
        let wait = match self {
            Self::SendRawTransaction => Duration::ZERO,
            Self::SendRawTransactionSync {
                max_wait_ms,
                default_wait,
            } => {
                // Preserve the original wire argument. For omitted/invalid values the remote
                // default can differ, so allow the full supported sync wait, or our larger default.
                max_wait_ms
                    .and_then(|ms| u64::try_from(ms).ok())
                    .map(Duration::from_millis)
                    .filter(|wait| *wait <= SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT)
                    .unwrap_or(default_wait.max(SEND_RAW_TRANSACTION_SYNC_MAX_TIMEOUT))
            }
        };
        wait.saturating_add(transport_margin)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TxForwardError {
    #[error(
        "transaction forwarding timed out; upstream outcome is unknown: transaction {tx_hash} may still be included; check its status by hash before submitting again"
    )]
    Timeout { tx_hash: B256 },
    #[error("consensus leader is unknown")]
    NoKnownLeader,
    #[error("no RPC forwarder is configured for consensus leader {0}")]
    NoProvider(String),
    #[error(transparent)]
    Rpc(#[from] RpcError<TransportErrorKind>),
}

impl TxForwardError {
    pub(crate) fn as_rpc_error(&self) -> Option<&RpcError<TransportErrorKind>> {
        match self {
            Self::Rpc(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    // Capture requests without needing a chain, and keep stalled connections open until
    // the client cancels them. The task owns its connection tasks for deterministic cleanup.
    async fn upstream(
        partial_body: bool,
        response: Option<(Duration, serde_json::Value)>,
    ) -> (
        TxForwardEndpoint,
        mpsc::UnboundedReceiver<serde_json::Value>,
        mpsc::UnboundedReceiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = mpsc::unbounded_channel();
        let (closed, disconnections) = mpsc::unbounded_channel();
        let task =
            tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let sent = sent.clone();
                    let closed = closed.clone();
                    let response = response.clone();
                    connections.spawn(async move {
                    let mut bytes = Vec::new();
                    let request = loop {
                        let mut chunk = [0; 4096];
                        let n = socket.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|value| value.trim().parse().unwrap())
                                })
                                .unwrap();
                            if bytes.len() >= end + 4 + length {
                                break serde_json::from_slice::<serde_json::Value>(
                                    &bytes[end + 4..end + 4 + length],
                                )
                                .unwrap();
                            }
                        }
                    };
                    sent.send(request.clone()).unwrap();
                    if let Some((delay, result)) = response {
                        tokio::time::sleep(delay).await;
                        let mut reply = serde_json::json!({"jsonrpc":"2.0", "id":request["id"]});
                        let key = if result.is_string() { "result" } else { "error" };
                        reply[key] = result;
                        let body = reply.to_string();
                        let reply = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                    } else {
                        if partial_body {
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{")
                                .await
                                .unwrap();
                        }
                        let mut byte = [0];
                        if matches!(socket.read(&mut byte).await, Ok(0)) {
                            let _ = closed.send(());
                        }
                    }
                });
                }
            });
        (
            TxForwardEndpoint::connect(&url).await.unwrap(),
            received,
            disconnections,
            task,
        )
    }

    #[test]
    fn forwarding_budgets_preserve_sync_wait_and_bound_invalid_inputs() {
        let margin = Duration::from_secs(5);
        assert_eq!(TxForwardCall::SendRawTransaction.timeout(margin), margin);
        for (input, expected) in [
            (Some(U256::ZERO), 5),
            (Some(U256::from(30000)), 35),
            (None, 35),
            (Some(U256::from(30001)), 35),
            (Some(U256::MAX), 35),
        ] {
            let call = TxForwardCall::SendRawTransactionSync {
                max_wait_ms: input,
                default_wait: Duration::from_secs(2),
            };
            assert_eq!(call.timeout(margin), Duration::from_secs(expected));
        }
        assert_eq!(
            TxForwardCall::SendRawTransactionSync {
                max_wait_ms: None,
                default_wait: Duration::from_secs(60)
            }
            .timeout(margin),
            Duration::from_secs(65)
        );
    }

    #[tokio::test]
    async fn stalled_headers_and_body_time_out_for_both_methods_without_retry() {
        for sync in [false, true] {
            for partial in [false, true] {
                let (endpoint, mut requests, _disconnections, task) = upstream(partial, None).await;
                let mut forwarder = TxForwarder::static_target(endpoint);
                forwarder.transport_timeout = Duration::from_millis(300);
                let hash = B256::repeat_byte(1);
                let result = tokio::time::timeout(Duration::from_secs(2), async {
                    if sync {
                        forwarder
                            .forward_raw_transaction_sync(
                                hash,
                                &Bytes::new(),
                                Some(U256::ZERO),
                                Duration::ZERO,
                            )
                            .await
                            .map(|_| ())
                    } else {
                        forwarder.forward_raw_transaction(hash, &Bytes::new()).await
                    }
                })
                .await
                .expect("forwarding remained unbounded");
                assert!(
                    matches!(result, Err(TxForwardError::Timeout { tx_hash }) if tx_hash == hash)
                );
                let request = requests.recv().await.unwrap();
                assert_eq!(
                    request["method"],
                    if sync {
                        "eth_sendRawTransactionSync"
                    } else {
                        "eth_sendRawTransaction"
                    }
                );
                if sync {
                    assert_eq!(request["params"][1], "0x0");
                }
                assert!(requests.try_recv().is_err());
                task.abort();
            }
        }
    }

    #[tokio::test]
    async fn earlier_http_transport_timeout_is_also_an_unknown_outcome() {
        let (endpoint, mut requests, _disconnections, task) = upstream(false, None).await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let provider = ProviderBuilder::new()
            .connect_reqwest(client, endpoint.rpc_url.parse().unwrap())
            .erased();
        let forwarder =
            TxForwarder::static_target(TxForwardEndpoint::new(endpoint.rpc_url, provider));
        assert!(matches!(
            forwarder
                .forward_raw_transaction(B256::ZERO, &Bytes::new())
                .await,
            Err(TxForwardError::Timeout { .. })
        ));
        requests.recv().await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn sync_budget_allows_upstream_wait_and_preserves_remote_timeout() {
        let remote_error =
            serde_json::json!({"code":4,"message":"upstream wait expired","data":{"test":true}});
        let (endpoint, mut requests, _disconnections, task) =
            upstream(false, Some((Duration::from_millis(150), remote_error))).await;
        let mut forwarder = TxForwarder::static_target(endpoint);
        forwarder.transport_timeout = Duration::from_millis(100);
        let err = forwarder
            .forward_raw_transaction_sync(
                B256::ZERO,
                &Bytes::new(),
                Some(U256::from(500)),
                Duration::from_millis(500),
            )
            .await
            .unwrap_err();
        let rpc = err.as_rpc_error().unwrap().as_error_resp().unwrap();
        assert_eq!(rpc.code, 4);
        assert_eq!(rpc.data.as_ref().unwrap().get(), r#"{"test":true}"#);
        assert_eq!(requests.recv().await.unwrap()["params"][1], "0x1f4");
        assert!(requests.try_recv().is_err());
        task.abort();
    }

    #[tokio::test]
    async fn consensus_leader_bypasses_forwarding_but_follower_is_bounded() {
        let (endpoint, mut requests, _disconnections, task) = upstream(false, None).await;
        let mut status = RaftConsensusStatus {
            node_id: "local".into(),
            state: "leader".into(),
            is_leader: true,
            current_leader: Some("local".into()),
            current_term: 1,
            last_applied_index: None,
        };
        let (sender, receiver) = watch::channel(Some(status.clone()));
        let mut forwarder = TxForwarder::consensus_leader(
            "local".into(),
            receiver,
            HashMap::from([("remote".into(), endpoint)]),
        );
        forwarder.transport_timeout = Duration::from_millis(300);
        forwarder
            .forward_raw_transaction(B256::ZERO, &Bytes::new())
            .await
            .unwrap();
        assert!(requests.try_recv().is_err());
        status.is_leader = false;
        status.current_leader = Some("remote".into());
        sender.send(Some(status)).unwrap();
        assert!(matches!(
            forwarder
                .forward_raw_transaction(B256::ZERO, &Bytes::new())
                .await,
            Err(TxForwardError::Timeout { .. })
        ));
        assert_eq!(
            requests.recv().await.unwrap()["method"],
            "eth_sendRawTransaction"
        );
        task.abort();
    }

    #[tokio::test]
    async fn successful_async_forward_returns_without_waiting_for_a_receipt() {
        let hash = B256::repeat_byte(3);
        let (endpoint, mut requests, _disconnections, task) =
            upstream(false, Some((Duration::ZERO, serde_json::json!(hash)))).await;
        let forwarder = TxForwarder::static_target(endpoint);
        forwarder
            .forward_raw_transaction(hash, &Bytes::new())
            .await
            .unwrap();
        assert_eq!(
            requests.recv().await.unwrap()["method"],
            "eth_sendRawTransaction"
        );
        assert!(requests.try_recv().is_err());
        task.abort();
    }

    #[tokio::test]
    async fn caller_cancellation_drops_the_forwarding_future() {
        let (endpoint, mut requests, mut disconnections, task) = upstream(false, None).await;
        let forwarder = TxForwarder::static_target(endpoint);
        // Retain the provider pool: cancellation must close this request, not just the client.
        let request_forwarder = forwarder.clone();
        let call = tokio::spawn(async move {
            request_forwarder
                .forward_raw_transaction(B256::ZERO, &Bytes::new())
                .await
        });
        requests.recv().await.unwrap();
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), disconnections.recv())
                .await
                .expect("cancelled request kept the upstream connection open"),
            Some(())
        );
        assert!(requests.try_recv().is_err());
        task.abort();
    }

    #[test]
    fn redacted_rpc_url_removes_credentials_query_and_fragment() {
        assert_eq!(
            redacted_rpc_url("https://user:pass@example.com:8545/rpc?token=secret#frag"),
            "https://example.com:8545/rpc"
        );
    }

    #[test]
    fn redacted_rpc_url_preserves_plain_endpoint() {
        assert_eq!(
            redacted_rpc_url("http://127.0.0.1:3050"),
            "http://127.0.0.1:3050"
        );
    }
}
