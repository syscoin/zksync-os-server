mod health;
mod status;

use crate::health::health;
use crate::status::status;

pub use status::{ConsensusStatus, StatusResponse};
use axum::{routing::get, Router};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::watch};
use zksync_os_raft::RaftConsensusStatus;

#[derive(Clone)]
struct AppState {
    stop_receiver: watch::Receiver<bool>,
    consensus_raft_status_rx: Option<watch::Receiver<RaftConsensusStatus>>,
}

pub async fn run_status_server(
    bind_address: String,
    stop_receiver: watch::Receiver<bool>,
    consensus_raft_status_rx: Option<watch::Receiver<RaftConsensusStatus>>,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/status/health", get(health))
        .route("/status", get(status))
        .with_state(AppState {
            stop_receiver,
            consensus_raft_status_rx,
        });

    let addr: SocketAddr = bind_address.parse()?;
    let listener = TcpListener::bind(addr).await?;

    let addr = listener.local_addr()?;
    tracing::info!("running a status server" = %addr);

    axum::serve(listener, app).await?;

    Ok(())
}
