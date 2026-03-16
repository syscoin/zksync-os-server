use crate::dyn_wallet_provider::EthDynProvider;
use crate::network::Zksync;
use crate::version_restart::releases::{config_workspace_dir, workspace_dir};
use alloy::network::EthereumWallet;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use anyhow::Context;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};

pub(crate) struct ExternalServer {
    child: Child,
    rpc_url: String,
    #[allow(dead_code)]
    log_path: PathBuf,
}

impl ExternalServer {
    pub(crate) async fn spawn(
        phase: &str,
        binary_path: &Path,
        config_path: &Path,
        shared_db_path: &Path,
        output_dir: &Path,
        l1_rpc_url: &str,
    ) -> anyhow::Result<Self> {
        let rpc_port = crate::utils::LockedPort::acquire_unused().await?;
        let prover_port = crate::utils::LockedPort::acquire_unused().await?;
        let status_port = crate::utils::LockedPort::acquire_unused().await?;
        let rpc_address = format!("0.0.0.0:{}", rpc_port.port);
        let prover_address = format!("0.0.0.0:{}", prover_port.port);
        let status_address = format!("0.0.0.0:{}", status_port.port);
        let rpc_url = format!("http://localhost:{}", rpc_port.port);
        let log_path = output_dir.join(format!("{phase}.log"));
        let log_file = File::create(&log_path)?;
        let log_file_err = log_file.try_clone()?;

        let mut command = Command::new(binary_path);
        command
            .arg("--config")
            .arg(config_path)
            .current_dir(config_workspace_dir(config_path)?)
            .env("general_rocks_db_path", shared_db_path)
            .env("general_l1_rpc_url", l1_rpc_url)
            .env("rpc_address", &rpc_address)
            .env("prover_api_address", &prover_address)
            .env("status_server_enabled", "false")
            .env("status_server_address", &status_address)
            .env("network_enabled", "false")
            .env("batch_verification_server_enabled", "false")
            .env("batch_verification_client_enabled", "false")
            .env("observability_log_use_color", "false")
            .env("observability_log_format", "logfmt")
            .env("sequencer_block_time", "500ms")
            .env("WORKSPACE_DIR", workspace_dir())
            .env("RUST_BACKTRACE", "1")
            .stdout(log_file)
            .stderr(log_file_err);

        let child = command
            .spawn()
            .with_context(|| format!("failed to spawn {}", binary_path.display()))?;
        drop(rpc_port);
        drop(prover_port);
        drop(status_port);

        Ok(Self {
            child,
            rpc_url,
            log_path,
        })
    }

    pub(crate) async fn connect(
        &mut self,
        wallet: EthereumWallet,
        timeout: Duration,
    ) -> anyhow::Result<ConnectedProviders> {
        let rpc_url = self.rpc_url.clone();
        let started_at = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::bail!(
                    "server exited before becoming ready with status {status}\n{}",
                    self.log_excerpt()
                );
            }

            match connect_l2(wallet.clone(), &rpc_url).await {
                Ok(providers) => return Ok(providers),
                Err(err) if started_at.elapsed() < timeout => {
                    tracing::info!(%err, rpc_url, "retrying connection to external server");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to connect to external server at {rpc_url}\n{}",
                            self.log_excerpt()
                        )
                    });
                }
            }
        }
    }

    pub(crate) async fn exited(&mut self) -> anyhow::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    pub(crate) async fn stop(&mut self) -> anyhow::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
            let _ = self.child.wait().await?;
        }
        Ok(())
    }

    fn log_excerpt(&self) -> String {
        match std::fs::read_to_string(&self.log_path) {
            Ok(contents) => {
                let lines: Vec<_> = contents.lines().rev().take(80).collect();
                let excerpt = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
                format!("--- {} ---\n{excerpt}", self.log_path.display())
            }
            Err(err) => format!(
                "--- {} ---\nfailed to read log file: {err}",
                self.log_path.display()
            ),
        }
    }
}

pub(crate) struct ConnectedProviders {
    pub(crate) ethereum: EthDynProvider,
    pub(crate) zksync: DynProvider<Zksync>,
}

async fn connect_l2(wallet: EthereumWallet, rpc_url: &str) -> anyhow::Result<ConnectedProviders> {
    let ethereum = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect(rpc_url)
        .await?;
    ethereum.get_chain_id().await?;

    let zksync = ProviderBuilder::new_with_network::<Zksync>()
        .wallet(wallet)
        .connect(rpc_url)
        .await?;
    zksync.get_chain_id().await?;

    Ok(ConnectedProviders {
        ethereum: EthDynProvider::new(ethereum),
        zksync: DynProvider::new(zksync),
    })
}
