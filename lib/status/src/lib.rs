//! Status HTTP endpoints.
//!
//! - `GET /status` — general node status, including consensus (Raft) state.
//! - `GET /status/health` — liveness endpoint. Always 200 while the process is up.
//! - `GET /status/pipeline` — per-component backpressure and lag snapshot for
//!   diagnostics and dashboards.

mod health;
mod pipeline;
mod status;

use crate::health::{health, ready};
use crate::pipeline::pipeline;
use crate::status::status;
use axum::{Router, routing::get};
use reth_tasks::shutdown::GracefulShutdown;
use std::sync::{Arc, OnceLock};
use tokio::{net::TcpListener, sync::watch};
use zksync_os_backpressure::PipelineSnapshot;
use zksync_os_raft::RaftConsensusStatus;

pub use status::{ConsensusStatus, StatusResponse};

#[derive(Clone)]
pub struct StatusServerState {
    pub pipeline_snapshot: watch::Receiver<PipelineSnapshot>,
    pub consensus_raft_status_rx: Option<watch::Receiver<Option<RaftConsensusStatus>>>,
    pub ready: Arc<OnceLock<()>>,
}

pub(crate) type AppState = StatusServerState;

/// Runs the status HTTP server on a pre-bound listener.
pub async fn run_status_server(
    listener: TcpListener,
    shutdown: GracefulShutdown,
    state: StatusServerState,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/status", get(status))
        .route("/status/health", get(health))
        .route("/status/ready", get(ready))
        .route("/status/pipeline", get(pipeline))
        .with_state(state);

    let addr = listener.local_addr()?;
    tracing::info!(%addr, "status server running");

    // Axum polls the shutdown signal in a separate task before closing its listener. Keep
    // the original guard until the server has also drained its connections, so runtime
    // shutdown cannot return while this server still owns its port.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().ignore_guard())
        .await?;
    tracing::info!("status server graceful shutdown complete");
    drop(shutdown);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_tasks::Runtime;
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn graceful_shutdown_waits_for_status_listener_to_close() -> anyhow::Result<()> {
        let runtime = Runtime::test();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (_, pipeline_snapshot) = watch::channel(Vec::new());
        let state = StatusServerState {
            pipeline_snapshot,
            consensus_raft_status_rx: None,
            ready: Arc::new(OnceLock::new()),
        };
        let (started_tx, started_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        let server_task = runtime.spawn_critical_with_graceful_shutdown_signal(
            "status server test",
            |shutdown| async move {
                let server = run_status_server(listener, shutdown, state);
                tokio::pin!(server);
                // Start Axum's separate signal task, then hold the accept loop still. This
                // exposes early guard release without relying on a lucky restart race.
                poll_fn(|cx| {
                    assert!(server.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                started_tx.send(()).expect("test receiver dropped");
                resume_rx.await.expect("test sender dropped");
                server.await.expect("status server failed");
            },
        );
        started_rx.await?;
        let shutdown_signal = runtime.on_shutdown_signal().clone();
        let mut shutdown_task = tokio::task::spawn_blocking(move || {
            runtime.graceful_shutdown_with_timeout(Duration::from_secs(5))
        });
        shutdown_signal.await;

        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut shutdown_task)
                .await
                .is_err(),
            "runtime shutdown returned before the status accept loop could close its listener"
        );
        assert!(
            TcpListener::bind(address).await.is_err(),
            "the paused status server must still own its listener"
        );

        resume_tx.send(()).expect("status server task dropped");
        assert!(shutdown_task.await?, "status server shutdown timed out");
        // Do not join the server first: runtime shutdown itself must make rebinding safe.
        let replacement = TcpListener::bind(address).await?;
        assert_eq!(replacement.local_addr()?, address);
        server_task.await?;
        Ok(())
    }
}
