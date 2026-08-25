use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose};
use http::{
    HeaderValue, StatusCode,
    header::{CACHE_CONTROL, CONTENT_TYPE, PRAGMA},
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zksync_os_batch_types::batcher_model::{FriProof, ProverInput};

use crate::prover_api::fri_job_manager::SubmitError;
use crate::prover_api::snark_job_manager::SnarkSubmitError;
use crate::prover_api::{
    MAX_FRI_PEEK_RESPONSE_BYTES, fri_input_fits_response_contract,
    metrics::{PROVER_API_METRICS, PickJobResult, ProverStage},
    prover_server::{
        AppState, PROVER_DISPOSITION_ACCEPTED, PROVER_DISPOSITION_HEADER,
        PROVER_DISPOSITION_REJECTED, PROVER_PICK_OUTCOME_HEADER, PROVER_PICK_OUTCOME_UNLEASED,
        v1::models::{
            BatchDataPayload, FailedProofResponse, FriProofPayload, NextSnarkProverJobPayload,
            PeekBatchDataPayload, PeekSnarkProverJobPayload, ProverQuery, SnarkProofPayload,
        },
    },
};
use serde::Deserialize;

/// Ensures `pick_job_latency` is recorded on all exit paths including cancellation.
struct PickJobGuard {
    stage: ProverStage,
    started: Instant,
    result: Option<PickJobResult>,
}

impl PickJobGuard {
    fn new(stage: ProverStage) -> Self {
        Self {
            stage,
            started: Instant::now(),
            result: None,
        }
    }

    fn finish(&mut self, result: PickJobResult) {
        self.result = Some(result);
    }

    fn record_after_write(self, response: Response) -> Response {
        response.map(|inner| {
            Body::new(GuardedBody {
                inner,
                payload_ready: Instant::now(),
                guard: self,
            })
        })
    }
}

impl Drop for PickJobGuard {
    fn drop(&mut self) {
        let result = self.result.unwrap_or(PickJobResult::Cancelled);
        PROVER_API_METRICS.pick_job_latency[&(self.stage, result)].observe(self.started.elapsed());
    }
}

struct GuardedBody {
    inner: Body,
    payload_ready: Instant,
    guard: PickJobGuard,
}

impl Drop for GuardedBody {
    fn drop(&mut self) {
        PROVER_API_METRICS.pick_job_transfer_latency[&self.guard.stage]
            .observe(self.payload_ready.elapsed());
    }
}

impl HttpBody for GuardedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    /// Never true: hyper releases a finished body once the last frame is buffered, before the write.
    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

// SYSCOIN: Keep each dedicated large-response slot until Hyper releases the complete response
// body. Releasing it when the handler returns would allow unbounded concurrent compressed writes.
struct PermitResponseBody {
    inner: Body,
    _permit: OwnedSemaphorePermit,
}

impl HttpBody for PermitResponseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        // SYSCOIN: Force Hyper to observe the final `None` after writing the last buffered frame;
        // otherwise a one-frame body can release its response-capacity permit before socket flush.
        false
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Ensures `submit_proof_latency` is recorded on all exit paths including early returns and cancellation.
struct SubmitProofGuard {
    stage: ProverStage,
    started: Instant,
}

impl SubmitProofGuard {
    fn new(stage: ProverStage) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }
}

impl Drop for SubmitProofGuard {
    fn drop(&mut self) {
        PROVER_API_METRICS.submit_proof_latency[&self.stage].observe(self.started.elapsed());
    }
}

// SYSCOIN: Pick responses contain a live bearer capability. Explicitly forbid storage by HTTP
// intermediaries even though these endpoints use POST and authenticated deployments use TLS.
fn capability_pick_response(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

// SYSCOIN: A duplicate carrying the exact current capability is not stale: the original owned
// task may still finish or release it after a transient failure. 425 tells the client to retain
// and replay the same proof instead of treating the overlap as a definitive conflict.
fn submission_in_progress_response(stage: &str) -> (StatusCode, String) {
    (
        StatusCode::TOO_EARLY,
        format!("{stage} submission for this lease is still in progress; retry later"),
    )
}

// SYSCOIN: Only these handler-generated responses prove that the exact-token manager accepted or
// definitively rejected the submission. Pre-handler JSON/body errors and intermediaries cannot
// forge durable retirement merely by choosing the same HTTP status.
fn terminal_submission_response(
    status: StatusCode,
    disposition: &'static str,
    message: impl Into<String>,
) -> Response {
    let mut response = if status == StatusCode::NO_CONTENT {
        status.into_response()
    } else {
        (status, message.into()).into_response()
    };
    response.headers_mut().insert(
        PROVER_DISPOSITION_HEADER,
        HeaderValue::from_static(disposition),
    );
    response
}

fn accepted_submission_response() -> Response {
    terminal_submission_response(
        StatusCode::NO_CONTENT,
        PROVER_DISPOSITION_ACCEPTED,
        String::new(),
    )
}

fn rejected_submission_response(status: StatusCode, message: impl Into<String>) -> Response {
    // SYSCOIN: Keep this exact matrix synchronized with the Airbender durable client. Refuse to
    // manufacture a terminal marker on a status the client must treat as ambiguous.
    assert!(matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::CONFLICT
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNPROCESSABLE_ENTITY
    ));
    terminal_submission_response(status, PROVER_DISPOSITION_REJECTED, message)
}

// SYSCOIN: Mark only handler outcomes proven to precede assignment. The narrow status allowlist
// prevents a future call site from making an ordinary no-job or post-lease failure retryable.
fn definitely_unleased_pick_response(status: StatusCode, message: impl Into<String>) -> Response {
    assert!(matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::INTERNAL_SERVER_ERROR
    ));
    let mut response = (status, message.into()).into_response();
    response.headers_mut().insert(
        PROVER_PICK_OUTCOME_HEADER,
        HeaderValue::from_static(PROVER_PICK_OUTCOME_UNLEASED),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn pick_fri_job(
    Query(query): Query<ProverQuery>,
    State(state): State<AppState>,
) -> Response {
    let mut guard = PickJobGuard::new(ProverStage::Fri);
    // SYSCOIN: Reserve the complete response lifetime before the manager clones and assigns a
    // potentially large witness. Three slots preserve the intended parallel FRI fleet topology.
    let Some(permit) = try_acquire_response_slot(&state.fri_pick_slots) else {
        guard.finish(PickJobResult::Error);
        return definitely_unleased_pick_response(
            StatusCode::TOO_MANY_REQUESTS,
            "FRI pick capacity is busy; retry later",
        );
    };
    tracing::trace!(
        "Received FRI job pick request from prover with ID: {}",
        query.id
    );
    // SYSCOIN: Clamp the worker's decompressed-body capacity to the trusted edge envelope, then
    // apply it inside the existing atomic queue predicate before any lease or witness clone.
    let maximum_response_bytes = query.fri_pick_response_capacity(state.max_fri_response_bytes);
    let supported_proving_versions = query.supported_proving_versions();
    // for real provers, we return the next job immediately -
    // see `FakeProversPool` for fake provers implementation
    match state
        .fri_job_manager
        .pick_next_job(
            std::time::Duration::from_secs(0),
            query.id,
            supported_proving_versions.as_deref(),
            maximum_response_bytes,
        )
        .await
    {
        Some(leased_job) => {
            // SYSCOIN: Only this claim response exposes the newly generated bearer capability.
            let fri_job = leased_job.job;
            let batch_number = fri_job.batch_number;
            let input = leased_job.data;
            let lease_token = leased_job.lease_token;
            let bytes: Vec<u8> = match &input {
                ProverInput::Real(words) => words.iter().flat_map(|v| v.to_le_bytes()).collect(),
                ProverInput::Fake => vec![],
            };
            let prover_input = general_purpose::STANDARD.encode(&bytes);
            // SYSCOIN: The pre-lease word/base64/framing predicate guarantees this infallible JSON
            // model fits the advertised scalar. Do not introduce a post-lease reject/revoke path.
            guard.finish(PickJobResult::NewJob);
            guard.record_after_write(retain_response_slot(
                capability_pick_response(Json(BatchDataPayload {
                    batch_number,
                    vk_hash: fri_job.vk_hash,
                    prover_input,
                    lease_token,
                })),
                permit,
            ))
        }
        None => {
            guard.finish(PickJobResult::NoJob);
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

pub(super) async fn submit_fri_proof(
    Query(query): Query<ProverQuery>,
    State(state): State<AppState>,
    Json(payload): Json<FriProofPayload>,
) -> Response {
    let _guard = SubmitProofGuard::new(ProverStage::Fri);
    tracing::debug!(
        "Received submit FRI proof request from prover with ID: {}",
        query.id
    );
    let prover_id = query.id;
    // SYSCOIN: After authenticated, 10 MiB-bounded HTTP/JSON extraction, the manager admits by
    // opaque token before VK/base64 allocation, proof deserialization, or verification.
    match state
        .fri_job_manager
        .submit_proof(
            payload.batch_number,
            payload.proof,
            payload.vk_hash,
            &prover_id,
            &payload.lease_token,
        )
        .await
    {
        Ok(()) => accepted_submission_response(),
        Err(SubmitError::UnknownVerificationKey(error)) => rejected_submission_response(
            StatusCode::BAD_REQUEST,
            format!("no Proving Version matches the provided Verification Key: {error}"),
        ),
        Err(SubmitError::ProvingVersionMismatch(
            server_execution_version,
            prover_execution_version,
        )) => rejected_submission_response(
            StatusCode::BAD_REQUEST,
            format!(
                "execution error mismatch: server has {server_execution_version:?} (vk = {}), prover used {prover_execution_version:?} (vk = {})",
                server_execution_version.vk_hash(),
                prover_execution_version.vk_hash()
            ),
        ),
        Err(SubmitError::FriProofVerificationError {
            expected_hash_u32s,
            proof_final_register_values,
        }) => rejected_submission_response(
            StatusCode::BAD_REQUEST,
            format!(
                "FRI proof verification failed. Expected: {expected_hash_u32s:?}, Got: {proof_final_register_values:?}"
            ),
        ),
        Err(SubmitError::UnknownJob(_)) => {
            // SYSCOIN: Durable clients recognize exact-capability terminal rejection only through
            // the narrow 400/409/413/422 matrix; an absent completed job is a stale lease conflict.
            rejected_submission_response(StatusCode::CONFLICT, "unknown or completed block")
        }
        // SYSCOIN: Keep definitive lease rejection distinct from every response that guarantees
        // the exact token remains current and can safely replay identical proof bytes.
        Err(SubmitError::InvalidLease) => {
            rejected_submission_response(StatusCode::CONFLICT, "invalid or stale prover lease")
        }
        Err(SubmitError::SubmissionInProgress) => {
            submission_in_progress_response("FRI").into_response()
        }
        Err(SubmitError::VerificationBusy | SubmitError::AcceptedProofBackpressure) => (
            StatusCode::TOO_MANY_REQUESTS,
            "FRI submission capacity is busy; retry this lease later",
        )
            .into_response(),
        Err(SubmitError::DeserializationFailed(err)) => {
            rejected_submission_response(StatusCode::BAD_REQUEST, err.to_string())
        }
        Err(SubmitError::InvalidBase64(err)) => {
            rejected_submission_response(StatusCode::BAD_REQUEST, format!("invalid base64: {err}"))
        }
        Err(SubmitError::InvalidProofShape(err)) => {
            rejected_submission_response(StatusCode::BAD_REQUEST, err)
        }
        Err(SubmitError::ShuttingDown) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "server is shutting down; retry this lease later",
        )
            .into_response(),
        Err(SubmitError::TemporaryStorage(error)) => {
            tracing::error!(%error, "temporary FRI proof persistence failure");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "FRI proof storage is temporarily unavailable; retry this lease later",
            )
                .into_response()
        }
        // SYSCOIN: These failures either retain the exact token or are ambiguous after durable
        // completion. A 503 makes the client retry identical bytes; it then succeeds or safely
        // converges to definitive 409 if exact completion already consumed the capability.
        Err(SubmitError::TemporaryInternal(e) | SubmitError::AmbiguousHandoff(e)) => {
            tracing::error!("retryable/ambiguous internal FRI submission error: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "FRI submission hit a temporary internal error; retry this lease later",
            )
                .into_response()
        }
    }
}

pub(super) async fn pick_snark_job(
    Query(query): Query<ProverQuery>,
    State(state): State<AppState>,
) -> Response {
    let mut guard = PickJobGuard::new(ProverStage::Snark);
    // SYSCOIN: Admit before manager assignment or any aggregate clone/encoding. A 429 therefore
    // transfers no lease, while a successful permit remains owned through the trusted proxy drain.
    let Some(permit) = try_acquire_response_slot(&state.snark_pick_slots) else {
        guard.finish(PickJobResult::Error);
        return definitely_unleased_pick_response(
            StatusCode::TOO_MANY_REQUESTS,
            "SNARK pick capacity is busy; retry later",
        );
    };
    tracing::trace!(
        "Received SNARK job pick request from prover with ID: {}",
        query.id
    );
    let supported_proving_versions = query.supported_proving_versions();
    match state
        .snark_job_manager
        .pick_real_job(query.id, supported_proving_versions.as_deref())
        .await
    {
        Ok(Some(leased_job)) => {
            // SYSCOIN: One pick-only token authorizes precisely this returned aggregate.
            let batches = leased_job.batches;
            // Expect non-empty and all real FRI proofs
            let from = batches.first().unwrap().0.batch_number;
            let to = batches.last().unwrap().0.batch_number;
            let vk_hash = batches.first().unwrap().0.vk_hash.clone();

            let expected_proof_count = batches.len();
            let mut fri_proofs = Vec::with_capacity(expected_proof_count);
            for (fri_job, proof) in batches {
                match proof {
                    FriProof::Real(real) => {
                        fri_proofs.push(general_purpose::STANDARD.encode(real.proof()));
                    }
                    FriProof::Fake => {
                        // Should never happen; defensive guard
                        tracing::error!(
                            "SNARK pick returned fake FRI at batch {} (range {}-{})",
                            fri_job.batch_number,
                            from,
                            to
                        );
                        guard.finish(PickJobResult::Error);
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                    FriProof::AlreadySubmittedToL1 => {
                        tracing::warn!(
                            "SNARK pick returned already submitted to L1 FRI at batch {} (range {}-{})",
                            fri_job.batch_number,
                            from,
                            to
                        );
                        guard.finish(PickJobResult::Error);
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }
            }
            // SYSCOIN: A leased consecutive range must serialize every proof exactly once; never
            // emit a holey aggregate that the client can only discover after large allocations.
            if fri_proofs.len() != expected_proof_count {
                guard.finish(PickJobResult::Error);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            guard.finish(PickJobResult::NewJob);
            guard.record_after_write(retain_response_slot(
                capability_pick_response(Json(NextSnarkProverJobPayload {
                    from_batch_number: from,
                    to_batch_number: to,
                    vk_hash,
                    fri_proofs,
                    lease_token: leased_job.lease_token,
                })),
                permit,
            ))
        }
        Ok(None) => {
            guard.finish(PickJobResult::NoJob);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!("error picking SNARK job: {e}");
            guard.finish(PickJobResult::Error);
            // SYSCOIN: Every manager error exits before `SnarkJobPick::Assigned`, so no external
            // lease or bearer capability exists and another endpoint may safely service the pick.
            definitely_unleased_pick_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to pick SNARK job before lease assignment",
            )
        }
    }
}

pub(super) async fn submit_snark_proof(
    Query(query): Query<ProverQuery>,
    State(state): State<AppState>,
    Json(payload): Json<SnarkProofPayload>,
) -> Response {
    let _guard = SubmitProofGuard::new(ProverStage::Snark);
    tracing::debug!(
        "Received submit SNARK proof request from prover with ID: {}",
        query.id
    );
    match state
        .snark_job_manager
        // SYSCOIN: Public query ID remains diagnostic; payload capability is authoritative. Keep
        // encoded proof/version validation behind the manager's atomic exact-token admission.
        .submit_proof(
            payload.from_batch_number,
            payload.to_batch_number,
            payload.vk_hash,
            payload.proof,
            query.id,
            payload.lease_token,
        )
        .await
    {
        Ok(()) => accepted_submission_response(),
        // SYSCOIN: Capacity is retryable with the same token/proof; stale authority and shutdown
        // are distinct from definitive malformed/verification rejection.
        Err(SnarkSubmitError::DownstreamBackpressure) => (
            StatusCode::TOO_MANY_REQUESTS,
            "SNARK downstream capacity is busy; retry this lease later",
        )
            .into_response(),
        Err(SnarkSubmitError::ShuttingDown) => {
            (StatusCode::SERVICE_UNAVAILABLE, "server is shutting down").into_response()
        }
        // SYSCOIN: Disk-full/fsync/snapshot failures leave both the exact lease and wrapper
        // ownership live. No terminal disposition marker is emitted, so clients replay identically.
        Err(SnarkSubmitError::DurableJournal(error)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("durable SNARK handoff unavailable; retry this lease later: {error}"),
        )
            .into_response(),
        // SYSCOIN: RPC, topology, timeout, and reorg ambiguity keeps the exact lease live. The
        // client must retry identical bytes and must not write a terminal disposition marker.
        Err(SnarkSubmitError::VerifierPreflightUnavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "settlement verifier preflight unavailable; retry this lease later",
        )
            .into_response(),
        // SYSCOIN: Only a canonical fixed-block false result or data-bearing EVM revert reaches
        // this branch, so the capability is definitively retired and 422 is safe to persist.
        Err(SnarkSubmitError::ProofRejected) => rejected_submission_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "settlement verifier rejected the SNARK proof",
        ),
        Err(SnarkSubmitError::InvalidLease) => {
            rejected_submission_response(StatusCode::CONFLICT, "invalid or stale SNARK lease")
        }
        Err(SnarkSubmitError::SubmissionInProgress) => {
            submission_in_progress_response("SNARK").into_response()
        }
        // SYSCOIN: InvalidRange occurs before exact-token admission, so it cannot authorize
        // durable retirement. Every other malformed-owner error is emitted only after admission.
        Err(SnarkSubmitError::InvalidRange { from, to }) => (
            StatusCode::BAD_REQUEST,
            format!("invalid batch range: from batch {from} is greater than to batch {to}"),
        )
            .into_response(),
        Err(err) => {
            rejected_submission_response(StatusCode::BAD_REQUEST, format!("proof rejected: {err}"))
        }
    }
}

pub(super) async fn peek_fri_job(
    Path(batch_number): Path<u64>,
    State(state): State<AppState>,
) -> Response {
    // SYSCOIN: Admission precedes the manager clone, endian expansion, base64 allocation, JSON
    // serialization, compression, and response transfer for an end-to-end per-process bound.
    let Some(permit) = try_acquire_response_slot(&state.fri_peek_slots) else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "FRI peek capacity is busy; retry later",
        )
            .into_response();
    };
    match state.fri_job_manager.peek_batch_data(batch_number).await {
        Some((vk_hash, prover_input)) => {
            if !fri_input_fits_response_contract(&prover_input, MAX_FRI_PEEK_RESPONSE_BYTES) {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "FRI peek response exceeds {} bytes",
                        MAX_FRI_PEEK_RESPONSE_BYTES
                    ),
                )
                    .into_response();
            }
            let bytes: Vec<u8> = match &prover_input {
                ProverInput::Real(words) => words.iter().flat_map(|v| v.to_le_bytes()).collect(),
                ProverInput::Fake => vec![],
            };
            // SYSCOIN: Peek uses a distinct tokenless model and cannot be upgraded into authority.
            match bounded_fri_response(
                PeekBatchDataPayload {
                    batch_number,
                    vk_hash: vk_hash.to_string(),
                    prover_input: general_purpose::STANDARD.encode(&bytes),
                },
                MAX_FRI_PEEK_RESPONSE_BYTES,
                permit,
            ) {
                Ok(response) => response,
                Err(status) => status.into_response(),
            }
        }
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

// SYSCOIN: FRI pick, SNARK pick, FRI peek, and SNARK peek use independent lanes separate from
// proof uploads. A saturated large-response class therefore cannot consume another class's capacity.
fn try_acquire_response_slot(slots: &std::sync::Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    std::sync::Arc::clone(slots).try_acquire_owned().ok()
}

// SYSCOIN: Transfer an admitted large-response slot into the body shared by FRI and SNARK lanes.
fn retain_response_slot(response: Response, permit: OwnedSemaphorePermit) -> Response {
    response.map(|inner| {
        Body::new(PermitResponseBody {
            inner,
            _permit: permit,
        })
    })
}

// SYSCOIN: Serialize exactly once, enforce the complete body size, and transfer ownership of the
// admission permit into the HTTP body so every success, oversize, and disconnect releases it.
fn bounded_fri_response(
    payload: impl serde::Serialize,
    maximum: usize,
    permit: OwnedSemaphorePermit,
) -> Result<Response, StatusCode> {
    let bytes = match serde_json::to_vec(&payload) {
        Ok(bytes) if bytes.len() <= maximum => bytes,
        Ok(_) => return Err(StatusCode::PAYLOAD_TOO_LARGE),
        Err(error) => {
            tracing::error!(?error, "failed to serialize bounded FRI response");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(retain_response_slot(response, permit))
}

// SYSCOIN: Inclusive range arithmetic is attacker-controlled on the diagnostic route; reject
// overflow before the loop and apply the configured aggregate count separately.
fn snark_peek_batch_count(from_batch_number: u64, to_batch_number: u64) -> Option<u64> {
    to_batch_number
        .checked_sub(from_batch_number)?
        .checked_add(1)
}

// SYSCOIN: Bound the actual base64 proof strings without overflow before allocating the next one.
fn extend_snark_peek_encoded_bytes(
    current: usize,
    raw_proof_bytes: usize,
    maximum: usize,
) -> Option<usize> {
    let encoded = raw_proof_bytes
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?;
    let total = current.checked_add(encoded)?;
    (total <= maximum).then_some(total)
}

// SYSCOIN: A retained marker is never a proof and must make the aggregate debug response fail
// atomically rather than silently returning a holey range.
fn already_submitted_peek_response(batch_number: u64) -> Response {
    (
        StatusCode::CONFLICT,
        format!("FRI proof for batch {batch_number} was already submitted to L1"),
    )
        .into_response()
}

pub(super) async fn peek_snark_job(
    Path((from_batch_number, to_batch_number)): Path<(u64, u64)>,
    State(state): State<AppState>,
) -> Response {
    if from_batch_number > to_batch_number {
        return (
            StatusCode::BAD_REQUEST,
            format!("Invalid range: from_batch_number ({from_batch_number}) must be <= to_batch_number ({to_batch_number})")
        ).into_response();
    }

    // SYSCOIN: Diagnostic peeks must never bypass the configured production aggregate-count
    // bound. Checked arithmetic also rejects the full-u64 inclusive range without wrapping.
    let Some(requested_count) = snark_peek_batch_count(from_batch_number, to_batch_number) else {
        return (StatusCode::BAD_REQUEST, "SNARK peek range is too large").into_response();
    };
    if requested_count > state.max_fris_per_snark as u64 {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "SNARK peek range contains {requested_count} batches; maximum is {}",
                state.max_fris_per_snark
            ),
        )
            .into_response();
    }

    // SYSCOIN: A tokenless diagnostic can repeatedly clone and base64-encode the same aggregate.
    // Admit one before any proof read/allocation and retain it through the trusted proxy drain.
    let Some(permit) = try_acquire_response_slot(&state.snark_peek_slots) else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "SNARK peek capacity is busy; retry later",
        )
            .into_response();
    };

    let mut fri_proofs = vec![];
    let mut vk_hash = String::new();
    let mut encoded_proof_bytes = 0_usize;
    for batch_number in from_batch_number..=to_batch_number {
        match state.proof_storage.get_batch_with_proof(batch_number).await {
            Ok(Some(env)) => {
                vk_hash = env
                    .batch
                    .verification_key_hash()
                    .expect("VK must exist")
                    .to_string();
                match env.data {
                    FriProof::Real(real) => {
                        // SYSCOIN: Bound the complete base64 response before allocating each next
                        // string; JSON framing is small and count-bounded separately above.
                        let Some(next_encoded_proof_bytes) = extend_snark_peek_encoded_bytes(
                            encoded_proof_bytes,
                            real.proof().len(),
                            state.max_snark_peek_proof_bytes,
                        ) else {
                            return (
                                StatusCode::PAYLOAD_TOO_LARGE,
                                format!(
                                    "SNARK peek proof payload exceeds {} bytes",
                                    state.max_snark_peek_proof_bytes
                                ),
                            )
                                .into_response();
                        };
                        encoded_proof_bytes = next_encoded_proof_bytes;
                        fri_proofs.push(general_purpose::STANDARD.encode(real.proof()))
                    }
                    FriProof::Fake => {
                        tracing::info!(
                            "Requested FRI proof for batch {} is fake (range {}-{})",
                            batch_number,
                            from_batch_number,
                            to_batch_number
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("FRI proof for batch {batch_number} is fake"),
                        )
                            .into_response();
                    }
                    FriProof::AlreadySubmittedToL1 => {
                        // SYSCOIN: Never return a partial aggregate with an omitted marker; callers
                        // could otherwise mistake a holey debug response for the requested range.
                        tracing::warn!(
                            "Requested FRI proof for batch {} is already submitted to L1 (range {}-{})",
                            batch_number,
                            from_batch_number,
                            to_batch_number
                        );
                        return already_submitted_peek_response(batch_number);
                    }
                };
            }
            Ok(None) => {
                tracing::info!(
                    "No FRI proof found for batch {batch_number} (range {}-{})",
                    from_batch_number,
                    to_batch_number
                );
                return (
                    StatusCode::NOT_FOUND,
                    format!("No FRI proof found for batch {batch_number}"),
                )
                    .into_response();
            }
            Err(e) => {
                tracing::info!("Error retrieving FRI proof for batch {batch_number}: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Error retrieving proof: {e}"),
                )
                    .into_response();
            }
        }
    }
    // SYSCOIN: Aggregate peek likewise omits the live capability returned only by pick.
    retain_response_slot(
        Json(PeekSnarkProverJobPayload {
            from_batch_number,
            to_batch_number,
            vk_hash,
            fri_proofs,
        })
        .into_response(),
        permit,
    )
}
// SYSCOIN: Expose each proving stage independently for remote fleet monitoring.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum StatusStage {
    Fri,
    Snark,
}

pub(super) async fn status(
    Path(stage): Path<StatusStage>,
    State(state): State<AppState>,
) -> Response {
    let status = match stage {
        StatusStage::Fri => state.fri_job_manager.status().await,
        StatusStage::Snark => state.snark_job_manager.status().await,
    };
    Json(status).into_response()
}

pub(super) async fn status_default(State(state): State<AppState>) -> Response {
    Json(state.fri_job_manager.status().await).into_response()
}

/// Get detailed information about a failed FRI proof for debugging.
/// Returns the most recent failed proof for the given batch number.
pub(super) async fn get_failed_fri_proof(
    Path(batch_number): Path<u64>,
    State(state): State<AppState>,
) -> Response {
    match state.proof_storage.get_failed_proof(batch_number).await {
        Ok(Some(failed_proof)) => {
            let response = FailedProofResponse {
                batch_number: failed_proof.batch_number,
                last_batch_timestamp: failed_proof.last_block_timestamp,
                expected_hash_u32s: failed_proof.expected_hash_u32s,
                proof_final_register_values: failed_proof.proof_final_register_values,
                vk_hash: failed_proof.vk_hash,
                proof: general_purpose::STANDARD.encode(failed_proof.proof_bytes),
            };

            Json(response).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("No failed proof found for batch {batch_number}"),
        )
            .into_response(),
        Err(e) => {
            tracing::info!("Error retrieving failed proof for batch {batch_number}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error retrieving failed proof: {e}"),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::Request,
        http::header::{ACCEPT_ENCODING, CONTENT_ENCODING},
        routing::get,
    };
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    use tower_http::compression::CompressionLayer;

    #[test]
    fn bearer_capability_responses_are_never_cacheable() {
        let response = capability_pick_response(StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[PRAGMA], "no-cache");
    }

    #[test]
    fn overlapping_exact_capability_is_retryable_not_conflict() {
        for stage in ["FRI", "SNARK"] {
            let (status, message) = submission_in_progress_response(stage);
            assert_eq!(status, StatusCode::TOO_EARLY);
            assert!(message.contains("retry"));
        }
    }

    // SYSCOIN: Durable provers retire proof ownership only on this exact application marker;
    // retryable manager responses and pre-handler errors deliberately carry no disposition.
    #[test]
    fn terminal_submission_responses_are_explicitly_marked() {
        let accepted = accepted_submission_response();
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            accepted.headers()[PROVER_DISPOSITION_HEADER],
            PROVER_DISPOSITION_ACCEPTED
        );

        let rejected = rejected_submission_response(StatusCode::CONFLICT, "stale lease");
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(
            rejected.headers()[PROVER_DISPOSITION_HEADER],
            PROVER_DISPOSITION_REJECTED
        );

        let retryable = submission_in_progress_response("FRI").into_response();
        assert!(!retryable.headers().contains_key(PROVER_DISPOSITION_HEADER));

        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::CONFLICT,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert_eq!(
                rejected_submission_response(status, "terminal").status(),
                status
            );
        }
    }

    // SYSCOIN: Only the exact pre-lease application helper emits the cross-endpoint retry marker;
    // ordinary no-job and indistinguishable post-lease failures remain deliberately unmarked.
    #[test]
    fn definitely_unleased_pick_responses_are_narrowly_marked() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let response = definitely_unleased_pick_response(status, "retry elsewhere");
            assert_eq!(response.status(), status);
            assert_eq!(
                response.headers()[PROVER_PICK_OUTCOME_HEADER],
                PROVER_PICK_OUTCOME_UNLEASED
            );
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        }

        let no_job = StatusCode::NO_CONTENT.into_response();
        assert!(!no_job.headers().contains_key(PROVER_PICK_OUTCOME_HEADER));
        let post_lease_failure = StatusCode::INTERNAL_SERVER_ERROR.into_response();
        assert!(
            !post_lease_failure
                .headers()
                .contains_key(PROVER_PICK_OUTCOME_HEADER)
        );
    }

    // SYSCOIN: The helper must fail closed if a future caller attempts to mark a new status.
    #[test]
    #[should_panic]
    fn definitely_unleased_pick_response_rejects_unreviewed_status() {
        let _ = definitely_unleased_pick_response(StatusCode::NO_CONTENT, "no job");
    }

    // SYSCOIN: Full-u64 and over-configured diagnostic ranges fail before storage iteration.
    #[test]
    fn snark_peek_count_is_inclusive_and_overflow_safe() {
        assert_eq!(snark_peek_batch_count(7, 7), Some(1));
        assert_eq!(snark_peek_batch_count(7, 9), Some(3));
        assert_eq!(snark_peek_batch_count(9, 7), None);
        assert_eq!(snark_peek_batch_count(0, u64::MAX), None);
    }

    // SYSCOIN: The encoded aggregate cap fails closed both at its boundary and on arithmetic overflow.
    #[test]
    fn snark_peek_encoded_byte_cap_is_exact_and_overflow_safe() {
        assert_eq!(extend_snark_peek_encoded_bytes(0, 3, 4), Some(4));
        assert_eq!(extend_snark_peek_encoded_bytes(4, 1, 8), Some(8));
        assert_eq!(extend_snark_peek_encoded_bytes(4, 1, 7), None);
        assert_eq!(
            extend_snark_peek_encoded_bytes(usize::MAX, 1, usize::MAX),
            None
        );
        assert_eq!(
            extend_snark_peek_encoded_bytes(0, usize::MAX, usize::MAX),
            None
        );
    }

    // SYSCOIN: Already-submitted markers cannot be mistaken for a complete aggregate proof set.
    #[test]
    fn snark_peek_rejects_already_submitted_marker() {
        assert_eq!(
            already_submitted_peek_response(42).status(),
            StatusCode::CONFLICT
        );
    }

    // SYSCOIN: The tokenless FRI diagnostic lane has exactly the configured independent
    // concurrency and releases a rejected/abandoned admission without touching upload slots.
    #[test]
    fn fri_peek_admission_is_bounded_and_released() {
        let slots = std::sync::Arc::new(Semaphore::new(1));
        let permit = try_acquire_response_slot(&slots).expect("first peek must acquire the slot");
        assert!(try_acquire_response_slot(&slots).is_none());
        drop(permit);
        assert!(try_acquire_response_slot(&slots).is_some());
    }

    // SYSCOIN: The pick lane preserves three independent FRI transfers and rejects a fourth
    // before any queued witness is cloned or assigned.
    #[test]
    fn fri_pick_admission_preserves_three_way_distribution() {
        let slots = std::sync::Arc::new(Semaphore::new(3));
        let permits: Vec<_> = (0..3)
            .map(|_| try_acquire_response_slot(&slots).unwrap())
            .collect();
        assert!(try_acquire_response_slot(&slots).is_none());
        drop(permits);
        assert!(try_acquire_response_slot(&slots).is_some());
    }

    // SYSCOIN: The authenticated SNARK pick lane serializes only response delivery; its permit is
    // independent from the three FRI pick slots and releases without assigning hidden work.
    #[test]
    fn snark_pick_admission_is_one_wide_and_independent() {
        let snark_slots = std::sync::Arc::new(Semaphore::new(1));
        let fri_slots = std::sync::Arc::new(Semaphore::new(3));
        let snark_permit =
            try_acquire_response_slot(&snark_slots).expect("first SNARK pick must acquire");
        assert!(try_acquire_response_slot(&snark_slots).is_none());

        let _fri_permits: Vec<_> = (0..3)
            .map(|_| try_acquire_response_slot(&fri_slots).unwrap())
            .collect();
        drop(snark_permit);
        assert!(try_acquire_response_slot(&snark_slots).is_some());
    }

    // SYSCOIN: Reject the encoded witness before byte/base64 expansion under the synchronized
    // server/prover contract, including conservative complete-JSON framing.
    #[test]
    fn fri_response_contract_includes_maximal_json_framing() {
        assert!(fri_input_fits_response_contract(
            &ProverInput::Fake,
            MAX_FRI_PEEK_RESPONSE_BYTES,
        ));
        let framing = serde_json::to_vec(&BatchDataPayload {
            batch_number: u64::MAX,
            vk_hash: format!("0x{}", "f".repeat(64)),
            prover_input: String::new(),
            lease_token: format!("0x{}", "e".repeat(64)),
        })
        .unwrap()
        .len();
        assert!(framing <= crate::prover_api::FRI_PICK_RESPONSE_FRAMING_BYTES);
    }

    // SYSCOIN: A successful response owns the permit through body consumption, while an exact
    // oversize rejection releases it synchronously for the next authenticated diagnostic.
    #[tokio::test]
    async fn fri_peek_response_holds_and_releases_permit_on_every_path() {
        let slots = std::sync::Arc::new(Semaphore::new(1));
        let payload = || PeekBatchDataPayload {
            batch_number: 1,
            vk_hash: "0x01".to_owned(),
            prover_input: "AQID".to_owned(),
        };

        let response =
            bounded_fri_response(payload(), 1_024, try_acquire_response_slot(&slots).unwrap())
                .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(try_acquire_response_slot(&slots).is_none());
        let mut body = response.into_body();
        let frame = body
            .frame()
            .await
            .expect("bounded response must have one data frame")
            .unwrap();
        assert!(!frame.into_data().unwrap().is_empty());
        assert!(!body.is_end_stream());
        assert!(try_acquire_response_slot(&slots).is_none());
        drop(body);
        assert!(try_acquire_response_slot(&slots).is_some());

        let response =
            bounded_fri_response(payload(), 1, try_acquire_response_slot(&slots).unwrap());
        assert_eq!(response.unwrap_err(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(try_acquire_response_slot(&slots).is_some());
    }

    // SYSCOIN: The independent SNARK diagnostic lane retains its sole permit after JSON is ready
    // and until the trusted transport releases the response body.
    #[tokio::test]
    async fn snark_peek_response_holds_permit_through_body_release() {
        let slots = std::sync::Arc::new(Semaphore::new(1));
        let response = retain_response_slot(
            Json(PeekSnarkProverJobPayload {
                from_batch_number: 1,
                to_batch_number: 2,
                vk_hash: "0x01".to_owned(),
                fri_proofs: vec!["AQID".to_owned(), "BAUG".to_owned()],
            })
            .into_response(),
            try_acquire_response_slot(&slots).unwrap(),
        );
        assert!(try_acquire_response_slot(&slots).is_none());
        let mut body = response.into_body();
        assert!(
            !body
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap()
                .is_empty()
        );
        assert!(!body.is_end_stream());
        assert!(try_acquire_response_slot(&slots).is_none());
        drop(body);
        assert!(try_acquire_response_slot(&slots).is_some());
    }

    // SYSCOIN: Tower compression owns the retained SNARK-pick body until Hyper releases it. Even
    // after compressed EOF is observed, the lane cannot reopen before that body is dropped.
    #[tokio::test]
    async fn snark_pick_response_holds_permit_through_compression_body_drop() {
        let slots = std::sync::Arc::new(Semaphore::new(1));
        let handler_slots = slots.clone();
        let app = Router::new()
            .route(
                "/",
                get(move || {
                    let handler_slots = handler_slots.clone();
                    async move {
                        retain_response_slot(
                            capability_pick_response(Json(NextSnarkProverJobPayload {
                                from_batch_number: 1,
                                to_batch_number: 2,
                                vk_hash: "0x01".to_owned(),
                                fri_proofs: vec!["AQID".repeat(1_024), "BAUG".repeat(1_024)],
                                lease_token: format!("0x{}", "e".repeat(64)),
                            })),
                            try_acquire_response_slot(&handler_slots).unwrap(),
                        )
                    }
                }),
            )
            .layer(CompressionLayer::new());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[CONTENT_ENCODING], "gzip");
        assert!(try_acquire_response_slot(&slots).is_none());

        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            assert!(frame.unwrap().data_ref().is_some());
        }
        assert!(try_acquire_response_slot(&slots).is_none());
        drop(body);
        assert!(try_acquire_response_slot(&slots).is_some());
    }
}
