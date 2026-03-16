use crate::AnvilL1;
use alloy::network::EthereumWallet;
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use super::releases::{
    ReleaseBundle, ReleaseSelector, download_server_release, resolve_current_server_binary,
    start_anvil_from_state,
};
use super::server::ExternalServer;
use super::settlement::{settle_new_batches, wait_for_rich_l2_balance};
use super::{DEFAULT_RICH_PRIVATE_KEY, EXPECTED_BATCHES_PER_PHASE};

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(60);
const MINOR_UNUSABLE_TIMEOUT: Duration = Duration::from_secs(90);

pub async fn restart_from_previous_patch_settles_three_batches() -> anyhow::Result<()> {
    let current_bin = resolve_current_server_binary().await?;
    let previous_patch = match download_server_release(ReleaseSelector::PreviousPatch).await? {
        Some(bundle) => bundle,
        None => {
            tracing::warn!(
                version = env!("CARGO_PKG_VERSION"),
                "No previous patch release is available; skipping patch restart scenario"
            );
            return Ok(());
        }
    };

    let mut scenario = VersionRestartScenario::start(&previous_patch).await?;
    let patch_batches = scenario
        .run_success_phase(
            "previous-patch",
            &previous_patch.binary_path,
            previous_patch.fixture.default_config_path(),
            EXPECTED_BATCHES_PER_PHASE,
        )
        .await?;
    let current_batches = scenario
        .run_success_phase(
            "current-after-patch",
            &current_bin,
            previous_patch.fixture.default_config_path(),
            EXPECTED_BATCHES_PER_PHASE,
        )
        .await?;
    anyhow::ensure!(
        current_batches > patch_batches,
        "current phase did not advance settled batch count after patch restart"
    );
    Ok(())
}

pub async fn restart_from_previous_minor_is_not_operational() -> anyhow::Result<()> {
    let current_bin = resolve_current_server_binary().await?;
    let previous_minor = download_server_release(ReleaseSelector::PreviousMinor)
        .await?
        .context("previous minor release is required for this scenario")?;

    let mut scenario = VersionRestartScenario::start(&previous_minor).await?;
    scenario
        .run_success_phase(
            "previous-minor",
            &previous_minor.binary_path,
            previous_minor.fixture.default_config_path(),
            EXPECTED_BATCHES_PER_PHASE,
        )
        .await?;

    let baseline = scenario.known_finalized_batch;
    match scenario
        .run_unusable_phase(
            "current-after-minor",
            &current_bin,
            previous_minor.fixture.default_config_path(),
            baseline,
            EXPECTED_BATCHES_PER_PHASE,
        )
        .await?
    {
        UnusableOutcome::ExitedBeforeReady
        | UnusableOutcome::FailedToAdvanceBatches
        | UnusableOutcome::TransactionExecutionFailed => Ok(()),
        UnusableOutcome::Operational(final_batch) => anyhow::bail!(
            "current binary remained operational after previous minor restart and advanced to batch {final_batch}"
        ),
    }
}

#[derive(Debug)]
enum UnusableOutcome {
    ExitedBeforeReady,
    FailedToAdvanceBatches,
    TransactionExecutionFailed,
    Operational(u64),
}

struct VersionRestartScenario {
    l1: AnvilL1,
    l2_wallet: EthereumWallet,
    tempdir: Arc<TempDir>,
    shared_db_path: PathBuf,
    known_finalized_batch: u64,
}

impl VersionRestartScenario {
    async fn start(bundle: &ReleaseBundle) -> anyhow::Result<Self> {
        let l1 = start_anvil_from_state(bundle.fixture.l1_state_bytes()?).await?;
        let tempdir = Arc::new(tempfile::tempdir()?);
        let shared_db_path = tempdir.path().join("shared-rocksdb");
        std::fs::create_dir_all(&shared_db_path)?;
        let l2_wallet = EthereumWallet::new(
            alloy::signers::local::LocalSigner::from_str(DEFAULT_RICH_PRIVATE_KEY).unwrap(),
        );

        Ok(Self {
            l1,
            l2_wallet,
            tempdir,
            shared_db_path,
            known_finalized_batch: 0,
        })
    }

    async fn run_success_phase(
        &mut self,
        phase: &str,
        binary_path: &Path,
        config_path: PathBuf,
        new_batches: u64,
    ) -> anyhow::Result<u64> {
        let start_batch = self.known_finalized_batch;
        let mut server = ExternalServer::spawn(
            phase,
            binary_path,
            &config_path,
            &self.shared_db_path,
            self.tempdir.path(),
            &self.l1.address,
        )
        .await?;
        let providers = server.connect(self.l2_wallet.clone(), SERVER_START_TIMEOUT).await?;
        wait_for_rich_l2_balance(&providers, self.l2_wallet.default_signer().address()).await?;
        let final_batch = settle_new_batches(&self.l1.provider, &providers, start_batch, new_batches)
            .await
            .with_context(|| format!("phase `{phase}` failed to settle {new_batches} new batches"))?;
        server.stop().await?;
        self.known_finalized_batch = final_batch;
        Ok(final_batch)
    }

    async fn run_unusable_phase(
        &mut self,
        phase: &str,
        binary_path: &Path,
        config_path: PathBuf,
        start_batch: u64,
        new_batches: u64,
    ) -> anyhow::Result<UnusableOutcome> {
        let mut server = ExternalServer::spawn(
            phase,
            binary_path,
            &config_path,
            &self.shared_db_path,
            self.tempdir.path(),
            &self.l1.address,
        )
        .await?;
        let providers = match server
            .connect(self.l2_wallet.clone(), SERVER_START_TIMEOUT)
            .await
        {
            Ok(providers) => providers,
            Err(err) => {
                if server.exited().await? {
                    return Ok(UnusableOutcome::ExitedBeforeReady);
                }
                return Err(err);
            }
        };

        let outcome = match tokio::time::timeout(
            MINOR_UNUSABLE_TIMEOUT,
            settle_new_batches(&self.l1.provider, &providers, start_batch, new_batches),
        )
        .await
        {
            Ok(Ok(final_batch)) => UnusableOutcome::Operational(final_batch),
            Ok(Err(err)) => {
                tracing::info!(phase, %err, "current-after-minor failed while driving traffic");
                UnusableOutcome::TransactionExecutionFailed
            }
            Err(_) => UnusableOutcome::FailedToAdvanceBatches,
        };
        server.stop().await?;
        Ok(outcome)
    }
}
