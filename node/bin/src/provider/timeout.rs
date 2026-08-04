use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
use std::task::{Context, Poll};
use std::time::Duration;
use tower::Service;

/// SYSCOIN Matched by `retry::RetryService` to treat L1 and Gateway timeouts as retryable.
pub(super) const TIMED_OUT_MSG: &str = "settlement-layer RPC request timed out";

/// SYSCOIN Fails L1 or Gateway RPC requests that receive no response within the configured
/// timeout; without it, a request hanging on a half-dead connection never returns and freezes its
/// caller forever.
#[derive(Debug, Clone)]
pub(super) struct TimeoutService<S> {
    pub(super) inner: S,
    pub(super) timeout: Duration,
}

impl<S> Service<RequestPacket> for TimeoutService<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Send
        + 'static
        + Clone,
    S::Future: Send + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, inner);
        let timeout = self.timeout;
        Box::pin(async move {
            match tokio::time::timeout(timeout, inner.call(request)).await {
                Ok(result) => result,
                Err(_) => Err(TransportErrorKind::custom_str(&format!(
                    "{TIMED_OUT_MSG} after {timeout:?}"
                ))),
            }
        })
    }
}
