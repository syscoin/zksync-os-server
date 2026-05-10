//! Prover server module for handling proof generation requests.
//!
//! This module provides an HTTP server that manages proof generation jobs
//! and proof storage.
mod v1;

use std::{net::SocketAddr, sync::Arc};

use crate::prover_api::{
    fri_job_manager::FriJobManager, proof_storage::ProofStorage, prover_server::v1::v1_routes,
    snark_job_manager::SnarkJobManager,
};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use reth_tasks::shutdown::GracefulShutdown;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;

/// Application state shared across all request handlers.
#[derive(Clone)]
pub(in crate::prover_api::prover_server) struct AppState {
    fri_job_manager: Arc<FriJobManager>,
    snark_job_manager: Arc<SnarkJobManager>,
    proof_storage: ProofStorage,
}

// SYSCOIN: Remote provers authenticate using the Basic Auth header already supported by
// `SequencerProofClient` when credentials are embedded in the prover API URL.
#[derive(Clone)]
struct ProverApiAuth {
    expected_authorization: Arc<str>,
}

async fn require_basic_auth(
    State(auth): State<ProverApiAuth>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == auth.expected_authorization.as_ref());

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

/// Entry point for prover API server.
/// Starts an HTTP server listening on the specified bind address.
pub async fn run(
    fri_job_manager: Arc<FriJobManager>,
    snark_job_manager: Arc<SnarkJobManager>,
    proof_storage: ProofStorage,
    bind_address: String,
    basic_auth_header: Option<String>,
    shutdown: GracefulShutdown,
) {
    let app_state = AppState {
        fri_job_manager,
        snark_job_manager,
        proof_storage,
    };

    let routes = match basic_auth_header {
        Some(expected_authorization) => v1_routes().route_layer(middleware::from_fn_with_state(
            ProverApiAuth {
                expected_authorization: Arc::from(expected_authorization),
            },
            require_basic_auth,
        )),
        None => v1_routes(),
    };

    let app = Router::new()
        .nest("/prover-jobs/v1", routes)
        .with_state(app_state)
        // Set the request body limit to 10MiB
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        // SYSCOIN Large prover inputs are expected; allow standard HTTP response compression so
        // remote provers do not need to pull multi-megabyte JSON payloads uncompressed.
        .layer(CompressionLayer::new());

    let bind_address: SocketAddr = bind_address.parse().expect("failed to parse bind address");
    tracing::info!("starting proof data server on {bind_address}");

    let listener = TcpListener::bind(bind_address)
        .await
        .expect("failed to bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.ignore_guard())
        .await
        .expect("never errors according to doc");
}
