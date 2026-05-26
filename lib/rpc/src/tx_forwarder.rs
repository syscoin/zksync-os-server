use alloy::primitives::{B256, Bytes, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::transports::{RpcError, TransportErrorKind};
use std::collections::HashMap;
use tokio::sync::watch;
use zksync_os_raft::RaftConsensusStatus;
use zksync_os_rpc_api::types::ZkTransactionReceipt;

/// ENs use `main_node_rpc_url`; with consensus they may hit any node, which forwards to leader.
#[derive(Clone)]
pub struct TxForwarder {
    target: TxForwardTarget,
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
    pub fn new(rpc_url: String, provider: DynProvider) -> Self {
        Self { rpc_url, provider }
    }
}

impl TxForwarder {
    pub fn static_target(endpoint: TxForwardEndpoint) -> Self {
        Self {
            target: TxForwardTarget::StaticTarget(endpoint),
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
    ) -> Result<Option<ZkTransactionReceipt>, TxForwardError> {
        self.forward(
            tx_hash,
            tx_bytes,
            TxForwardCall::SendRawTransactionSync { max_wait_ms },
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
                Self::forward_to_endpoint(call, tx_hash, tx_bytes, None, endpoint).await
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

                Self::forward_to_endpoint(call, tx_hash, tx_bytes, Some(&leader), endpoint).await
            }
        }
    }

    async fn forward_to_endpoint(
        call: TxForwardCall,
        tx_hash: B256,
        tx_bytes: &Bytes,
        leader: Option<&str>,
        endpoint: &TxForwardEndpoint,
    ) -> Result<Option<ZkTransactionReceipt>, TxForwardError> {
        Self::log_forwarding(call, tx_hash, leader, endpoint);

        match call {
            TxForwardCall::SendRawTransaction => {
                let _ = endpoint.provider.send_raw_transaction(tx_bytes).await?;
                Ok(None)
            }
            TxForwardCall::SendRawTransactionSync { max_wait_ms } => Ok(Some(
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
    SendRawTransactionSync { max_wait_ms: Option<U256> },
}

#[derive(Debug, thiserror::Error)]
pub enum TxForwardError {
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
    use super::redacted_rpc_url;

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
