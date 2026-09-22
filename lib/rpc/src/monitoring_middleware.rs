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
use tokio_util::task::AbortOnDropHandle;

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

// SYSCOIN: `zks_getL2ToL1LogProof` may retain a maximum-size provider `ResponsePacket` while
// Alloy performs typed deserialization and the RPC handler constructs its response. Keep that
// end-to-end overlap to 8 (1 GiB at the 128-MiB provider cap) without serializing the route.
pub(crate) const MAX_CONCURRENT_L2_TO_L1_LOG_PROOF_RPCS: usize = 8;

const L2_TO_L1_LOG_PROOF_METHOD: &str = "zks_getL2ToL1LogProof";

// SYSCOIN: jsonrpsee clones request extensions into its blocking worker. Shared ownership keeps
// admission charged until that worker finishes, even when cancellation drops the awaiting RPC.
struct RpcAdmissionPermits {
    _blocking: Option<OwnedSemaphorePermit>,
    _log_proof: Option<OwnedSemaphorePermit>,
}

impl RpcAdmissionPermits {
    fn new(
        blocking: Option<OwnedSemaphorePermit>,
        log_proof: Option<OwnedSemaphorePermit>,
    ) -> Option<Arc<Self>> {
        (blocking.is_some() || log_proof.is_some()).then(|| {
            Arc::new(Self {
                _blocking: blocking,
                _log_proof: log_proof,
            })
        })
    }
}

/// Records RPC metrics and owns the custom batch-dispatch behavior.
///
/// Calls always pass through the inner service. When parallel batches are enabled, calls within a
/// batch are spawned with bounded concurrency and their responses are put back in request order.
#[derive(Clone)]
pub struct Monitoring<S = RpcService> {
    inner: S,
    max_response_size_bytes: usize,
    blocking_rpcs_semaphore: Arc<Semaphore>,
    // SYSCOIN: This route-specific gate spans the complete RPC handler, including Alloy's typed
    // provider deserialization, which cannot retain the lower transport's byte-budget guard.
    l2_to_l1_log_proof_semaphore: Arc<Semaphore>,
    known_methods: Arc<HashSet<&'static str>>,
    parallel_batches: bool,
}

impl<S> Monitoring<S> {
    pub fn new(
        inner: S,
        max_response_size_bytes: u32,
        blocking_rpcs_semaphore: Arc<Semaphore>,
        l2_to_l1_log_proof_semaphore: Arc<Semaphore>,
        known_methods: Arc<HashSet<&'static str>>,
        parallel_batches: bool,
    ) -> Self {
        Self {
            inner,
            max_response_size_bytes: max_response_size_bytes as usize,
            blocking_rpcs_semaphore,
            l2_to_l1_log_proof_semaphore,
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
            // SYSCOIN: Filling an omitted gas limit runs the same VM estimator.
            | "eth_fillTransaction"
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
            // SYSCOIN: Gateway MessageRoot proof construction scans/rebuilds bounded but large
            // ancestry, receipt, event, and Merkle inputs; share the process-wide heavy-work gate.
            | "zks_getL2ToL1LogProof"
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
        mut request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let method = method_label(&self.known_methods, request.method_name());
        let request_size = request.params.as_ref().map_or(0, |p| p.get().len());
        let inner = self.inner.clone();
        let blocking_rpcs_semaphore = self.blocking_rpcs_semaphore.clone();
        let l2_to_l1_log_proof_semaphore = self.l2_to_l1_log_proof_semaphore.clone();

        async move {
            let id = request.id.clone().into_owned();
            let handler_error_id = id.clone();
            let handler = RPC_TASK_MONITOR.instrument(async move {
                // SYSCOIN: Acquire the narrow route gate before the general heavy-work gate so
                // queued log-proof calls cannot consume all general permits. Cancellation releases
                // admission only after any already-dispatched blocking worker also finishes.
                let _l2_to_l1_log_proof_permit: Option<OwnedSemaphorePermit> =
                    if method == L2_TO_L1_LOG_PROOF_METHOD {
                        match l2_to_l1_log_proof_semaphore.acquire_owned().await {
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
                // SYSCOIN: Retain a local owner for async handlers and transfer a shared owner
                // through extensions into jsonrpsee's non-cancellable blocking worker.
                let permits = RpcAdmissionPermits::new(_permit, _l2_to_l1_log_proof_permit);
                if let Some(permits) = &permits {
                    request.extensions_mut().insert(Arc::clone(permits));
                }
                let mut response = inner.call(request).await;
                // SYSCOIN: Completed responses can be retained while a batch awaits more entries.
                // They must not retain admission permits and deadlock those remaining entries.
                response
                    .extensions_mut()
                    .remove::<Arc<RpcAdmissionPermits>>();
                response
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
        mut n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        let request_size = n.params.as_ref().map_or(0, |p| p.get().len());
        let method = method_label(&self.known_methods, n.method_name());
        let inner = self.inner.clone();
        // SYSCOIN: Notifications execute the same registered handlers as calls even though they
        // produce no response. Share the process-wide permit so an attacker cannot bypass the
        // heavy-work ceiling by omitting the JSON-RPC request ID.
        let blocking_rpcs_semaphore = self.blocking_rpcs_semaphore.clone();
        let l2_to_l1_log_proof_semaphore = self.l2_to_l1_log_proof_semaphore.clone();

        async move {
            let handler = async move {
                // SYSCOIN: Notifications reach the same handler and therefore share the same
                // end-to-end log-proof memory gate; a closed gate fails closed.
                let _l2_to_l1_log_proof_permit: Option<OwnedSemaphorePermit> =
                    if method == L2_TO_L1_LOG_PROOF_METHOD {
                        match l2_to_l1_log_proof_semaphore.acquire_owned().await {
                            Ok(permit) => Some(permit),
                            Err(_) => return MethodResponse::notification(),
                        }
                    } else {
                        None
                    };
                let _permit: Option<OwnedSemaphorePermit> = if is_heavy_rpc_method(method) {
                    match blocking_rpcs_semaphore.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        // SYSCOIN: Notifications cannot return an error response. A closed
                        // process-wide gate therefore fails closed without running the handler.
                        Err(_) => return MethodResponse::notification(),
                    }
                } else {
                    None
                };
                // SYSCOIN: Keep notification admission tied to the same worker ownership as calls,
                // including inner services that execute notifications on the blocking pool.
                let permits = RpcAdmissionPermits::new(_permit, _l2_to_l1_log_proof_permit);
                if let Some(permits) = &permits {
                    n.extensions_mut().insert(Arc::clone(permits));
                }
                let mut response = inner.notification(n).await;
                response
                    .extensions_mut()
                    .remove::<Arc<RpcAdmissionPermits>>();
                response
            };
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
        // SYSCOIN: Abort-on-drop cancels async dispatch when it next yields. Already-started
        // blocking workers continue while retaining their shared admission permits.
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
    use super::{Monitoring, UNKNOWN_METHOD, is_heavy_rpc_method, method_label};
    // SYSCOIN: Exercise jsonrpsee's actual blocking callback and extension ownership offline.
    use futures::FutureExt as _;
    use jsonrpsee::core::middleware::{Batch, BatchEntry, Notification};
    use jsonrpsee::core::server::{MethodCallback, Methods};
    use jsonrpsee::server::middleware::rpc::RpcServiceT;
    use jsonrpsee::types::{ErrorObjectOwned, Id, Params, Request};
    use jsonrpsee::{MethodResponse, ResponsePayload, RpcModule};
    use std::borrow::Cow;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{Notify, Semaphore, mpsc, oneshot};

    // SYSCOIN: RpcService's constructor is private; dispatch its real registered callback so these
    // tests cover jsonrpsee's spawn_blocking boundary rather than a cancellable async stand-in.
    #[derive(Clone)]
    struct BlockingTestService {
        methods: Methods,
    }

    impl RpcServiceT for BlockingTestService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        fn call<'a>(
            &self,
            mut request: Request<'a>,
        ) -> impl Future<Output = MethodResponse> + Send + 'a {
            let MethodCallback::Async(callback) =
                self.methods.method(request.method_name()).unwrap().clone()
            else {
                panic!("expected jsonrpsee's registered blocking callback");
            };
            let params =
                Params::new(request.params.as_ref().map(|params| params.get())).into_owned();
            let extensions = std::mem::take(request.extensions_mut());
            callback(
                request.id.into_owned(),
                params,
                0usize.into(),
                1_000_000,
                extensions,
            )
        }

        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(&self, _batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            async { panic!("monitoring dispatches each batch entry") }
        }

        fn notification<'a>(
            &self,
            mut notification: Notification<'a>,
        ) -> impl Future<Output = MethodResponse> + Send + 'a {
            // SYSCOIN: Monitoring is generic over inner services, including notification handlers
            // that dispatch work; exercise that sibling ownership path with the same real worker.
            let mut request = Request::owned(
                notification.method_name().to_owned(),
                notification.params.take().map(|params| params.into_owned()),
                Id::Null,
            );
            *request.extensions_mut() = std::mem::take(notification.extensions_mut());
            self.call(request)
        }
    }

    // SYSCOIN: A dropped release sender also unblocks its worker, so failed assertions cannot hang
    // runtime shutdown. No test needs VM work or wall-clock sleeps to keep a worker running.
    enum BlockingOutcome {
        Success(String),
        Error,
        Panic,
    }

    fn blocking_test_monitoring(
        permits: usize,
        response_limit: u32,
        parallel: bool,
    ) -> (
        Monitoring<BlockingTestService>,
        Arc<Semaphore>,
        mpsc::UnboundedReceiver<oneshot::Sender<BlockingOutcome>>,
    ) {
        let (started_tx, started_rx) = mpsc::unbounded_channel();
        let mut module = RpcModule::new(());
        module
            .register_blocking_method("eth_call", move |_, _, _| {
                let (release_tx, release_rx) = oneshot::channel();
                let _ = started_tx.send(release_tx);
                match release_rx.blocking_recv() {
                    Ok(BlockingOutcome::Success(value)) => Ok(value),
                    Ok(BlockingOutcome::Error) | Err(_) => {
                        Err(ErrorObjectOwned::owned(-32000, "test error", None::<()>))
                    }
                    Ok(BlockingOutcome::Panic) => panic!("test blocking worker panic"),
                }
            })
            .unwrap();
        let semaphore = Arc::new(Semaphore::new(permits));
        let monitoring = Monitoring::new(
            BlockingTestService {
                methods: module.into(),
            },
            response_limit,
            semaphore.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(["eth_call"].into_iter().collect()),
            parallel,
        );
        (monitoring, semaphore, started_rx)
    }

    fn blocking_request(id: u64) -> Request<'static> {
        Request::owned("eth_call".to_owned(), None, Id::Number(id))
    }

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("blocking RPC test did not make progress")
    }

    // SYSCOIN: Cancellation must not readmit work while the previous blocking worker still runs;
    // cancelling a queued request must also prevent it from dispatching later.
    #[tokio::test]
    async fn cancelled_call_keeps_blocking_worker_permit() {
        let (monitoring, semaphore, mut started) = blocking_test_monitoring(1, 1_000_000, false);
        let task = tokio::spawn(monitoring.call(blocking_request(1)));
        let release = bounded(started.recv()).await.unwrap();
        task.abort();
        assert!(bounded(task).await.unwrap_err().is_cancelled());
        assert_eq!(semaphore.available_permits(), 0);

        let mut waiter = Box::pin(monitoring.call(blocking_request(2)));
        assert!(waiter.as_mut().now_or_never().is_none());
        assert!(started.try_recv().is_err());
        drop(waiter);
        assert!(
            release
                .send(BlockingOutcome::Success("done".into()))
                .is_ok()
        );
        let _permit = bounded(semaphore.acquire()).await.unwrap();
        assert!(started.try_recv().is_err());
    }

    // SYSCOIN: Notification dispatch shares the same cancellation boundary as ordinary calls.
    #[tokio::test]
    async fn cancelled_notification_keeps_blocking_worker_permit() {
        let (monitoring, semaphore, mut started) = blocking_test_monitoring(1, 1_000_000, false);
        let notification = Notification::new(Cow::Borrowed("eth_call"), None);
        let task = tokio::spawn(monitoring.notification(notification));
        let release = bounded(started.recv()).await.unwrap();
        task.abort();
        assert!(bounded(task).await.unwrap_err().is_cancelled());
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            release
                .send(BlockingOutcome::Success("done".into()))
                .is_ok()
        );
        let _permit = bounded(semaphore.acquire()).await.unwrap();
    }

    // SYSCOIN: Both batch dispatch modes must retain running-worker admission when the client
    // drops the whole batch, while entries still waiting for admission must never execute.
    #[tokio::test]
    async fn cancelled_batches_keep_blocking_worker_permits() {
        for parallel in [false, true] {
            let (monitoring, semaphore, mut started) =
                blocking_test_monitoring(1, 1_000_000, parallel);
            let batch = Batch::from(vec![
                Ok(BatchEntry::Call(blocking_request(1))),
                Ok(BatchEntry::Call(blocking_request(2))),
            ]);
            let task = tokio::spawn(monitoring.batch(batch));
            let release = bounded(started.recv()).await.unwrap();
            task.abort();
            assert!(bounded(task).await.unwrap_err().is_cancelled());
            assert_eq!(semaphore.available_permits(), 0);
            assert!(
                release
                    .send(BlockingOutcome::Success("done".into()))
                    .is_ok()
            );
            let _permit = bounded(semaphore.acquire()).await.unwrap();
            assert!(started.try_recv().is_err());
        }
    }

    // SYSCOIN: A response overflow aborts sibling async wrappers without cancelling their workers.
    #[tokio::test]
    async fn parallel_batch_overflow_keeps_sibling_worker_permit() {
        let (monitoring, semaphore, mut started) = blocking_test_monitoring(2, 100, true);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(blocking_request(1))),
            Ok(BatchEntry::Call(blocking_request(2))),
        ]);
        let task = tokio::spawn(monitoring.batch(batch));
        let first = bounded(started.recv()).await.unwrap();
        let second = bounded(started.recv()).await.unwrap();
        assert!(
            first
                .send(BlockingOutcome::Success("x".repeat(200)))
                .is_ok()
        );
        let response = bounded(task).await.unwrap();
        assert!(response.is_error());
        assert_eq!(semaphore.available_permits(), 1);
        assert!(second.send(BlockingOutcome::Success("done".into())).is_ok());
        let _permits = bounded(semaphore.acquire_many(2)).await.unwrap();
    }

    // SYSCOIN: Completed responses stay in the batch accumulator. Success, error, and panic must
    // release their admission before the batch finishes so later entries cannot deadlock.
    #[tokio::test]
    async fn completed_blocking_responses_release_batch_permits() {
        let (monitoring, semaphore, mut started) = blocking_test_monitoring(1, 1_000_000, true);
        let batch = Batch::from(
            (1..=3)
                .map(|id| Ok(BatchEntry::Call(blocking_request(id))))
                .collect::<Vec<_>>(),
        );
        let task = tokio::spawn(monitoring.batch(batch));
        for outcome in [
            BlockingOutcome::Success("done".into()),
            BlockingOutcome::Error,
            BlockingOutcome::Panic,
        ] {
            let release = bounded(started.recv()).await.unwrap();
            assert!(release.send(outcome).is_ok());
        }
        let response = bounded(task).await.unwrap();
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.as_json().get()).unwrap();
        assert_eq!(responses.len(), 3);
        assert_eq!(semaphore.available_permits(), 1);
    }

    // SYSCOIN: Observe inner dispatch so a classification assertion alone cannot hide a bypass.
    #[derive(Clone)]
    struct AdmissionTestService {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl RpcServiceT for AdmissionTestService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        fn call<'a>(
            &self,
            request: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            let started = self.started.clone();
            let release = self.release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                let params: serde_json::Value =
                    serde_json::from_str(request.params.as_ref().unwrap().get()).unwrap();
                MethodResponse::response(request.id, ResponsePayload::success(params), 1_000_000)
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(
            &self,
            _batch: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            async { MethodResponse::notification() }
        }

        fn notification<'a>(
            &self,
            _notification: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            let started = self.started.clone();
            let release = self.release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                MethodResponse::notification()
            }
        }
    }

    fn notification_test_monitoring(
        blocking_semaphore: Arc<Semaphore>,
        l2_to_l1_log_proof_semaphore: Arc<Semaphore>,
        started: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Monitoring<AdmissionTestService> {
        Monitoring::new(
            AdmissionTestService { started, release },
            1_000_000,
            blocking_semaphore,
            l2_to_l1_log_proof_semaphore,
            Arc::new(
                ["zks_getL2ToL1LogProof", "eth_call", "eth_blockNumber"]
                    .into_iter()
                    .collect(),
            ),
            false,
        )
    }

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
        assert!(is_heavy_rpc_method("eth_fillTransaction"));
        assert!(is_heavy_rpc_method("eth_getLogs"));
        assert!(is_heavy_rpc_method("debug_traceTransaction"));
        assert!(is_heavy_rpc_method("zks_getL2ToL1LogProof"));
        // SYSCOIN: IMT reconstruction owns a stricter fail-fast one-wide lane at the reader
        // boundary, so requests never queue here while occupying the broad heavy-work budget.
        assert!(!is_heavy_rpc_method("zks_getImtInclusionProof"));
        assert!(!is_heavy_rpc_method("zks_getImtLowNullifierIndex"));
        assert!(is_heavy_rpc_method("zks_getProof"));
        assert!(is_heavy_rpc_method("unstable_getLocalRoot"));
        assert!(!is_heavy_rpc_method("eth_blockNumber"));
        assert!(!is_heavy_rpc_method(UNKNOWN_METHOD));
    }

    // SYSCOIN: Exercise each batch mode because all filler entry paths must share the VM budget.
    async fn assert_fill_transaction_uses_shared_permit(parallel_batch: Option<bool>) {
        let semaphore = Arc::new(Semaphore::new(1));
        let held = semaphore.clone().acquire_owned().await.unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let monitoring = Monitoring::new(
            AdmissionTestService {
                started: started.clone(),
                release: release.clone(),
            },
            1_000_000,
            semaphore.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(["eth_fillTransaction"].into_iter().collect()),
            parallel_batch.unwrap_or(false),
        );
        let params = serde_json::json!([{
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002"
        }]);
        let request = Request::owned(
            "eth_fillTransaction".to_owned(),
            Some(serde_json::value::to_raw_value(&params).unwrap()),
            Id::Number(7),
        );
        let task = tokio::spawn(async move {
            if parallel_batch.is_some() {
                monitoring
                    .batch(Batch::from(vec![Ok(BatchEntry::Call(request))]))
                    .await
            } else {
                monitoring.call(request).await
            }
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), started.notified())
                .await
                .is_err(),
            "eth_fillTransaction ran without a heavy-work permit"
        );
        drop(held);
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("eth_fillTransaction did not run after a permit became available");
        assert_eq!(semaphore.available_permits(), 0);

        release.notify_one();
        let response = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("eth_fillTransaction did not complete")
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(response.as_json().get()).unwrap();
        let expected = serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": params});
        assert_eq!(
            response,
            if parallel_batch.is_some() {
                serde_json::json!([expected])
            } else {
                expected
            }
        );
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn fill_transaction_call_uses_shared_permit() {
        assert_fill_transaction_uses_shared_permit(None).await;
    }

    #[tokio::test(start_paused = true)]
    async fn fill_transaction_sequential_batch_uses_shared_permit() {
        assert_fill_transaction_uses_shared_permit(Some(false)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn fill_transaction_parallel_batch_uses_shared_permit() {
        assert_fill_transaction_uses_shared_permit(Some(true)).await;
    }

    // SYSCOIN: Saturating the VM budget must leave ordinary RPC calls responsive.
    #[tokio::test(start_paused = true)]
    async fn ordinary_call_does_not_wait_for_heavy_permit() {
        let semaphore = Arc::new(Semaphore::new(1));
        let held = semaphore.clone().acquire_owned().await.unwrap();
        let release = Arc::new(Notify::new());
        release.notify_one();
        let monitoring = notification_test_monitoring(
            semaphore.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(Notify::new()),
            release,
        );
        let request = Request::owned(
            "eth_blockNumber".to_owned(),
            Some(serde_json::value::to_raw_value(&serde_json::json!([])).unwrap()),
            Id::Number(8),
        );

        let response = tokio::time::timeout(Duration::from_secs(1), monitoring.call(request))
            .await
            .expect("ordinary call incorrectly waited for the heavy-work permit");
        let response: serde_json::Value = serde_json::from_str(response.as_json().get()).unwrap();
        assert_eq!(
            response,
            serde_json::json!({"jsonrpc": "2.0", "id": 8, "result": []})
        );
        assert_eq!(semaphore.available_permits(), 0);
        drop(held);
        assert_eq!(semaphore.available_permits(), 1);
    }

    // SYSCOIN: Omitting a JSON-RPC request ID must not bypass the shared heavy-work ceiling, and
    // cancelling a waiting/running notification must return its owned permit immediately.
    #[tokio::test]
    async fn heavy_notification_waits_for_and_releases_owned_permit_on_cancellation() {
        let semaphore = Arc::new(Semaphore::new(1));
        let l2_to_l1_log_proof_semaphore = Arc::new(Semaphore::new(1));
        let held = semaphore.clone().acquire_owned().await.unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let monitoring = notification_test_monitoring(
            semaphore.clone(),
            l2_to_l1_log_proof_semaphore.clone(),
            started.clone(),
            release,
        );
        let notification = Notification::new(Cow::Borrowed("zks_getL2ToL1LogProof"), None);
        let task = tokio::spawn(async move { monitoring.notification(notification).await });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), started.notified())
                .await
                .is_err(),
            "heavy notification ran without a permit"
        );
        drop(held);
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("heavy notification did not run after a permit became available");
        assert_eq!(semaphore.available_permits(), 0);

        task.abort();
        let _ = task.await;
        assert_eq!(semaphore.available_permits(), 1);
        assert_eq!(l2_to_l1_log_proof_semaphore.available_permits(), 1);
    }

    // SYSCOIN: Log-proof waiters acquire the narrow memory gate before the shared heavy-work
    // gate. They therefore cannot starve unrelated heavy RPCs, and cancellation returns both
    // owned permits after the handler has begun.
    #[tokio::test]
    async fn log_proof_gate_is_route_specific_and_releases_on_cancellation() {
        let blocking_semaphore = Arc::new(Semaphore::new(1));
        let l2_to_l1_log_proof_semaphore = Arc::new(Semaphore::new(1));
        let held_log_proof = l2_to_l1_log_proof_semaphore
            .clone()
            .acquire_owned()
            .await
            .unwrap();

        let log_started = Arc::new(Notify::new());
        let log_monitoring = notification_test_monitoring(
            blocking_semaphore.clone(),
            l2_to_l1_log_proof_semaphore.clone(),
            log_started.clone(),
            Arc::new(Notify::new()),
        );
        let log_notification = Notification::new(Cow::Borrowed("zks_getL2ToL1LogProof"), None);
        let log_task =
            tokio::spawn(async move { log_monitoring.notification(log_notification).await });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), log_started.notified())
                .await
                .is_err(),
            "log-proof notification ran without its route permit"
        );
        assert_eq!(
            blocking_semaphore.available_permits(),
            1,
            "a route-gated waiter consumed the general heavy-work permit"
        );

        let other_started = Arc::new(Notify::new());
        let other_monitoring = notification_test_monitoring(
            blocking_semaphore.clone(),
            l2_to_l1_log_proof_semaphore.clone(),
            other_started.clone(),
            Arc::new(Notify::new()),
        );
        let other_notification = Notification::new(Cow::Borrowed("eth_call"), None);
        let other_task =
            tokio::spawn(async move { other_monitoring.notification(other_notification).await });
        tokio::time::timeout(Duration::from_secs(1), other_started.notified())
            .await
            .expect("unrelated heavy notification waited for the log-proof route permit");
        other_task.abort();
        let _ = other_task.await;
        assert_eq!(blocking_semaphore.available_permits(), 1);

        drop(held_log_proof);
        tokio::time::timeout(Duration::from_secs(1), log_started.notified())
            .await
            .expect("log-proof notification did not run after its route permit became available");
        assert_eq!(blocking_semaphore.available_permits(), 0);
        assert_eq!(l2_to_l1_log_proof_semaphore.available_permits(), 0);

        log_task.abort();
        let _ = log_task.await;
        assert_eq!(blocking_semaphore.available_permits(), 1);
        assert_eq!(l2_to_l1_log_proof_semaphore.available_permits(), 1);
    }

    // SYSCOIN: The security gate is method-specific; ordinary notifications retain their existing
    // behavior and do not wait behind unrelated heavy work.
    #[tokio::test]
    async fn ordinary_notification_does_not_wait_for_heavy_permit() {
        let semaphore = Arc::new(Semaphore::new(1));
        let l2_to_l1_log_proof_semaphore = Arc::new(Semaphore::new(1));
        let held = semaphore.clone().acquire_owned().await.unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let monitoring = notification_test_monitoring(
            semaphore.clone(),
            l2_to_l1_log_proof_semaphore.clone(),
            started.clone(),
            release,
        );
        let notification = Notification::new(Cow::Borrowed("eth_blockNumber"), None);
        let task = tokio::spawn(async move { monitoring.notification(notification).await });

        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("ordinary notification incorrectly waited for the heavy-work permit");
        task.abort();
        let _ = task.await;
        assert_eq!(semaphore.available_permits(), 0);
        assert_eq!(l2_to_l1_log_proof_semaphore.available_permits(), 1);
        drop(held);
        assert_eq!(semaphore.available_permits(), 1);
    }
}
