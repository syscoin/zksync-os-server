//! Prover server module for handling proof generation requests.
//!
//! This module provides an HTTP server that manages proof generation jobs
//! and proof storage.
mod v1;

use std::{sync::Arc, time::Duration};

use crate::prover_api::{
    fri_job_manager::FriJobManager, proof_storage::ProofStorage, prover_server::v1::v1_routes,
    snark_job_manager::SnarkJobManager,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use http_body_util::LengthLimitError;
use reth_tasks::shutdown::GracefulShutdown;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use tokio::{net::TcpListener, sync::Semaphore};
use tower_http::compression::CompressionLayer;

// SYSCOIN: Bound authenticated proof-body buffering to the production topology without allowing
// one stage to starve the other. Pick/status/debug requests do not consume these slots.
const FRI_SUBMISSION_CONCURRENCY: usize = 3;
const SNARK_SUBMISSION_CONCURRENCY: usize = 1;
const PROVER_SUBMISSION_BODY_LIMIT: usize = 10 * 1024 * 1024;
// SYSCOIN: Give a remote prover two minutes to transfer at most 10 MiB, while imposing a total
// deadline independent of nginx's recommended matching `client_body_timeout 120s` backstop.
const PROVER_SUBMISSION_BODY_TIMEOUT: Duration = Duration::from_secs(120);
// SYSCOIN: A durable client may retire proof ownership only when this application—not a proxy,
// parser, or body-admission layer—reports an exact manager disposition.
pub(super) const PROVER_DISPOSITION_HEADER: &str = "x-syscoin-prover-disposition";
pub(super) const PROVER_DISPOSITION_ACCEPTED: &str = "accepted";
pub(super) const PROVER_DISPOSITION_REJECTED: &str = "rejected";
// SYSCOIN: A diagnostic aggregate response must remain a small fraction of a 64-GiB production
// host after base64/JSON duplication. Normal authenticated SNARK pick policy remains unchanged.
const MAX_SNARK_PEEK_PROOF_BYTES: usize = 256 * 1024 * 1024;

/// Application state shared across all request handlers.
#[derive(Clone)]
pub(in crate::prover_api::prover_server) struct AppState {
    fri_job_manager: Arc<FriJobManager>,
    snark_job_manager: Arc<SnarkJobManager>,
    proof_storage: ProofStorage,
    max_fris_per_snark: usize,
    max_snark_peek_proof_bytes: usize,
}

// SYSCOIN: Remote provers authenticate using the Basic Auth header already supported by
// `SequencerProofClient` when credentials are embedded in the prover API URL.
#[derive(Clone)]
struct ProverApiAuth {
    expected_authorization_hash: [u8; 32],
}

// SYSCOIN: Hash both operands to a fixed width before constant-time comparison. The supplied
// header length is public to its sender, while credential length and mismatch position stay hidden.
fn authorization_hash(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

async fn require_basic_auth(
    State(auth): State<ProverApiAuth>,
    request: Request,
    next: Next,
) -> Response {
    // SYSCOIN: Basic Auth is a reusable prover capability. Avoid data-dependent mismatch exits.
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .is_some_and(|value| {
            bool::from(
                authorization_hash(value.as_bytes()).ct_eq(&auth.expected_authorization_hash),
            )
        });

    if !authorized {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"prover-api\""
                .parse()
                .expect("static WWW-Authenticate header is valid"),
        );
        return response;
    }

    next.run(request).await
}

#[derive(Clone)]
struct SubmissionAdmission {
    fri_slots: Arc<Semaphore>,
    snark_slots: Arc<Semaphore>,
}

#[derive(Clone, Copy)]
enum SubmissionAdmissionStage {
    Fri,
    Snark,
}

impl SubmissionAdmission {
    fn try_acquire(
        &self,
        stage: SubmissionAdmissionStage,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        let semaphore = match stage {
            SubmissionAdmissionStage::Fri => &self.fri_slots,
            SubmissionAdmissionStage::Snark => &self.snark_slots,
        };
        Arc::clone(semaphore).try_acquire_owned()
    }
}

fn proof_submission_stage(request: &Request) -> Option<SubmissionAdmissionStage> {
    let path = request.uri().path();
    if request.method() != Method::POST {
        None
    } else if path.ends_with("/FRI/submit") {
        Some(SubmissionAdmissionStage::Fri)
    } else if path.ends_with("/SNARK/submit") {
        Some(SubmissionAdmissionStage::Snark)
    } else {
        None
    }
}

// SYSCOIN: Authenticate first, then reserve one bounded body/handler slot and completely buffer a
// submission under a total deadline. Stale-token floods therefore cannot allocate arbitrary JSON
// bodies or hold sockets indefinitely before the managers perform exact-token admission.
async fn admit_proof_submission(
    State(admission): State<SubmissionAdmission>,
    request: Request,
    next: Next,
) -> Response {
    let Some(stage) = proof_submission_stage(&request) else {
        return next.run(request).await;
    };

    let Ok(_permit) = admission.try_acquire(stage) else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "prover submission capacity is busy; retry the same lease later",
        )
            .into_response();
    };

    let (parts, body) = request.into_parts();
    let bytes = match buffer_submission_body(
        body,
        PROVER_SUBMISSION_BODY_LIMIT,
        PROVER_SUBMISSION_BODY_TIMEOUT,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(status) => return status.into_response(),
    };
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

// SYSCOIN: Keep both byte and wall-clock admission bounds directly regression-testable; neither
// trusts Content-Length and the timer covers the complete authenticated body stream.
async fn buffer_submission_body(
    body: Body,
    limit: usize,
    deadline: Duration,
) -> Result<axum::body::Bytes, StatusCode> {
    match tokio::time::timeout(deadline, to_bytes(body, limit)).await {
        Ok(Ok(bytes)) => Ok(bytes),
        // SYSCOIN: Only the limiter itself proves a permanent oversize body. An interrupted body
        // stream is ambiguous and must retain the exact proof capability for a safe retry.
        Ok(Err(error)) => {
            if error.into_inner().is::<LengthLimitError>() {
                Err(StatusCode::PAYLOAD_TOO_LARGE)
            } else {
                Err(StatusCode::REQUEST_TIMEOUT)
            }
        }
        Err(_) => Err(StatusCode::REQUEST_TIMEOUT),
    }
}

/// Runs the prover API HTTP server on a pre-bound listener.
pub async fn run(
    fri_job_manager: Arc<FriJobManager>,
    snark_job_manager: Arc<SnarkJobManager>,
    proof_storage: ProofStorage,
    listener: TcpListener,
    basic_auth_header: Option<String>,
    max_fris_per_snark: usize,
    shutdown: GracefulShutdown,
) {
    let app_state = AppState {
        fri_job_manager,
        snark_job_manager,
        proof_storage,
        max_fris_per_snark,
        max_snark_peek_proof_bytes: MAX_SNARK_PEEK_PROOF_BYTES,
    };

    let routes = v1_routes().route_layer(middleware::from_fn_with_state(
        SubmissionAdmission {
            fri_slots: Arc::new(Semaphore::new(FRI_SUBMISSION_CONCURRENCY)),
            snark_slots: Arc::new(Semaphore::new(SNARK_SUBMISSION_CONCURRENCY)),
        },
        admit_proof_submission,
    ));
    let routes = match basic_auth_header {
        // SYSCOIN: `route_layer` additions are outermost, so authentication rejects before
        // admission accounting or body buffering can consume resources.
        Some(expected_authorization) => routes.route_layer(middleware::from_fn_with_state(
            ProverApiAuth {
                expected_authorization_hash: authorization_hash(expected_authorization.as_bytes()),
            },
            require_basic_auth,
        )),
        None => routes,
    };

    let app = Router::new()
        .nest("/prover-jobs/v1", routes)
        .with_state(app_state)
        // Set the request body limit to 10MiB
        .layer(DefaultBodyLimit::max(PROVER_SUBMISSION_BODY_LIMIT))
        // SYSCOIN: Large prover inputs are expected; allow standard HTTP response compression so
        // remote provers do not need to pull multi-megabyte JSON payloads uncompressed.
        .layer(CompressionLayer::new());

    let addr = listener
        .local_addr()
        .expect("failed to get prover server local addr");
    tracing::info!("prover API server listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.ignore_guard())
        .await
        .expect("never errors according to doc");
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
        time::Duration,
    };

    use axum::{
        Router,
        body::{Body, Bytes},
        extract::Request,
        http::{Method, StatusCode, header},
        middleware,
        routing::post,
    };
    use http_body::Frame;
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use super::{
        FRI_SUBMISSION_CONCURRENCY, ProverApiAuth, SNARK_SUBMISSION_CONCURRENCY,
        SubmissionAdmission, SubmissionAdmissionStage, admit_proof_submission, authorization_hash,
        buffer_submission_body, proof_submission_stage, require_basic_auth,
    };

    fn admission() -> SubmissionAdmission {
        SubmissionAdmission {
            fri_slots: Arc::new(Semaphore::new(FRI_SUBMISSION_CONCURRENCY)),
            snark_slots: Arc::new(Semaphore::new(SNARK_SUBMISSION_CONCURRENCY)),
        }
    }

    // SYSCOIN: A saturated SNARK upload lane cannot consume any of the three FRI body/handler slots.
    #[test]
    fn snark_submission_cannot_starve_fri_admission() {
        let admission = admission();
        let _snark = admission
            .try_acquire(SubmissionAdmissionStage::Snark)
            .unwrap();
        assert!(
            admission
                .try_acquire(SubmissionAdmissionStage::Snark)
                .is_err()
        );
        let _fri_permits: Vec<_> = (0..FRI_SUBMISSION_CONCURRENCY)
            .map(|_| {
                admission
                    .try_acquire(SubmissionAdmissionStage::Fri)
                    .unwrap()
            })
            .collect();
        assert!(
            admission
                .try_acquire(SubmissionAdmissionStage::Fri)
                .is_err()
        );
    }

    // SYSCOIN: Three saturated FRI uploads likewise leave the dedicated SNARK slot available.
    #[test]
    fn fri_submissions_cannot_starve_snark_admission() {
        let admission = admission();
        let _fri_permits: Vec<_> = (0..FRI_SUBMISSION_CONCURRENCY)
            .map(|_| {
                admission
                    .try_acquire(SubmissionAdmissionStage::Fri)
                    .unwrap()
            })
            .collect();
        let _snark = admission
            .try_acquire(SubmissionAdmissionStage::Snark)
            .unwrap();
    }

    // SYSCOIN: Only proof-upload routes consume body slots; pick/status/peek remain responsive.
    #[test]
    fn admission_routes_only_stage_submission_bodies() {
        let request = |method, path| {
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap()
        };
        assert!(matches!(
            proof_submission_stage(&request(Method::POST, "/prover-jobs/v1/FRI/submit")),
            Some(SubmissionAdmissionStage::Fri)
        ));
        assert!(matches!(
            proof_submission_stage(&request(Method::POST, "/prover-jobs/v1/SNARK/submit")),
            Some(SubmissionAdmissionStage::Snark)
        ));
        for path in [
            "/prover-jobs/v1/FRI/pick",
            "/prover-jobs/v1/SNARK/peek/1/3",
            "/prover-jobs/v1/status/FRI",
        ] {
            assert!(proof_submission_stage(&request(Method::GET, path)).is_none());
        }
    }

    struct PanicIfPolledBody;

    impl http_body::Body for PanicIfPolledBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            panic!("unauthenticated request body was polled")
        }
    }

    // SYSCOIN: Reject bad credentials before reserving a stage slot or reading attacker bytes.
    #[tokio::test]
    async fn authentication_is_outermost_and_never_polls_unauthorized_body() {
        let routes = Router::new()
            .route("/FRI/submit", post(|| async { StatusCode::NO_CONTENT }))
            .route_layer(middleware::from_fn_with_state(
                admission(),
                admit_proof_submission,
            ))
            .route_layer(middleware::from_fn_with_state(
                ProverApiAuth {
                    expected_authorization_hash: authorization_hash(b"Basic expected"),
                },
                require_basic_auth,
            ));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/FRI/submit")
            .header(header::AUTHORIZATION, "Basic wrong")
            .body(Body::new(PanicIfPolledBody))
            .unwrap();

        let response = routes.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let request = Request::builder()
            .method(Method::POST)
            .uri("/FRI/submit")
            .header(header::AUTHORIZATION, "Basic expected")
            .body(Body::empty())
            .unwrap();
        let response = routes.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    struct NeverCompletesBody;

    impl http_body::Body for NeverCompletesBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Pending
        }
    }

    struct InterruptedBody;

    impl http_body::Body for InterruptedBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "test upload interrupted",
            ))))
        }
    }

    // SYSCOIN: Slow authenticated uploads cannot retain their dedicated stage slot indefinitely.
    #[tokio::test]
    async fn submission_body_has_total_deadline() {
        let status =
            buffer_submission_body(Body::new(NeverCompletesBody), 16, Duration::from_millis(5))
                .await
                .unwrap_err();
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    }

    // SYSCOIN: Stream bytes, not a caller-controlled Content-Length, enforce the allocation cap.
    #[tokio::test]
    async fn submission_body_rejects_actual_bytes_over_limit() {
        let status = buffer_submission_body(Body::from(vec![0_u8; 5]), 4, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // SYSCOIN: A broken upload is ambiguous, not proof that the canonical body exceeded its cap.
    #[tokio::test]
    async fn submission_body_stream_error_is_retryable() {
        let status = buffer_submission_body(Body::new(InterruptedBody), 16, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    }
}
