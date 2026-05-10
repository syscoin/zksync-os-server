use crate::metrics::{API_METRICS, RPC_TASK_MONITOR};
use crate::result::internal_rpc_err;
use futures::FutureExt as _;
use jsonrpsee::core::middleware::{Batch, BatchEntry, Notification};
use jsonrpsee::server::middleware::rpc::{RpcService, RpcServiceT};
use jsonrpsee::types::Request;
use jsonrpsee::{BatchResponseBuilder, MethodResponse};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const UNKNOWN_METHOD_LABEL: &str = "unknown";

// SYSCOIN: method names are client-controlled JSON-RPC input. Keep metric cardinality bounded
// by mapping them to a static allowlist before indexing global metric families.
fn normalize_method_label(method: &str) -> &'static str {
    match method {
        "batch" => "batch",
        "debug_chainConfig" => "debug_chainConfig",
        "debug_codeByHash" => "debug_codeByHash",
        "debug_getRawBlock" => "debug_getRawBlock",
        "debug_getRawHeader" => "debug_getRawHeader",
        "debug_getRawReceipts" => "debug_getRawReceipts",
        "debug_getRawTransaction" => "debug_getRawTransaction",
        "debug_getRawTransactions" => "debug_getRawTransactions",
        "debug_traceBlock" => "debug_traceBlock",
        "debug_traceBlockByHash" => "debug_traceBlockByHash",
        "debug_traceBlockByNumber" => "debug_traceBlockByNumber",
        "debug_traceCall" => "debug_traceCall",
        "debug_traceCallMany" => "debug_traceCallMany",
        "debug_traceTransaction" => "debug_traceTransaction",
        "erigon_getHeaderByNumber" => "erigon_getHeaderByNumber",
        "eth_accounts" => "eth_accounts",
        "eth_blobBaseFee" => "eth_blobBaseFee",
        "eth_blockNumber" => "eth_blockNumber",
        "eth_call" => "eth_call",
        "eth_callMany" => "eth_callMany",
        "eth_chainId" => "eth_chainId",
        "eth_coinbase" => "eth_coinbase",
        "eth_createAccessList" => "eth_createAccessList",
        "eth_estimateGas" => "eth_estimateGas",
        "eth_feeHistory" => "eth_feeHistory",
        "eth_gasPrice" => "eth_gasPrice",
        "eth_getAccount" => "eth_getAccount",
        "eth_getAccountInfo" => "eth_getAccountInfo",
        "eth_getBalance" => "eth_getBalance",
        "eth_getBlockByHash" => "eth_getBlockByHash",
        "eth_getBlockByNumber" => "eth_getBlockByNumber",
        "eth_getBlockReceipts" => "eth_getBlockReceipts",
        "eth_getBlockTransactionCountByHash" => "eth_getBlockTransactionCountByHash",
        "eth_getBlockTransactionCountByNumber" => "eth_getBlockTransactionCountByNumber",
        "eth_getCode" => "eth_getCode",
        "eth_getFilterChanges" => "eth_getFilterChanges",
        "eth_getFilterLogs" => "eth_getFilterLogs",
        "eth_getHeaderByHash" => "eth_getHeaderByHash",
        "eth_getHeaderByNumber" => "eth_getHeaderByNumber",
        "eth_getLogs" => "eth_getLogs",
        "eth_getProof" => "eth_getProof",
        "eth_getRawTransactionByBlockHashAndIndex" => "eth_getRawTransactionByBlockHashAndIndex",
        "eth_getRawTransactionByBlockNumberAndIndex" => {
            "eth_getRawTransactionByBlockNumberAndIndex"
        }
        "eth_getRawTransactionByHash" => "eth_getRawTransactionByHash",
        "eth_getStorageAt" => "eth_getStorageAt",
        "eth_getTransactionByBlockHashAndIndex" => "eth_getTransactionByBlockHashAndIndex",
        "eth_getTransactionByBlockNumberAndIndex" => "eth_getTransactionByBlockNumberAndIndex",
        "eth_getTransactionByHash" => "eth_getTransactionByHash",
        "eth_getTransactionBySenderAndNonce" => "eth_getTransactionBySenderAndNonce",
        "eth_getTransactionCount" => "eth_getTransactionCount",
        "eth_getTransactionReceipt" => "eth_getTransactionReceipt",
        "eth_getUncleByBlockHashAndIndex" => "eth_getUncleByBlockHashAndIndex",
        "eth_getUncleByBlockNumberAndIndex" => "eth_getUncleByBlockNumberAndIndex",
        "eth_getUncleCountByBlockHash" => "eth_getUncleCountByBlockHash",
        "eth_getUncleCountByBlockNumber" => "eth_getUncleCountByBlockNumber",
        "eth_maxPriorityFeePerGas" => "eth_maxPriorityFeePerGas",
        "eth_newBlockFilter" => "eth_newBlockFilter",
        "eth_newFilter" => "eth_newFilter",
        "eth_newPendingTransactionFilter" => "eth_newPendingTransactionFilter",
        "eth_protocolVersion" => "eth_protocolVersion",
        "eth_sendRawTransaction" => "eth_sendRawTransaction",
        "eth_sendRawTransactionSync" => "eth_sendRawTransactionSync",
        "eth_sendTransaction" => "eth_sendTransaction",
        "eth_sign" => "eth_sign",
        "eth_signTransaction" => "eth_signTransaction",
        "eth_signTypedData" => "eth_signTypedData",
        "eth_simulateV1" => "eth_simulateV1",
        "eth_subscribe" => "eth_subscribe",
        "eth_syncing" => "eth_syncing",
        "eth_uninstallFilter" => "eth_uninstallFilter",
        "eth_unsubscribe" => "eth_unsubscribe",
        "net_version" => "net_version",
        "ots_getApiLevel" => "ots_getApiLevel",
        "ots_getBlockDetails" => "ots_getBlockDetails",
        "ots_getBlockDetailsByHash" => "ots_getBlockDetailsByHash",
        "ots_getBlockTransactions" => "ots_getBlockTransactions",
        "ots_getContractCreator" => "ots_getContractCreator",
        "ots_getHeaderByNumber" => "ots_getHeaderByNumber",
        "ots_getInternalOperations" => "ots_getInternalOperations",
        "ots_getTransactionBySenderAndNonce" => "ots_getTransactionBySenderAndNonce",
        "ots_getTransactionError" => "ots_getTransactionError",
        "ots_hasCode" => "ots_hasCode",
        "ots_searchTransactionsAfter" => "ots_searchTransactionsAfter",
        "ots_searchTransactionsBefore" => "ots_searchTransactionsBefore",
        "ots_traceTransaction" => "ots_traceTransaction",
        "txpool_content" => "txpool_content",
        "txpool_inspect" => "txpool_inspect",
        "txpool_status" => "txpool_status",
        "unstable_getBatchByBlockNumber" => "unstable_getBatchByBlockNumber",
        "unstable_getLocalRoot" => "unstable_getLocalRoot",
        "web3_clientVersion" => "web3_clientVersion",
        "web3_sha3" => "web3_sha3",
        "zks_getBlockMetadataByNumber" => "zks_getBlockMetadataByNumber",
        "zks_getBridgehubContract" => "zks_getBridgehubContract",
        "zks_getBytecodeSupplierContract" => "zks_getBytecodeSupplierContract",
        "zks_getGenesis" => "zks_getGenesis",
        "zks_getL2ToL1LogProof" => "zks_getL2ToL1LogProof",
        "zks_getProof" => "zks_getProof",
        _ => UNKNOWN_METHOD_LABEL,
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CallKind {
    Call,
    Notification,
}

#[derive(Clone)]
pub struct Monitoring {
    inner: RpcService,
    max_response_size_bytes: usize,
    blocking_rpcs_semaphore: Arc<Semaphore>,
}

impl Monitoring {
    pub fn new(
        inner: RpcService,
        max_response_size_bytes: u32,
        max_concurrent_blocking_rpcs: u32,
    ) -> Self {
        Self {
            inner,
            max_response_size_bytes: max_response_size_bytes as usize,
            // SYSCOIN: keep at least one permit if the value is misconfigured to 0;
            // a zero-permit semaphore would make heavy RPCs wait forever.
            blocking_rpcs_semaphore: Arc::new(Semaphore::new(
                max_concurrent_blocking_rpcs.max(1) as usize
            )),
        }
    }
}

// SYSCOIN: jsonrpsee runs `blocking` methods on Tokio's blocking pool. Gate the
// expensive public methods before dispatch so connection count does not become
// an implicit heavy-work concurrency limit.
fn is_heavy_rpc_method(method: &str) -> bool {
    matches!(
        method,
        "debug_traceBlockByHash"
            | "debug_traceBlockByNumber"
            | "debug_traceCall"
            | "debug_traceTransaction"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_feeHistory"
            | "eth_getBlockReceipts"
            | "eth_getFilterChanges"
            | "eth_getFilterLogs"
            | "eth_getLogs"
            | "ots_getBlockDetails"
            | "ots_getBlockDetailsByHash"
            | "ots_getBlockTransactions"
            | "ots_searchTransactionsAfter"
            | "ots_searchTransactionsBefore"
            | "unstable_getLocalRoot"
            | "zks_getProof"
    )
}

/// Ensures latency is recorded even if the future is dropped mid-flight (client disconnected).
struct CallGuard {
    kind: CallKind,
    method: &'static str,
    started: Instant,
    request_size: usize,
    /// `Some((output_size, error_code))` once the future has resolved.
    completed: Option<(usize, Option<i32>)>,
    panicked: bool,
}

impl CallGuard {
    fn new(kind: CallKind, method: &'static str, request_size: usize) -> Self {
        Self {
            kind,
            method,
            started: Instant::now(),
            request_size,
            completed: None,
            panicked: false,
        }
    }

    async fn handle_result<F>(
        mut self,
        fut: F,
        on_panic: impl FnOnce() -> MethodResponse + Send,
    ) -> MethodResponse
    where
        F: Future<Output = MethodResponse> + Send,
    {
        let result = AssertUnwindSafe(fut).catch_unwind().await;
        self.panicked = result.is_err();
        let out = result.unwrap_or_else(|_| on_panic());
        self.completed = Some((out.as_json().get().len(), out.as_error_code()));
        out
    }
}

/// Ensures batch-level metrics are recorded even if the future is dropped mid-flight (client disconnected).
struct BatchGuard {
    batch_input_size: usize,
    request_counts: HashMap<&'static str, u64>,
    started: Instant,
    /// `Some(response_size)` once the batch has resolved.
    completed: Option<usize>,
}

impl BatchGuard {
    fn new(batch_input_size: usize, request_counts: HashMap<&'static str, u64>) -> Self {
        Self {
            batch_input_size,
            request_counts,
            started: Instant::now(),
            completed: None,
        }
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let cancelled = self.completed.is_none();
        let response_size = self.completed.take().unwrap_or(0);
        if cancelled {
            API_METRICS.cancelled["batch"].inc();
        }
        API_METRICS.response_time["batch"].observe(elapsed);
        API_METRICS.request_size["batch"].observe(self.batch_input_size);
        API_METRICS.response_size["batch"].observe(response_size);
        for (method, count) in &self.request_counts {
            API_METRICS.requests_in_batch_count[*method].observe(*count);
        }
        tracing::debug!(
            target: "rpc::monitoring::batch",
            cancelled,
            "rpc batch call completed cancelled={}", cancelled
        );
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let cancelled = self.completed.is_none();
        let (output_size, error_code) = self.completed.take().unwrap_or((0, None));
        API_METRICS.response_time[self.method].observe(elapsed);
        API_METRICS.request_size[self.method].observe(self.request_size);
        API_METRICS.response_size[self.method].observe(output_size);
        if let Some(code) = error_code {
            API_METRICS.errors[&(self.method.to_owned(), code)].inc();
        }
        if cancelled {
            API_METRICS.cancelled[self.method].inc();
        }
        if self.panicked {
            API_METRICS.panicked[self.method].inc();
            match self.kind {
                CallKind::Call => tracing::error!(method = %self.method, "RPC handler panicked"),
                CallKind::Notification => {
                    tracing::error!(method = %self.method, "Notification handler panicked")
                }
            }
        }

        macro_rules! log {
            ($target:literal) => {
                tracing::debug!(
                    target: $target,
                    kind = ?self.kind,
                    cancelled,
                    "rpc call completed kind={:?} cancelled={}", self.kind, cancelled
                )
            };
        }

        match self.method {
            "eth_call" => log!("rpc::monitoring::eth::call"),
            "eth_sendRawTransaction" => log!("rpc::monitoring::eth::sendRawTransaction"),
            "debug_traceTransaction" => log!("rpc::monitoring::debug::traceTransaction"),
            _ => log!("rpc::monitoring::call"),
        }
    }
}

impl RpcServiceT for Monitoring {
    type MethodResponse = <RpcService as RpcServiceT>::MethodResponse;
    type NotificationResponse = <RpcService as RpcServiceT>::NotificationResponse;
    type BatchResponse = <RpcService as RpcServiceT>::BatchResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = normalize_method_label(request.method_name());
        let request_size = request.params.as_ref().map_or(0, |p| p.get().len());
        let inner = self.inner.clone();
        let blocking_rpcs_semaphore = self.blocking_rpcs_semaphore.clone();

        async move {
            let id = request.id.clone().into_owned();
            let handler_error_id = id.clone();
            let handler = RPC_TASK_MONITOR.instrument(async move {
                let _permit: Option<OwnedSemaphorePermit> = if is_heavy_rpc_method(method) {
                    match blocking_rpcs_semaphore.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            return MethodResponse::error(
                                handler_error_id,
                                internal_rpc_err("Internal error"),
                            );
                        }
                    }
                } else {
                    None
                };
                inner.call(request).await
            });
            let on_panic = || MethodResponse::error(id, internal_rpc_err("Internal error"));
            CallGuard::new(CallKind::Call, method, request_size)
                .handle_result(handler, on_panic)
                .await
        }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // Collect some metrics about the batch
        let batch_input_size: usize = batch
            .iter()
            .filter_map(|x| {
                if let Ok(req) = x {
                    Some(req.params().as_ref().map_or(0, |p| p.get().len()))
                } else {
                    None
                }
            })
            .sum();

        let request_counts = batch
            .iter()
            .filter_map(|x| {
                if let Ok(req) = x {
                    Some(normalize_method_label(req.method_name()))
                } else {
                    None
                }
            })
            .fold(HashMap::new(), |mut acc, method| {
                *acc.entry(method).or_insert(0u64) += 1;
                acc
            });

        let mut batch_rp = BatchResponseBuilder::new_with_limit(self.max_response_size_bytes);
        let service = self.clone();
        async move {
            let mut guard = BatchGuard::new(batch_input_size, request_counts);
            let mut got_notification = false;

            for batch_entry in batch.into_iter() {
                match batch_entry {
                    Ok(BatchEntry::Call(req)) => {
                        let rp = service.call(req).await;
                        if let Err(err) = batch_rp.append(rp) {
                            return err;
                        }
                    }
                    Ok(BatchEntry::Notification(n)) => {
                        got_notification = true;
                        service.notification(n).await;
                    }
                    Err(err) => {
                        let (err, id) = err.into_parts();
                        let rp = MethodResponse::error(id, err);
                        if let Err(err) = batch_rp.append(rp) {
                            return err;
                        }
                    }
                }
            }

            // If the batch is empty, and we got a notification, we return an empty response.
            let response = if batch_rp.is_empty() && got_notification {
                MethodResponse::notification()
            } else {
                MethodResponse::from_batch(batch_rp.finish())
            };

            guard.completed = Some(response.as_json().get().len());
            response
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        let request_size = n.params.as_ref().map_or(0, |p| p.get().len());
        let method = normalize_method_label(n.method_name());
        let inner = self.inner.clone();

        async move {
            let handler = async move { inner.notification(n).await };
            CallGuard::new(CallKind::Notification, method, request_size)
                .handle_result(handler, MethodResponse::notification)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UNKNOWN_METHOD_LABEL, is_heavy_rpc_method, normalize_method_label};

    #[test]
    fn known_methods_keep_specific_labels() {
        assert_eq!(normalize_method_label("eth_call"), "eth_call");
        assert_eq!(
            normalize_method_label("debug_traceTransaction"),
            "debug_traceTransaction"
        );
        assert_eq!(
            normalize_method_label("zks_getBridgehubContract"),
            "zks_getBridgehubContract"
        );
    }

    #[test]
    fn unknown_methods_share_one_bounded_label() {
        assert_eq!(
            normalize_method_label("attacker_unique_1"),
            UNKNOWN_METHOD_LABEL
        );
        assert_eq!(
            normalize_method_label(&"attacker_unique_2".repeat(1024)),
            UNKNOWN_METHOD_LABEL
        );
    }

    #[test]
    fn heavy_methods_are_gated() {
        assert!(is_heavy_rpc_method("eth_call"));
        assert!(is_heavy_rpc_method("eth_estimateGas"));
        assert!(is_heavy_rpc_method("eth_getLogs"));
        assert!(is_heavy_rpc_method("debug_traceTransaction"));
        assert!(is_heavy_rpc_method("zks_getProof"));
        assert!(is_heavy_rpc_method("unstable_getLocalRoot"));
        assert!(!is_heavy_rpc_method("eth_blockNumber"));
        assert!(!is_heavy_rpc_method(UNKNOWN_METHOD_LABEL));
    }
}
