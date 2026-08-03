use crate::metrics::{API_METRICS, RPC_TASK_MONITOR};
use crate::result::internal_rpc_err;
use futures::{FutureExt as _, StreamExt as _};
use jsonrpsee::core::middleware::{Batch, BatchEntry, Notification};
use jsonrpsee::server::middleware::rpc::{RpcService, RpcServiceT};
use jsonrpsee::types::error::reject_too_big_batch_response;
use jsonrpsee::types::{Id, Request};
use jsonrpsee::{BatchResponseBuilder, MethodResponse};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy, Debug)]
pub enum CallKind {
    Call,
    Notification,
}

/// Metric label for any method name that isn't registered on the server. Folding all such names
/// into one label bounds metric cardinality, so a client can't spawn unbounded time series by
/// sending requests for arbitrary nonexistent methods.
const UNKNOWN_METHOD: &str = "<unknown>";

/// Keeps one batch from flooding the runtime with spawned tasks. This limit applies per batch;
/// separate batches and connections can still make progress independently.
const MAX_CONCURRENT_BATCH_ENTRIES: usize = 32;

/// Records RPC metrics and owns the custom batch-dispatch behavior.
///
/// Calls always pass through the inner service. When parallel batches are enabled, calls within a
/// batch are spawned with bounded concurrency and their responses are put back in request order.
#[derive(Clone)]
pub struct Monitoring<S = RpcService> {
    inner: S,
    max_response_size_bytes: usize,
    blocking_rpcs_semaphore: Arc<Semaphore>,
    known_methods: Arc<HashSet<&'static str>>,
    parallel_batches: bool,
}

impl<S> Monitoring<S> {
    pub fn new(
        inner: S,
        max_response_size_bytes: u32,
        blocking_rpcs_semaphore: Arc<Semaphore>,
        known_methods: Arc<HashSet<&'static str>>,
        parallel_batches: bool,
    ) -> Self {
        Self {
            inner,
            max_response_size_bytes: max_response_size_bytes as usize,
            blocking_rpcs_semaphore,
            known_methods,
            parallel_batches,
        }
    }
}

/// Maps a method name to a bounded metric label: the registered name (a `'static` string, so no
/// per-request allocation) or [`UNKNOWN_METHOD`].
fn method_label(known_methods: &HashSet<&'static str>, method: &str) -> &'static str {
    known_methods.get(method).copied().unwrap_or(UNKNOWN_METHOD)
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
            // SYSCOIN: eth_simulateV1 can execute many VM blocks, so gate it with other heavy RPCs.
            | "eth_simulateV1"
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

impl<S> RpcServiceT for Monitoring<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = MethodResponse;
    type BatchResponse = MethodResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = method_label(&self.known_methods, request.method_name());
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
                    Some(method_label(&self.known_methods, req.method_name()))
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

            if service.parallel_batches {
                let responses = match service
                    .execute_batch_parallel(batch, &mut got_notification)
                    .await
                {
                    Ok(responses) => responses,
                    Err(err) => return err,
                };
                for rp in responses {
                    if let Err(err) = batch_rp.append(rp) {
                        return err;
                    }
                }
            } else {
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
        let method = method_label(&self.known_methods, n.method_name());
        let inner = self.inner.clone();

        async move {
            let handler = async move { inner.notification(n).await };
            CallGuard::new(CallKind::Notification, method, request_size)
                .handle_result(handler, MethodResponse::notification)
                .await
        }
    }
}

impl<S> Monitoring<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    /// Executes a batch's calls in parallel, with at most [`MAX_CONCURRENT_BATCH_ENTRIES`] in
    /// flight, and returns their responses in request order. Notifications are handled inline
    /// (they have no response slot) and reported via `got_notification`.
    ///
    /// Fails with the whole-batch error response when the combined response size exceeds
    /// `max_response_size_bytes`.
    async fn execute_batch_parallel(
        &self,
        batch: Batch<'_>,
        got_notification: &mut bool,
    ) -> Result<Vec<MethodResponse>, MethodResponse> {
        enum Prepared {
            /// An owned call ready to spawn. Its id is used if the task cannot be joined.
            Call {
                id: Id<'static>,
                req: Request<'static>,
            },
            /// A malformed entry whose error response is already known.
            Ready(MethodResponse),
        }

        let mut prepared = Vec::with_capacity(batch.len());
        for batch_entry in batch.into_iter() {
            match batch_entry {
                Ok(BatchEntry::Call(mut req)) => {
                    let id = req.id.clone().into_owned();
                    // Spawned tasks require `'static` data. Rebuild the request from owned
                    // parts, including the per-connection context in its extensions.
                    let mut owned = Request::owned(
                        req.method_name().to_owned(),
                        req.params.take().map(|params| params.into_owned()),
                        id.clone(),
                    );
                    *owned.extensions_mut() = std::mem::take(req.extensions_mut());
                    prepared.push(Prepared::Call { id, req: owned });
                }
                Ok(BatchEntry::Notification(n)) => {
                    *got_notification = true;
                    // jsonrpsee's root service only propagates notification extensions;
                    // awaiting it here records our metrics without making `Prepared`
                    // borrow from the batch.
                    self.notification(n).await;
                }
                Err(err) => {
                    let (err, id) = err.into_parts();
                    prepared.push(Prepared::Ready(MethodResponse::error(id, err)));
                }
            }
        }

        // Spawning lets CPU-heavy handlers use multiple runtime workers. An unordered
        // buffer keeps the window full when an early call is slow; sorting below restores
        // request order before the response is built.
        //
        // Abort-on-drop prevents calls from running detached after the batch is cancelled.
        // Cancellation takes effect when a handler next yields.
        let response_capacity = prepared.len();
        let mut response_stream = futures::stream::iter(prepared.into_iter().enumerate().map(
            move |(index, entry)| async move {
                let rp = match entry {
                    Prepared::Call { id, req } => {
                        match AbortOnDropHandle::new(tokio::spawn(self.call(req))).await {
                            Ok(rp) => rp,
                            // `CallGuard` converts handler panics. Treat any remaining
                            // task failure as an internal error for this call.
                            Err(_) => MethodResponse::error(id, internal_rpc_err("Internal error")),
                        }
                    }
                    Prepared::Ready(rp) => rp,
                };
                (index, rp)
            },
        ))
        .buffer_unordered(MAX_CONCURRENT_BATCH_ENTRIES);

        // Enforce the limit as unordered results arrive. This caps retained responses and
        // dropping the stream on overflow cancels work whose output would be discarded.
        // The accounting mirrors `BatchResponseBuilder`: `[` plus every response and one
        // delimiter, with the final comma replaced by `]`.
        let mut responses = Vec::with_capacity(response_capacity);
        let mut response_size = 1usize;
        while let Some((index, rp)) = response_stream.next().await {
            let next_size = response_size
                .checked_add(rp.as_json().get().len())
                .and_then(|size| size.checked_add(1));
            match next_size {
                Some(next_size) if next_size <= self.max_response_size_bytes => {
                    response_size = next_size;
                    responses.push((index, rp));
                }
                _ => {
                    return Err(MethodResponse::error(
                        Id::Null,
                        reject_too_big_batch_response(self.max_response_size_bytes),
                    ));
                }
            }
        }

        responses.sort_unstable_by_key(|(index, _)| *index);
        Ok(responses.into_iter().map(|(_, rp)| rp).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{UNKNOWN_METHOD, is_heavy_rpc_method, method_label};
    use std::collections::HashSet;

    #[test]
    fn registered_methods_pass_through_unknown_methods_collapse() {
        let known: HashSet<&'static str> = ["eth_call", "eth_getBlockByHash"].into_iter().collect();

        // Registered methods are reported verbatim.
        assert_eq!(method_label(&known, "eth_call"), "eth_call");
        assert_eq!(
            method_label(&known, "eth_getBlockByHash"),
            "eth_getBlockByHash"
        );

        // Anything unregistered — including arbitrarily long junk used to pollute metrics —
        // collapses to a single bounded label instead of minting a new time series.
        assert_eq!(method_label(&known, "eth_does_not_exist"), UNKNOWN_METHOD);
        assert_eq!(method_label(&known, ""), UNKNOWN_METHOD);
        let junk = format!("eth_{}", "a".repeat(1_000_000));
        assert_eq!(method_label(&known, &junk), UNKNOWN_METHOD);
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
        assert!(!is_heavy_rpc_method(UNKNOWN_METHOD));
    }
}

