use alloy::rpc::json_rpc::{RequestPacket, Response, ResponsePacket, ResponsePayload};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut, TransportResult};
use reqwest::{Client, StatusCode, header};
use std::borrow::Cow;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tower::Service;
use url::Url;

// SYSCOIN: A canonical AppendedChainBatchRoot log is well below 2 KiB in JSON, so this admits the
// complete 65,536-event proof bound with conservative metadata overhead while preventing an RPC
// peer from materializing an unbounded response before semantic validation.
const MAX_PROVIDER_RESPONSE_BYTES: usize = 128 * 1024 * 1024;
// SYSCOIN: Do not eagerly reserve an attacker-declared Content-Length; grow only as streamed bytes
// arrive while retaining a small allocation optimization for ordinary JSON-RPC responses.
const MAX_INITIAL_RESPONSE_CAPACITY: usize = 64 * 1024;
// SYSCOIN: Error objects and logs must not retain a second maximum-size copy of an invalid peer
// response after the raw-body guard is released.
const MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const TRUNCATED_RESPONSE_DIAGNOSTIC: &str = "\n<provider response diagnostic truncated>";
const TRUNCATED_JSON_RPC_ERROR_MESSAGE: &str = "\n<provider JSON-RPC error truncated>";
// SYSCOIN: Bound aggregate buffered provider bodies to 512 MiB process-wide. That is 0.8% of a
// 64-GiB production host and admits three ordinary maximum-size responses plus one guaranteed
// completion lane, while small responses continue to share the budget by their actual byte size.
const MAX_AGGREGATE_PROVIDER_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

/// SYSCOIN: Process-wide weighted response-byte budget shared by L1, archive-L1, Gateway, and all
/// of their transport clones. The normal pool is byte-weighted; one full-response reserve is kept
/// behind an exclusive gate so partially buffered responses can never deadlock one another.
#[derive(Clone, Debug)]
pub(crate) struct ProviderResponseByteBudget {
    normal: Arc<Semaphore>,
    completion: Arc<Semaphore>,
    completion_gate: Arc<Semaphore>,
}

impl ProviderResponseByteBudget {
    pub(crate) fn new() -> Self {
        Self::with_limits(
            MAX_AGGREGATE_PROVIDER_RESPONSE_BYTES,
            MAX_PROVIDER_RESPONSE_BYTES,
        )
    }

    fn with_limits(total_bytes: usize, maximum_response_bytes: usize) -> Self {
        // SYSCOIN: Keep at least one normal full-response lane in addition to the exclusive
        // completion reserve; production uses four full lanes and therefore does not serialize
        // ordinary provider traffic.
        assert!(maximum_response_bytes > 0);
        assert!(total_bytes >= maximum_response_bytes.saturating_mul(2));
        assert!(maximum_response_bytes <= u32::MAX as usize);
        Self {
            normal: Arc::new(Semaphore::new(total_bytes - maximum_response_bytes)),
            completion: Arc::new(Semaphore::new(maximum_response_bytes)),
            completion_gate: Arc::new(Semaphore::new(1)),
        }
    }

    fn lease(&self) -> ResponseByteBudgetLease {
        ResponseByteBudgetLease {
            budget: self.clone(),
            normal: None,
            completion: None,
            completion_gate: None,
        }
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.normal.available_permits() + self.completion.available_permits()
    }
}

/// SYSCOIN: Permits follow the buffered bytes through transport JSON parsing and are returned on
/// success, stream/parse error, timeout, or future cancellation. Alloy's transport result is an
/// owned `ResponsePacket`, so the transport trait cannot carry this guard into the immediately
/// following typed deserialization; the public log-proof route has a separate end-to-end gate.
struct ResponseByteBudgetLease {
    budget: ProviderResponseByteBudget,
    normal: Option<OwnedSemaphorePermit>,
    completion: Option<OwnedSemaphorePermit>,
    completion_gate: Option<OwnedSemaphorePermit>,
}

impl ResponseByteBudgetLease {
    async fn acquire_chunk(&mut self, bytes: usize) -> TransportResult<()> {
        if bytes == 0 {
            return Ok(());
        }
        let permits = u32::try_from(bytes).map_err(|_| {
            TransportErrorKind::custom_str("provider response chunk exceeds semaphore capacity")
        })?;

        if self.completion_gate.is_some() {
            let permit = self
                .budget
                .completion
                .clone()
                .acquire_many_owned(permits)
                .await
                .map_err(|_| response_budget_closed_error())?;
            merge_permit(&mut self.completion, permit);
            return Ok(());
        }

        match self.budget.normal.clone().try_acquire_many_owned(permits) {
            Ok(permit) => {
                merge_permit(&mut self.normal, permit);
                return Ok(());
            }
            Err(TryAcquireError::Closed) => return Err(response_budget_closed_error()),
            Err(TryAcquireError::NoPermits) => {}
        }

        // SYSCOIN: Only one partially buffered response may use the dedicated reserve. Waiting
        // responses retain their already-accounted normal bytes; the promoted response can always
        // consume its remaining <=128 MiB and release both pools, guaranteeing forward progress.
        let completion_gate = self
            .budget
            .completion_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| response_budget_closed_error())?;

        // Capacity may have returned while this response waited for the completion gate. Prefer
        // the shared pool in that case and leave the reserve available for a genuinely blocked peer.
        match self.budget.normal.clone().try_acquire_many_owned(permits) {
            Ok(permit) => {
                merge_permit(&mut self.normal, permit);
                return Ok(());
            }
            Err(TryAcquireError::Closed) => return Err(response_budget_closed_error()),
            Err(TryAcquireError::NoPermits) => {}
        }

        self.completion_gate = Some(completion_gate);
        let permit = self
            .budget
            .completion
            .clone()
            .acquire_many_owned(permits)
            .await
            .map_err(|_| response_budget_closed_error())?;
        merge_permit(&mut self.completion, permit);
        Ok(())
    }
}

fn merge_permit(held: &mut Option<OwnedSemaphorePermit>, permit: OwnedSemaphorePermit) {
    if let Some(held) = held {
        held.merge(permit);
    } else {
        *held = Some(permit);
    }
}

fn response_budget_closed_error() -> TransportError {
    TransportErrorKind::custom_str("provider response byte budget unexpectedly closed")
}

/// SYSCOIN: HTTP transport equivalent to Alloy's reqwest transport, except response bodies are
/// streamed under a hard allocation cap before JSON deserialization. L1/Gateway providers remain
/// availability dependencies, but they cannot force an unbounded single-response allocation.
#[derive(Clone, Debug)]
pub(super) struct BoundedHttpTransport {
    client: Client,
    url: Url,
    response_byte_budget: ProviderResponseByteBudget,
}

impl BoundedHttpTransport {
    pub(super) fn new(
        url: Url,
        response_byte_budget: ProviderResponseByteBudget,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "bounded provider transport supports only HTTP(S) URLs"
        );
        Ok(Self {
            client: Client::new(),
            url,
            response_byte_budget,
        })
    }

    pub(super) fn is_local(&self) -> bool {
        matches!(
            self.url.host_str(),
            None | Some("localhost") | Some("127.0.0.1") | Some("::1")
        )
    }

    async fn execute(self, request: RequestPacket) -> TransportResult<ResponsePacket> {
        // SYSCOIN: The lease is per HTTP attempt, while its semaphores are shared across every
        // provider and clone. Outer timeout/retry layers continue to bound waits and attempts.
        let mut response_byte_lease = self.response_byte_budget.lease();
        let mut response = self
            .client
            .post(self.url)
            .json(&request)
            .headers(request.headers().clone())
            .send()
            .await
            .map_err(TransportErrorKind::custom)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok())
            .map(Duration::from_secs);
        if let Some(content_length) = response.content_length() {
            ensure_content_length_within_limit(content_length)?;
        }

        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(MAX_INITIAL_RESPONSE_CAPACITY),
        );
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                // SYSCOIN: Preserve Alloy's retry classification when an error response body
                // fails mid-stream; 429 / 503 responses must remain retryable.
                Err(error) if !status.is_success() => {
                    return Err(TransportErrorKind::http_error_with_retry_after(
                        status.as_u16(),
                        format!("<failed to read response body: {error}>"),
                        retry_after,
                    ));
                }
                Err(error) => return Err(TransportErrorKind::custom(error)),
            };
            ensure_response_growth_within_limit(
                body.len(),
                chunk.len(),
                MAX_PROVIDER_RESPONSE_BYTES,
            )?;
            response_byte_lease.acquire_chunk(chunk.len()).await?;
            extend_response_body(&mut body, &chunk)?;
        }
        tracing::debug!(bytes = body.len(), %status, "retrieved bounded provider response body");

        // SYSCOIN: Keep byte permits while serde_json temporarily overlaps `body` with the owned
        // raw payload in `ResponsePacket`; every return path below then releases them by RAII.
        let parsed = parse_response(status, &body, retry_after);
        // SYSCOIN: Release the accounted raw allocation before returning its byte permits. This
        // makes the destructor order explicit even if this function is later refactored.
        drop(body);
        drop(response_byte_lease);
        parsed
    }
}

impl Service<RequestPacket> for BoundedHttpTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let transport = self.clone();
        Box::pin(async move { transport.execute(request).await })
    }
}

fn ensure_content_length_within_limit(content_length: u64) -> TransportResult<()> {
    ensure_content_length_within_limit_for(content_length, MAX_PROVIDER_RESPONSE_BYTES)
}

fn ensure_content_length_within_limit_for(
    content_length: u64,
    maximum: usize,
) -> TransportResult<()> {
    if content_length > maximum as u64 {
        return Err(response_too_large_error(content_length, maximum));
    }
    Ok(())
}

fn extend_response_body(body: &mut Vec<u8>, chunk: &[u8]) -> TransportResult<()> {
    extend_response_body_with_limit(body, chunk, MAX_PROVIDER_RESPONSE_BYTES)
}

fn extend_response_body_with_limit(
    body: &mut Vec<u8>,
    chunk: &[u8],
    maximum: usize,
) -> TransportResult<()> {
    let new_length = ensure_response_growth_within_limit(body.len(), chunk.len(), maximum)?;
    // SYSCOIN: Reserve only the exact logical growth before extending. `Vec::extend_from_slice`
    // otherwise uses amortized growth and can make capacity exceed the advertised hard cap.
    if new_length > body.capacity() {
        body.try_reserve_exact(new_length - body.len())
            .map_err(TransportErrorKind::custom)?;
    }
    body.extend_from_slice(chunk);
    debug_assert!(body.capacity() <= maximum);
    Ok(())
}

fn ensure_response_growth_within_limit(
    current: usize,
    additional: usize,
    maximum: usize,
) -> TransportResult<usize> {
    let new_length = current
        .checked_add(additional)
        .ok_or_else(|| response_too_large_error(u64::MAX, maximum))?;
    if new_length > maximum {
        return Err(response_too_large_error(new_length as u64, maximum));
    }
    Ok(new_length)
}

fn response_too_large_error(observed: u64, maximum: usize) -> TransportError {
    TransportErrorKind::custom_str(&format!(
        "provider response is {observed} bytes; maximum is {maximum} bytes"
    ))
}

fn parse_response(
    status: StatusCode,
    body: &[u8],
    retry_after: Option<Duration>,
) -> TransportResult<ResponsePacket> {
    if !status.is_success() {
        if let Ok(response) = serde_json::from_slice::<ResponsePacket>(body)
            && response.is_error()
        {
            return Ok(bound_json_rpc_error_packet(response));
        }
        return Err(TransportErrorKind::http_error_with_retry_after(
            status.as_u16(),
            bounded_response_diagnostic(body),
            retry_after,
        ));
    }
    serde_json::from_slice(body)
        .map(bound_json_rpc_error_packet)
        .map_err(|error| TransportError::deser_err(error, bounded_response_diagnostic(body)))
}

// SYSCOIN: Alloy's retry layer clones the first JSON-RPC error and retains it across backoff after
// this transport releases its raw-body byte lease. Keep ordinary revert data intact, but cap the
// aggregate owned error message/data retained outside that lease. Retry classification depends on
// code/message; small `data` remains available for provider backoff hints, while an oversized hint
// is discarded and Alloy falls back to its configured retry delay.
fn bound_json_rpc_error_packet(mut packet: ResponsePacket) -> ResponsePacket {
    let mut remaining = MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES;
    match &mut packet {
        ResponsePacket::Single(response) => {
            bound_json_rpc_error_response(response, &mut remaining);
        }
        ResponsePacket::Batch(responses) => {
            for response in responses {
                bound_json_rpc_error_response(response, &mut remaining);
            }
        }
    }
    packet
}

fn bound_json_rpc_error_response(response: &mut Response, remaining: &mut usize) {
    let ResponsePayload::Failure(error) = &mut response.payload else {
        return;
    };

    if error.message.len() > *remaining {
        if *remaining == 0 {
            error.message = Cow::Borrowed("");
        } else {
            let marker_bytes = TRUNCATED_JSON_RPC_ERROR_MESSAGE.len().min(*remaining);
            let mut prefix_bytes = remaining.saturating_sub(marker_bytes);
            while !error.message.is_char_boundary(prefix_bytes) {
                prefix_bytes -= 1;
            }
            let mut message = error.message[..prefix_bytes].to_owned();
            message.push_str(&TRUNCATED_JSON_RPC_ERROR_MESSAGE[..marker_bytes]);
            error.message = Cow::Owned(message);
        }
        error.data = None;
        *remaining = 0;
        return;
    }

    *remaining -= error.message.len();
    if let Some(data) = error.data.as_ref() {
        let data_bytes = data.get().len();
        if data_bytes <= *remaining {
            *remaining -= data_bytes;
        } else {
            error.data = None;
        }
    }
}

fn bounded_response_diagnostic(body: &[u8]) -> String {
    let prefix_length = body.len().min(MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES);
    let mut diagnostic = String::from_utf8_lossy(&body[..prefix_length]).into_owned();
    let decoded_was_truncated = diagnostic.len() > MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES;
    if decoded_was_truncated {
        let mut boundary = MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES;
        while !diagnostic.is_char_boundary(boundary) {
            boundary -= 1;
        }
        diagnostic.truncate(boundary);
    }
    if body.len() > prefix_length || decoded_was_truncated {
        diagnostic.push_str(TRUNCATED_RESPONSE_DIAGNOSTIC);
    }
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::{
        BoundedHttpTransport, MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES, ProviderResponseByteBudget,
        TRUNCATED_JSON_RPC_ERROR_MESSAGE, TRUNCATED_RESPONSE_DIAGNOSTIC,
        bounded_response_diagnostic, ensure_content_length_within_limit_for,
        extend_response_body_with_limit, parse_response,
    };
    use alloy::rpc::json_rpc::{Id, Request, RequestPacket};
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        response::Response,
        routing::post,
    };
    use reqwest::StatusCode;
    use std::{convert::Infallible, sync::Arc, time::Duration};
    use tokio::{
        sync::{Barrier, Semaphore},
        task::JoinHandle,
    };
    use url::Url;

    #[test]
    fn content_length_and_streaming_limits_are_exact() {
        const TEST_LIMIT: usize = 4;

        assert!(ensure_content_length_within_limit_for(TEST_LIMIT as u64, TEST_LIMIT).is_ok());
        assert!(ensure_content_length_within_limit_for(TEST_LIMIT as u64 + 1, TEST_LIMIT).is_err());

        let mut body = vec![0; TEST_LIMIT - 1];
        extend_response_body_with_limit(&mut body, &[0], TEST_LIMIT).unwrap();
        assert!(body.capacity() <= TEST_LIMIT);
        assert!(extend_response_body_with_limit(&mut body, &[0], TEST_LIMIT).is_err());
        assert!(body.capacity() <= TEST_LIMIT);

        // Exercise a growth pattern that `Vec::extend_from_slice` would ordinarily amortize past
        // the limit even though the logical response remains below it.
        const AMORTIZED_GROWTH_LIMIT: usize = 16;
        let mut body = Vec::new();
        extend_response_body_with_limit(&mut body, &[0; 10], AMORTIZED_GROWTH_LIMIT).unwrap();
        extend_response_body_with_limit(&mut body, &[0; 5], AMORTIZED_GROWTH_LIMIT).unwrap();
        assert_eq!(body.len(), 15);
        assert!(body.capacity() <= AMORTIZED_GROWTH_LIMIT);
    }

    // SYSCOIN: Invalid bodies may be maximum-size attacker input, but their owned transport error
    // and eventual log representation retain only a small, explicitly marked diagnostic prefix.
    #[test]
    fn malformed_response_diagnostic_is_bounded_and_marked() {
        let body = vec![b'x'; MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES + 1];
        let diagnostic = bounded_response_diagnostic(&body);
        assert_eq!(
            diagnostic.len(),
            MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES + TRUNCATED_RESPONSE_DIAGNOSTIC.len()
        );
        assert!(diagnostic.ends_with(TRUNCATED_RESPONSE_DIAGNOSTIC));

        let invalid_utf8 = vec![0xff; MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES];
        let diagnostic = bounded_response_diagnostic(&invalid_utf8);
        assert!(
            diagnostic.len()
                <= MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES + TRUNCATED_RESPONSE_DIAGNOSTIC.len()
        );
        assert!(diagnostic.ends_with(TRUNCATED_RESPONSE_DIAGNOSTIC));

        let short = bounded_response_diagnostic(b"short invalid response");
        assert_eq!(short, "short invalid response");
    }

    // SYSCOIN: A syntactically valid JSON-RPC error must not carry a maximum-size message/data
    // outside the raw-body lease or through Alloy's retry clone. Preserve its retryable prefix.
    #[test]
    fn json_rpc_error_payload_is_bounded_before_transport_return() {
        let oversized_message = format!(
            "rate limit {}",
            "x".repeat(MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES)
        );
        let body = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": 429,
                "message": oversized_message,
                "data": "y".repeat(MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES),
            }
        }))
        .unwrap();

        for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let packet = parse_response(status, &body, None).unwrap();
            let error = packet.as_error().expect("response must remain an error");
            assert!(error.is_retry_err());
            assert!(error.message.len() <= MAX_PROVIDER_RESPONSE_DIAGNOSTIC_BYTES);
            assert!(error.message.ends_with(TRUNCATED_JSON_RPC_ERROR_MESSAGE));
            assert!(error.data.is_none());
        }
    }

    // SYSCOIN: Ordinary bounded data remains available both to contract-call error handling and
    // Alloy's provider-directed rate-limit backoff parser.
    #[test]
    fn small_json_rpc_error_data_is_preserved() {
        let body = br#"{"jsonrpc":"2.0","id":1,"error":{"code":429,"message":"rate limit","data":{"rate":{"backoff_seconds":7}}}}"#;
        let packet = parse_response(StatusCode::OK, body, None).unwrap();
        let error = packet.as_error().expect("response must remain an error");
        assert_eq!(error.message, "rate limit");
        assert_eq!(
            error.data.as_ref().unwrap().get(),
            "{\"rate\":{\"backoff_seconds\":7}}"
        );
    }

    // SYSCOIN: A streaming test peer makes chunk boundaries deterministic so concurrent responses
    // can exhaust the normal byte pool before either is allowed to send its tail.
    #[derive(Clone)]
    struct ChunkedResponseState {
        chunks: Arc<Vec<Bytes>>,
        before_tail: Option<Arc<Barrier>>,
        release_tail: Option<Arc<Semaphore>>,
    }

    async fn chunked_response(State(state): State<ChunkedResponseState>) -> Response<Body> {
        let stream = futures::stream::unfold((state, 0), |(state, index)| async move {
            if index == state.chunks.len() {
                return None;
            }
            if index == 1 {
                if let Some(before_tail) = &state.before_tail {
                    before_tail.wait().await;
                }
                if let Some(release_tail) = &state.release_tail {
                    let permit = release_tail.acquire().await.ok()?;
                    permit.forget();
                }
            }
            let chunk = state.chunks[index].clone();
            Some((Ok::<Bytes, Infallible>(chunk), (state, index + 1)))
        });
        Response::new(Body::from_stream(stream))
    }

    async fn spawn_chunked_server(state: ChunkedResponseState) -> (Url, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(chunked_response))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), server)
    }

    fn request_packet() -> RequestPacket {
        Request::new("test_method", Id::Number(1), ())
            .serialize()
            .unwrap()
            .into()
    }

    async fn wait_for_available_permits(budget: &ProviderResponseByteBudget, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.available_permits() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "response budget did not reach {expected} available bytes; observed {}",
                budget.available_permits()
            )
        });
    }

    // SYSCOIN: Two responses may each retain a partial normal-pool body. The exclusive full-size
    // completion reserve must let one finish and release its bytes, rather than deadlocking both.
    #[tokio::test]
    async fn aggregate_budget_allows_a_partial_response_to_complete() {
        const TEST_TOTAL_BYTES: usize = 128;
        const TEST_MAX_RESPONSE_BYTES: usize = 64;
        let before_tail = Arc::new(Barrier::new(3));
        let release_tail = Arc::new(Semaphore::new(0));
        let state = ChunkedResponseState {
            chunks: Arc::new(vec![
                Bytes::from_static(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\""),
                Bytes::from_static(b":true}"),
            ]),
            before_tail: Some(before_tail.clone()),
            release_tail: Some(release_tail.clone()),
        };
        let (url, server) = spawn_chunked_server(state).await;
        let budget =
            ProviderResponseByteBudget::with_limits(TEST_TOTAL_BYTES, TEST_MAX_RESPONSE_BYTES);
        let transport = BoundedHttpTransport::new(url, budget.clone()).unwrap();

        let first = tokio::spawn(transport.clone().execute(request_packet()));
        let second = tokio::spawn(transport.execute(request_packet()));
        tokio::time::timeout(Duration::from_secs(2), before_tail.wait())
            .await
            .expect("both responses did not retain their first chunk");
        // SYSCOIN: Hyper may poll the server stream's tail before reqwest has scheduled the
        // already-emitted prefix. Wait for both clients to account it before releasing the tail.
        wait_for_available_permits(&budget, TEST_MAX_RESPONSE_BYTES).await;

        release_tail.add_permits(2);
        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(first, second)
        })
        .await
        .expect("partial responses deadlocked while waiting for aggregate budget");
        assert!(first.unwrap().is_ok());
        assert!(second.unwrap().is_ok());
        assert_eq!(budget.available_permits(), TEST_TOTAL_BYTES);
        server.abort();
    }

    // SYSCOIN: A JSON parse error must drop every byte permit after the buffered body is rejected.
    #[tokio::test]
    async fn aggregate_budget_releases_permits_on_parse_error() {
        const TEST_TOTAL_BYTES: usize = 128;
        const TEST_MAX_RESPONSE_BYTES: usize = 64;
        let state = ChunkedResponseState {
            chunks: Arc::new(vec![
                Bytes::from_static(b"{\"jsonrpc\":"),
                Bytes::from_static(b"not-json}"),
            ]),
            before_tail: None,
            release_tail: None,
        };
        let (url, server) = spawn_chunked_server(state).await;
        let budget =
            ProviderResponseByteBudget::with_limits(TEST_TOTAL_BYTES, TEST_MAX_RESPONSE_BYTES);
        let transport = BoundedHttpTransport::new(url, budget.clone()).unwrap();

        assert!(transport.execute(request_packet()).await.is_err());
        assert_eq!(budget.available_permits(), TEST_TOTAL_BYTES);
        server.abort();
    }

    // SYSCOIN: Cancelling a response after its first chunk must release retained permits even
    // though the HTTP peer never sends the tail and no parse step runs.
    #[tokio::test]
    async fn aggregate_budget_releases_permits_on_cancellation() {
        const TEST_TOTAL_BYTES: usize = 128;
        const TEST_MAX_RESPONSE_BYTES: usize = 64;
        const PREFIX_BYTES: usize = 32;
        let before_tail = Arc::new(Barrier::new(2));
        let release_tail = Arc::new(Semaphore::new(0));
        let state = ChunkedResponseState {
            chunks: Arc::new(vec![
                Bytes::from_static(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\""),
                Bytes::from_static(b":true}"),
            ]),
            before_tail: Some(before_tail.clone()),
            release_tail: Some(release_tail),
        };
        let (url, server) = spawn_chunked_server(state).await;
        let budget =
            ProviderResponseByteBudget::with_limits(TEST_TOTAL_BYTES, TEST_MAX_RESPONSE_BYTES);
        let transport = BoundedHttpTransport::new(url, budget.clone()).unwrap();

        let request = tokio::spawn(transport.execute(request_packet()));
        tokio::time::timeout(Duration::from_secs(2), before_tail.wait())
            .await
            .expect("response did not retain its first chunk");
        wait_for_available_permits(&budget, TEST_TOTAL_BYTES - PREFIX_BYTES).await;

        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(budget.available_permits(), TEST_TOTAL_BYTES);
        server.abort();
    }
}
