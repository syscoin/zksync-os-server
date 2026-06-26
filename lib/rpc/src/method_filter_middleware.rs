use jsonrpsee::core::middleware::{Batch, BatchEntry, Notification};
use jsonrpsee::server::middleware::rpc::{RpcService, RpcServiceT};
use jsonrpsee::types::Request;
use jsonrpsee::types::error::{ErrorObject, METHOD_NOT_FOUND_CODE};
use jsonrpsee::{BatchResponseBuilder, MethodResponse};
use std::collections::HashSet;
use std::sync::Arc;

fn method_disabled_err() -> ErrorObject<'static> {
    ErrorObject::owned(METHOD_NOT_FOUND_CODE, "Method disabled", None::<()>)
}

/// JSON-RPC middleware that rejects filtered methods with -32601 and emits a warning.
#[derive(Clone)]
pub(crate) struct MethodFiltering<S = RpcService> {
    inner: S,
    filter: Arc<HashSet<String>>,
    max_response_size_bytes: usize,
}

impl<S> MethodFiltering<S> {
    pub(crate) fn new(
        inner: S,
        filter: Arc<HashSet<String>>,
        max_response_size_bytes: usize,
    ) -> Self {
        Self {
            inner,
            filter,
            max_response_size_bytes,
        }
    }
}

impl<S> RpcServiceT for MethodFiltering<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Clone
        + Send
        + 'static,
{
    type MethodResponse = MethodResponse;
    type NotificationResponse = MethodResponse;
    type BatchResponse = MethodResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let rejected = self.filter.contains(request.method_name());
        let inner = self.inner.clone();
        async move {
            if rejected {
                tracing::warn!(
                    method = request.method_name(),
                    "rpc method rejected by filter",
                );
                let id = request.id.clone().into_owned();
                return MethodResponse::error(id, method_disabled_err());
            }
            inner.call(request).await
        }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // SYSCOIN: enforce filters for batch entries too, including notification entries that
        // cannot return an error response but still must not execute stateful disabled methods.
        let mut batch_rp = BatchResponseBuilder::new_with_limit(self.max_response_size_bytes);
        let service = self.clone();
        async move {
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

            if batch_rp.is_empty() && got_notification {
                MethodResponse::notification()
            } else {
                MethodResponse::from_batch(batch_rp.finish())
            }
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        // SYSCOIN: notifications cannot return -32601, but filtered methods must not execute.
        let rejected = self.filter.contains(n.method_name());
        let inner = self.inner.clone();
        async move {
            if rejected {
                tracing::warn!(
                    method = n.method_name(),
                    "rpc notification rejected by filter"
                );
                return MethodResponse::notification();
            }
            inner.notification(n).await
        }
    }
}
