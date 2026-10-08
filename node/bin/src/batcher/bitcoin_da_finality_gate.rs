use crate::batcher::bitcoin_da_status_storage::{
    BitcoinDaBatchStatus, BitcoinDaFinalityPolicy, BitcoinDaStatusStorage,
};
use crate::config::{BatcherConfig, BitcoinDaFinalityMode};
use alloy::hex;
use anyhow::Context;
use async_trait::async_trait;
use bitcoin_da_client::{
    BitcoinDaFinalityMode as ClientBitcoinDaFinalityMode, BlobFinalityState, MAX_BLOB_SIZE,
    SyscoinClient,
};
use blake2::{Blake2s256, Digest};
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zksync_os_batch_types::{SYSCOIN_DA_MAX_BLOBS_PER_BATCH, syscoin_edge_da_refs_from_input};
use zksync_os_contract_interface::models::DACommitmentScheme;
use zksync_os_l1_sender::commands::{L1SenderCommand, commit::CommitCommand};
use zksync_os_observability::ComponentStateReporter;
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent, SendAndRecordExt};

pub struct BitcoinDaFinalityGate {
    config: BatcherConfig,
    storage: BitcoinDaStatusStorage,
    settling_on_gateway: bool,
}

#[derive(Clone, Copy)]
enum BlobFinalityWaitContext {
    OwnBatch { batch_number: u64 },
    GatewayEdgeRef,
}

fn authenticate_recovered_blob(
    blob: Vec<u8>,
    version_hash: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    anyhow::ensure!(
        !blob.is_empty() && blob.len() <= 2 * MAX_BLOB_SIZE,
        "recovered Bitcoin DA blob {version_hash} has invalid size {}, expected raw data up to {MAX_BLOB_SIZE} bytes or bounded bare hex",
        blob.len()
    );
    let normalized_expected = version_hash.strip_prefix("0x").unwrap_or(version_hash);
    // SYSCOIN: The RPC returns decoded data, but PoDA may return bare hex. Authenticate
    // raw bytes first so valid hex-looking blobs are not silently decoded into other data.
    if blob.len() <= MAX_BLOB_SIZE {
        let recovered_hash = hex::encode(Blake2s256::digest(&blob));
        if recovered_hash.eq_ignore_ascii_case(normalized_expected) {
            return Ok((blob, recovered_hash));
        }
    }
    let is_bare_hex = blob.len().is_multiple_of(2) && blob.iter().all(u8::is_ascii_hexdigit);
    anyhow::ensure!(
        blob.len() <= MAX_BLOB_SIZE || is_bare_hex,
        "recovered Bitcoin DA blob {version_hash} has invalid size {}, expected raw data up to {MAX_BLOB_SIZE} bytes or bounded bare hex",
        blob.len()
    );
    anyhow::ensure!(
        is_bare_hex,
        "recovered Bitcoin DA hash mismatch: expected {normalized_expected}; recovery bytes are not authenticated raw data or strict bare hex"
    );
    // SYSCOIN: Bound the wire representation before allocating, decode once, and
    // require the same committed hash before reserving an attempt or spending funds.
    let blob = hex::decode(blob).context("invalid recovered Bitcoin DA bare hex")?;
    anyhow::ensure!(
        !blob.is_empty() && blob.len() <= MAX_BLOB_SIZE,
        "recovered Bitcoin DA blob {version_hash} has invalid decoded size {}",
        blob.len()
    );
    let recovered_hash = hex::encode(Blake2s256::digest(&blob));
    anyhow::ensure!(
        recovered_hash.eq_ignore_ascii_case(normalized_expected),
        "recovered Bitcoin DA hash mismatch: expected {normalized_expected}, got {recovered_hash}"
    );
    Ok((blob, recovered_hash))
}

impl BitcoinDaFinalityGate {
    pub fn new(
        config: BatcherConfig,
        storage: BitcoinDaStatusStorage,
        settling_on_gateway: bool,
    ) -> Self {
        Self {
            config,
            storage,
            settling_on_gateway,
        }
    }

    fn current_finality_policy(&self) -> BitcoinDaFinalityPolicy {
        BitcoinDaFinalityPolicy {
            mode: self.config.bitcoin_da_finality_mode,
            confirmations: self.config.bitcoin_da_finality_confirmations,
        }
    }

    fn finality_mode(&self) -> ClientBitcoinDaFinalityMode {
        match self.config.bitcoin_da_finality_mode {
            BitcoinDaFinalityMode::Chainlock => ClientBitcoinDaFinalityMode::Chainlock,
            BitcoinDaFinalityMode::Confirmations => ClientBitcoinDaFinalityMode::Confirmations,
        }
    }

    fn client(&self) -> anyhow::Result<SyscoinClient> {
        let rpc_url = self
            .config
            .bitcoin_da_rpc_url
            .as_deref()
            .context("`batcher.bitcoin_da_rpc_url` must be set when using blob pubdata mode")?;
        let rpc_user =
            self.config.bitcoin_da_rpc_user.as_ref().context(
                "`batcher.bitcoin_da_rpc_user` must be set when using blob pubdata mode",
            )?;
        let rpc_password = self.config.bitcoin_da_rpc_password.as_ref().context(
            "`batcher.bitcoin_da_rpc_password` must be set when using blob pubdata mode",
        )?;

        SyscoinClient::new(
            rpc_url,
            rpc_user.expose_secret(),
            rpc_password.expose_secret(),
            &self.config.bitcoin_da_poda_url,
            Some(self.config.bitcoin_da_request_timeout),
            &self.config.bitcoin_da_wallet_name,
        )
        .map_err(|err| anyhow::anyhow!("failed to create Bitcoin DA client: {err}"))
    }

    async fn verify_batch_da_before_commit(
        &self,
        batch_number: u64,
        expected_version_hashes: &[u8],
    ) -> anyhow::Result<()> {
        let expected_hashes: Vec<String> = expected_version_hashes
            .chunks_exact(32)
            .map(hex::encode)
            .collect();
        anyhow::ensure!(
            expected_hashes.len() * 32 == expected_version_hashes.len(),
            "Bitcoin DA operator input for batch {batch_number} is not a 32-byte hash array"
        );
        anyhow::ensure!(
            expected_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            expected_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
        );

        let mut status = self.storage.load(batch_number).await?.with_context(|| {
            format!("missing Bitcoin DA publication status for batch {batch_number}")
        })?;
        anyhow::ensure!(
            status.expected_hashes == expected_hashes,
            "Bitcoin DA publication status mismatch for batch {batch_number}: stored expected {:?}, command expected {:?}",
            status.expected_hashes,
            expected_hashes
        );
        if self.settling_on_gateway {
            self.verify_published_batch_status(batch_number, &status)
                .await?;
            return Ok(());
        }

        let current_policy = self.current_finality_policy();
        if status.finalized && status.finality_policy.as_ref() == Some(&current_policy) {
            tracing::info!(batch_number, "Bitcoin DA already finalized");
            return Ok(());
        }

        anyhow::ensure!(
            status.published_hashes.len() == status.expected_hashes.len(),
            "Bitcoin DA publication incomplete for batch {batch_number}: published {} of {} blobs",
            status.published_hashes.len(),
            status.expected_hashes.len(),
        );
        anyhow::ensure!(
            status.published_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            status.published_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
        );
        for (idx, (published_hash, expected_hash)) in status
            .published_hashes
            .iter()
            .zip(status.expected_hashes.iter())
            .enumerate()
        {
            let normalized_hash = published_hash.strip_prefix("0x").unwrap_or(published_hash);
            anyhow::ensure!(
                normalized_hash.eq_ignore_ascii_case(expected_hash),
                "Bitcoin DA version hash mismatch for batch {batch_number}, blob {idx}: expected {expected_hash}, got {normalized_hash}"
            );
        }

        let client = self.client()?;
        for version_hash in &status.published_hashes {
            self.wait_for_blob_finality(
                &client,
                version_hash,
                BlobFinalityWaitContext::OwnBatch { batch_number },
            )
            .await?;
        }

        status.finalized = true;
        status.finality_policy = Some(current_policy);
        self.storage.save(batch_number, &status).await?;
        Ok(())
    }

    async fn verify_published_batch_status(
        &self,
        batch_number: u64,
        status: &BitcoinDaBatchStatus,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            status.published_hashes.len() == status.expected_hashes.len(),
            "Bitcoin DA publication incomplete for batch {batch_number}: published {} of {} blobs",
            status.published_hashes.len(),
            status.expected_hashes.len(),
        );
        anyhow::ensure!(
            status.published_hashes.len() <= SYSCOIN_DA_MAX_BLOBS_PER_BATCH,
            "Bitcoin DA batch {batch_number} has {} blobs, max is {}",
            status.published_hashes.len(),
            SYSCOIN_DA_MAX_BLOBS_PER_BATCH
        );
        for (idx, (published_hash, expected_hash)) in status
            .published_hashes
            .iter()
            .zip(status.expected_hashes.iter())
            .enumerate()
        {
            let normalized_hash = published_hash.strip_prefix("0x").unwrap_or(published_hash);
            anyhow::ensure!(
                normalized_hash.eq_ignore_ascii_case(expected_hash),
                "Bitcoin DA version hash mismatch for batch {batch_number}, blob {idx}: expected {expected_hash}, got {normalized_hash}"
            );
        }

        tracing::info!(
            batch_number,
            blob_count = status.published_hashes.len(),
            "Bitcoin DA publication status verified before Gateway commit"
        );
        Ok(())
    }

    async fn wait_for_edge_ref_finality(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
    ) -> anyhow::Result<()> {
        self.wait_for_blob_finality(
            client,
            version_hash,
            BlobFinalityWaitContext::GatewayEdgeRef,
        )
        .await
    }

    async fn wait_for_blob_finality(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
        context: BlobFinalityWaitContext,
    ) -> anyhow::Result<()> {
        // SYSCOIN: Keep the wallet budget guard effective for callers that bypass startup config.
        anyhow::ensure!(
            self.config.bitcoin_da_max_republish_attempts == 0
                || !self.config.bitcoin_da_finality_timeout.is_zero(),
            "Bitcoin DA finality timeout must be positive when republication is enabled"
        );
        let mut start = Instant::now();
        loop {
            let finality_state = self.blob_finality_state(client, version_hash).await?;
            if finality_state.is_final() {
                match context {
                    BlobFinalityWaitContext::OwnBatch { batch_number } => {
                        tracing::info!(batch_number, version_hash, "Bitcoin DA blob finalized");
                    }
                    BlobFinalityWaitContext::GatewayEdgeRef => {
                        tracing::info!(version_hash, "Gateway edge DA ref finalized");
                    }
                }
                return Ok(());
            }
            if matches!(finality_state, BlobFinalityState::Confirmed { .. }) {
                tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
                continue;
            }

            if start.elapsed() >= self.config.bitcoin_da_finality_timeout {
                self.republish_blob_after_timeout(client, version_hash, context)
                    .await?;
                start = Instant::now();
            }

            tokio::time::sleep(self.config.bitcoin_da_finality_poll_interval).await;
        }
    }

    async fn republish_blob_after_timeout(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
        context: BlobFinalityWaitContext,
    ) -> anyhow::Result<()> {
        match context {
            BlobFinalityWaitContext::OwnBatch { batch_number } => {
                tracing::warn!(
                    batch_number,
                    version_hash,
                    "Bitcoin DA blob did not make confirmation progress before timeout; fetching and republishing"
                );
            }
            BlobFinalityWaitContext::GatewayEdgeRef => {
                anyhow::ensure!(
                    self.config.bitcoin_da_gateway_l1_republish_enabled,
                    "Gateway edge DA ref {version_hash} did not finalize within {:?} and republish is disabled",
                    self.config.bitcoin_da_finality_timeout
                );
                tracing::warn!(
                    version_hash,
                    "Gateway edge DA ref did not finalize before timeout; fetching and republishing"
                );
            }
        }

        let blob = client
            .get_blob(version_hash)
            .await
            .map_err(|err| match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => anyhow::anyhow!(
                    "failed to fetch Bitcoin DA blob for batch {batch_number}, ref {version_hash}: {err}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => anyhow::anyhow!(
                    "failed to fetch Bitcoin DA blob for Gateway edge ref {version_hash}: {err}"
                ),
            })?;
        // SYSCOIN: Availability is not authentication; normalize only hash-bound recovery
        // bytes before the durable attempt reservation and wallet publication boundary.
        let (blob, recovered_hash) = authenticate_recovered_blob(blob, version_hash)?;
        let normalized_expected = version_hash.strip_prefix("0x").unwrap_or(version_hash);
        let attempt = self
            .storage
            .reserve_republication(
                &recovered_hash,
                self.config.bitcoin_da_max_republish_attempts,
            )
            .await?;
        tracing::warn!(
            version_hash,
            attempt,
            "Reserved Bitcoin DA republication attempt"
        );
        let republished_hash = client.force_create_blob(&blob).await.map_err(|err| {
            match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => anyhow::anyhow!(
                    "failed to republish Bitcoin DA blob for batch {batch_number}, ref {version_hash}: {err}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => anyhow::anyhow!(
                    "failed to republish Bitcoin DA blob for Gateway edge ref {version_hash}: {err}"
                ),
            }
        })?;
        let normalized_republished = republished_hash
            .strip_prefix("0x")
            .unwrap_or(&republished_hash);
        anyhow::ensure!(
            normalized_republished.eq_ignore_ascii_case(normalized_expected),
            "{}",
            match context {
                BlobFinalityWaitContext::OwnBatch { batch_number } => format!(
                    "republished Bitcoin DA hash mismatch for batch {batch_number}: expected {normalized_expected}, got {normalized_republished}"
                ),
                BlobFinalityWaitContext::GatewayEdgeRef => format!(
                    "republished Bitcoin DA hash mismatch for Gateway edge ref: expected {normalized_expected}, got {normalized_republished}"
                ),
            }
        );
        Ok(())
    }

    async fn blob_finality_state(
        &self,
        client: &SyscoinClient,
        version_hash: &str,
    ) -> anyhow::Result<BlobFinalityState> {
        client
            .blob_finality_state_with_mode(
                version_hash,
                self.finality_mode(),
                self.config.bitcoin_da_finality_confirmations,
            )
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to check Bitcoin DA finality for {version_hash}: {err}")
            })
    }

    async fn wait_for_gateway_edge_refs_finality(
        &self,
        command: &CommitCommand,
    ) -> anyhow::Result<()> {
        let mut client = None;
        for batch in command.as_ref() {
            let input = &batch.batch.batch_info.commit_info.edge_da_refs_input;
            if input.is_empty() {
                continue;
            }
            // SYSCOIN: Parse and enforce the canonical per-message and aggregate 32-ref bound
            // before constructing a client or issuing any finality/republish work.
            let edge_refs = syscoin_edge_da_refs_from_input(input).with_context(|| {
                format!(
                    "failed to parse Gateway edge DA refs for batch {}",
                    batch.batch_number()
                )
            })?;
            // SYSCOIN: This pass verifies only compact edge-chain references forwarded by a Gateway
            // batch; the batch's own Bitcoin DA chunks are checked separately. Create the
            // client lazily once there is at least one forwarded reference to verify.
            if client.is_none() {
                client = Some(self.client()?);
            }
            let client = client
                .as_ref()
                .expect("Bitcoin DA client was initialized before use");
            for edge_ref in edge_refs {
                for version_hash in edge_ref.blob_version_hashes.chunks_exact(32) {
                    let version_hash = hex::encode(version_hash);
                    self.wait_for_edge_ref_finality(client, &version_hash)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn verify_command_da_before_commit(&self, command: &CommitCommand) -> anyhow::Result<()> {
        for batch in command.as_ref() {
            if batch.batch.batch_info.commit_info.l2_da_commitment_scheme
                == DACommitmentScheme::BlobsZKsyncOS
            {
                self.verify_batch_da_before_commit(
                    batch.batch_number(),
                    &batch.batch.batch_info.commit_info.operator_da_input,
                )
                .await?;
            }
        }
        if !self.settling_on_gateway {
            self.wait_for_gateway_edge_refs_finality(command).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PipelineComponent for BitcoinDaFinalityGate {
    type Input = L1SenderCommand<CommitCommand>;
    type Output = L1SenderCommand<CommitCommand>;

    const COMPONENT_ID: zksync_os_pipeline::ComponentId =
        zksync_os_pipeline::ComponentId::BitcoinDaFinalityGate;
    const OUTPUT_CHANNEL_CAPACITY: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
        state_reporter: ComponentStateReporter,
    ) -> anyhow::Result<()> {
        while let Some(command) = input.recv().await {
            if let L1SenderCommand::SendToL1(commit_command) = &command {
                self.verify_command_da_before_commit(commit_command).await?;
            }
            output.send_and_record(command, &state_reporter).await?;
        }
        tracing::info!("inbound channel closed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const ABC_HASH: &str = "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982";
    const HEX_LOOKING_HASH: &str =
        "d6ffd191d5851bd88ef4f0fffd051050a9e91c15346096a0e1f0a9afa50f34e5";

    #[test]
    fn recovery_authentication_preserves_raw_and_decodes_only_matching_hex() {
        for raw in [b"abc".to_vec(), b"616263".to_vec(), vec![0, 255, 17]] {
            let expected_hash = hex::encode(Blake2s256::digest(&raw));
            let (recovered, hash) =
                authenticate_recovered_blob(raw.clone(), &expected_hash).unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
        let raw = vec![0xde, 0xad, 0xbe, 0xef];
        let expected_hash = hex::encode(Blake2s256::digest(&raw));
        for wire in [
            b"deadbeef".to_vec(),
            b"DEADBEEF".to_vec(),
            b"DeAdBeEf".to_vec(),
        ] {
            let (recovered, hash) =
                authenticate_recovered_blob(wire, &format!("0x{}", expected_hash.to_uppercase()))
                    .unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
    }

    #[test]
    fn recovery_authentication_preserves_decoded_limit_and_bounds_wire() {
        let raw = vec![42; MAX_BLOB_SIZE];
        let expected_hash = hex::encode(Blake2s256::digest(&raw));
        for input in [raw.clone(), hex::encode(&raw).into_bytes()] {
            let (recovered, hash) = authenticate_recovered_blob(input, &expected_hash).unwrap();
            assert_eq!(recovered, raw);
            assert_eq!(hash, expected_hash);
        }
        let oversized = vec![42; MAX_BLOB_SIZE + 1];
        let oversized_hash = hex::encode(Blake2s256::digest(&oversized));
        for input in [oversized.clone(), hex::encode(oversized).into_bytes()] {
            assert!(authenticate_recovered_blob(input, &oversized_hash).is_err());
        }
    }

    #[test]
    fn recovery_authentication_refuses_malformed_mismatched_and_repeated_encoding() {
        for input in [
            Vec::new(),
            b"corrupt".to_vec(),
            b"61626".to_vec(),
            b"0x616263".to_vec(),
            b"616263\n".to_vec(),
            b" 616263".to_vec(),
            b"\"616263\"".to_vec(),
            br#"{"data":"616263"}"#.to_vec(),
            vec![255],
            b"deadbeef".to_vec(),
            b"363136323633".to_vec(),
        ] {
            assert!(authenticate_recovered_blob(input, ABC_HASH).is_err());
        }
        assert!(authenticate_recovered_blob(b"abc".to_vec(), "1234").is_err());
        assert!(authenticate_recovered_blob(b"616263".to_vec(), "not-a-hash").is_err());
        assert!(authenticate_recovered_blob(b"616263".to_vec(), &format!("0X{ABC_HASH}")).is_err());
    }

    async fn recovery_rpc(
        finality: Option<Value>,
        wallet_error: bool,
    ) -> (SyscoinClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let wallet_calls = Arc::new(AtomicUsize::new(0));
        let calls = wallet_calls.clone();
        let app = Router::new().fallback(
            get(|| async { b"abc".to_vec() }).post(move |Json(request): Json<Value>| {
                let calls = calls.clone();
                let finality = finality.clone();
                async move {
                    let body = match request["method"].as_str().unwrap() {
                        "syscoincreatenevmblob" => {
                            calls.fetch_add(1, Ordering::SeqCst);
                            if wallet_error {
                                json!({"error": {"code": -1, "message": "ambiguous publication failure"}})
                            } else {
                                json!({"result": {"versionhash": ABC_HASH}})
                            }
                        }
                        "getnevmblobdata" if request["params"][1] == true => {
                            json!({"result": {"data": "616263"}})
                        }
                        "getnevmblobdata" => match finality {
                            Some(state) => json!({"result": state}),
                            None => json!({"error": {"code": -32602, "message": "missing blob"}}),
                        },
                        method => panic!("unexpected method: {method}"),
                    };
                    let mut body = body;
                    body["id"] = request["id"].clone();
                    Json(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SyscoinClient::new(
            &url,
            "user",
            "password",
            &url,
            Some(std::time::Duration::from_secs(2)),
            "test",
        )
        .unwrap();
        (client, wallet_calls, server)
    }

    #[tokio::test]
    async fn zero_recovery_window_cannot_consume_wallet_budget() {
        let (client, calls, server) = recovery_rpc(None, false).await;
        let dir = tempfile::tempdir().unwrap();
        let config = BatcherConfig {
            bitcoin_da_max_republish_attempts: 2,
            bitcoin_da_finality_timeout: std::time::Duration::ZERO,
            ..Default::default()
        };
        let gate = BitcoinDaFinalityGate::new(
            config,
            BitcoinDaStatusStorage::new(dir.path()).unwrap(),
            false,
        );
        let error = gate
            .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be positive"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        server.abort();
    }

    #[tokio::test]
    async fn nonfinal_recovery_stops_at_the_wallet_attempt_budget() {
        for state in [None, Some(json!({"chainlock": false}))] {
            let (client, calls, server) = recovery_rpc(state, false).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                bitcoin_da_finality_timeout: std::time::Duration::from_millis(1),
                bitcoin_da_finality_poll_interval: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let result = gate
                .wait_for_blob_finality(&client, ABC_HASH, BlobFinalityWaitContext::GatewayEdgeRef)
                .await;
            assert!(result.unwrap_err().to_string().contains("limit exhausted"));
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }

    #[tokio::test]
    async fn failed_wallet_calls_consume_budget_across_gate_restarts_and_contexts() {
        let (client, calls, server) = recovery_rpc(None, true).await;
        let dir = tempfile::tempdir().unwrap();
        for attempt in 0..3 {
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 2,
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let context = if attempt == 0 {
                BlobFinalityWaitContext::OwnBatch { batch_number: 1 }
            } else {
                BlobFinalityWaitContext::GatewayEdgeRef
            };
            let hash = if attempt == 0 {
                ABC_HASH.to_owned()
            } else {
                format!("0x{}", ABC_HASH.to_uppercase())
            };
            let error = gate
                .republish_blob_after_timeout(&client, &hash, context)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(if attempt < 2 {
                "failed to republish"
            } else {
                "limit exhausted"
            }));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn confirmed_wait_and_finalized_success_need_no_republication_budget() {
        for finalized in [false, true] {
            let (client, calls, server) =
                recovery_rpc(Some(json!({"height": 10, "chainlock": finalized})), false).await;
            let dir = tempfile::tempdir().unwrap();
            let config = BatcherConfig {
                bitcoin_da_max_republish_attempts: 0,
                bitcoin_da_finality_timeout: std::time::Duration::ZERO,
                bitcoin_da_finality_poll_interval: std::time::Duration::from_millis(1),
                ..Default::default()
            };
            let gate = BitcoinDaFinalityGate::new(
                config,
                BitcoinDaStatusStorage::new(dir.path()).unwrap(),
                false,
            );
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                gate.wait_for_blob_finality(
                    &client,
                    ABC_HASH,
                    BlobFinalityWaitContext::GatewayEdgeRef,
                ),
            )
            .await;
            if finalized {
                result.unwrap().unwrap();
            } else {
                assert!(result.is_err());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn disabled_gateway_recovery_and_storage_failure_cannot_call_wallet() {
        let (client, calls, server) = recovery_rpc(None, false).await;
        let dir = tempfile::tempdir().unwrap();
        for disabled in [true, false] {
            let config = BatcherConfig {
                bitcoin_da_gateway_l1_republish_enabled: !disabled,
                ..Default::default()
            };
            let storage_path = dir
                .path()
                .join(if disabled { "disabled" } else { "broken" });
            let storage = BitcoinDaStatusStorage::new(&storage_path).unwrap();
            if !disabled {
                std::fs::remove_dir(&storage_path).unwrap();
                std::fs::write(&storage_path, b"not a directory").unwrap();
            }
            let gate = BitcoinDaFinalityGate::new(config, storage, false);
            assert!(
                gate.republish_blob_after_timeout(
                    &client,
                    ABC_HASH,
                    BlobFinalityWaitContext::GatewayEdgeRef
                )
                .await
                .is_err()
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    // SYSCOIN: Exercise the wallet boundary through the real client, including its archive
    // fallback, so a check moved after publication cannot silently regress this protection.
    #[tokio::test]
    async fn recovery_authenticates_blob_before_wallet_publication() {
        let mut cases = vec![
            (b"abc".to_vec(), ABC_HASH.to_owned(), ABC_HASH, None, 1),
            (b"616263".to_vec(), ABC_HASH.to_owned(), ABC_HASH, None, 1),
            (
                b"616263".to_vec(),
                HEX_LOOKING_HASH.to_owned(),
                HEX_LOOKING_HASH,
                None,
                1,
            ),
            (
                b"abc".to_vec(),
                format!("0x{}", ABC_HASH.to_uppercase()),
                ABC_HASH,
                None,
                1,
            ),
            (
                b"corrupt".to_vec(),
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                b"363136323633".to_vec(),
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                Vec::new(),
                hex::encode(Blake2s256::digest([])),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                vec![0; MAX_BLOB_SIZE + 1],
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                vec![b'0'; 2 * MAX_BLOB_SIZE + 1],
                ABC_HASH.to_owned(),
                ABC_HASH,
                Some("invalid size"),
                0,
            ),
            (
                b"abc".to_vec(),
                "1234".to_owned(),
                ABC_HASH,
                Some("recovered Bitcoin DA hash mismatch"),
                0,
            ),
            (
                b"abc".to_vec(),
                ABC_HASH.to_owned(),
                "wrong-wallet-result",
                Some("republished Bitcoin DA hash mismatch"),
                1,
            ),
        ];
        cases.extend(
            [
                b"61626".to_vec(),
                b"0x616263".to_vec(),
                b"616263\n".to_vec(),
                b" 616263".to_vec(),
                b"\"616263\"".to_vec(),
                br#"{"data":"616263"}"#.to_vec(),
                vec![255],
                b"deadbeef".to_vec(),
            ]
            .into_iter()
            .map(|blob| {
                (
                    blob,
                    ABC_HASH.to_owned(),
                    ABC_HASH,
                    Some("recovered Bitcoin DA hash mismatch"),
                    0,
                )
            }),
        );
        for (blob, expected_hash, wallet_hash, expected_error, expected_calls) in cases {
            let wallet_calls = Arc::new(AtomicUsize::new(0));
            let calls = wallet_calls.clone();
            let expected_rpc_data = if expected_hash == HEX_LOOKING_HASH {
                "363136323633"
            } else {
                "616263"
            };
            let app = Router::new().fallback(
                get(move || {
                    let blob = blob.clone();
                    async move {
                        ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], blob)
                    }
                })
                .post(move |Json(request): Json<Value>| {
                    let calls = calls.clone();
                    async move {
                        if request["method"] == "syscoincreatenevmblob" {
                            calls.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(request["params"], json!([expected_rpc_data, true, "blake2s"]));
                            Json(json!({"id": 1, "result": {"versionhash": wallet_hash}}))
                        } else {
                            assert_eq!(request["method"], "getnevmblobdata");
                            Json(json!({"id": 1, "error": {"code": -32602, "message": "missing blob"}}))
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = SyscoinClient::new(
                &url,
                "user",
                "password",
                &url,
                Some(std::time::Duration::from_secs(2)),
                "test",
            )
            .unwrap();
            let storage_dir = tempfile::tempdir().unwrap();
            let gate = BitcoinDaFinalityGate::new(
                BatcherConfig::default(),
                BitcoinDaStatusStorage::new(storage_dir.path()).unwrap(),
                false,
            );
            let result = gate
                .republish_blob_after_timeout(
                    &client,
                    &expected_hash,
                    BlobFinalityWaitContext::OwnBatch { batch_number: 1 },
                )
                .await;
            server.abort();
            match expected_error {
                Some(message) => assert!(result.unwrap_err().to_string().contains(message)),
                None => result.unwrap(),
            }
            assert_eq!(wallet_calls.load(Ordering::SeqCst), expected_calls);
            assert_eq!(
                std::fs::read_dir(storage_dir.path()).unwrap().count(),
                expected_calls,
                "invalid recovery must not consume a durable attempt"
            );
            if expected_calls != 0 {
                let canonical_hash = expected_hash
                    .strip_prefix("0x")
                    .unwrap_or(&expected_hash)
                    .to_ascii_lowercase();
                let reservation = storage_dir
                    .path()
                    .join(format!("republish_{canonical_hash}_1"));
                let metadata = std::fs::symlink_metadata(reservation).unwrap();
                assert!(metadata.is_file());
                assert_eq!(metadata.len(), 0);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
                    assert_eq!(metadata.nlink(), 1);
                }
            }
        }
    }
}
