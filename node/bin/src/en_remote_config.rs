use crate::config::GenesisConfig;
use alloy::primitives::Address;
use anyhow::Context;
use jsonrpsee::http_client::HttpClient;
use std::sync::Arc;
use zksync_os_genesis::{FileGenesisInputSource, GenesisInput, GenesisInputSource};
use zksync_os_rpc_api::eth::EthApiClient;
use zksync_os_rpc_api::zks::ZksApiClient;

/// Returns
/// (bridgehub_address, bytecode_supplier_address, chain_id, genesis_input_source)
pub async fn load_remote_config(
    main_node_rpc_url: &str,
    en_local_genesis_config: &GenesisConfig,
) -> anyhow::Result<(Address, Address, u64, Arc<dyn GenesisInputSource>)> {
    let main_node_rpc_client =
        jsonrpsee::http_client::HttpClientBuilder::new().build(main_node_rpc_url)?;

    let remote_bridgehub_address = main_node_rpc_client.get_bridgehub_contract().await?;
    if let Some(local_bridgehub_address) = en_local_genesis_config.bridgehub_address {
        anyhow::ensure!(
            remote_bridgehub_address == local_bridgehub_address,
            "Bridgehub address mismatch: remote = {remote_bridgehub_address}, local = {local_bridgehub_address}",
        );
    }

    let bytecode_supplier_address = match main_node_rpc_client
        .get_bytecode_supplier_contract()
        .await
    {
        Ok(result) => {
            if let Some(local_bytecode_supplier_address) =
                en_local_genesis_config.bytecode_supplier_address
            {
                anyhow::ensure!(
                    result == local_bytecode_supplier_address,
                    "Bytecode Supplier address mismatch: remote = {result}, local = {local_bytecode_supplier_address}",
                );
            }
            result
        }
        // todo: remove when `main_node_rpc_client.get_bytecode_supplier_contract()` is deployed everywhere
        Err(_) => {
            tracing::info!(
                "Cannot read bytecode supplier contract address from the Main Node. This is expected if Main Node runs an older version. Using local config value instead..."
            );
            en_local_genesis_config
                .bytecode_supplier_address
                .context("`genesis_bytecode_supplier_address` config must be provided when running against an older version of Main Node (`main_node_rpc_client.get_bytecode_supplier_contract()` is not available)")?
        }
    };

    let remote_chain_id: u64 = u64::from_be_bytes(
        main_node_rpc_client
            .chain_id()
            .await?
            .context("missing chain_id")?
            .to_be_bytes(),
    );
    if let Some(local_chain_id) = en_local_genesis_config.chain_id {
        anyhow::ensure!(
            remote_chain_id == local_chain_id,
            "chain id mismatch: remote = {remote_chain_id}, local = {local_chain_id}",
        );
    }

    let main_node_genesis_input_source =
        Arc::new(MainNodeGenesisInputSource::new(main_node_rpc_client));
    let genesis_input_source: Arc<dyn GenesisInputSource> =
        if let Some(local_genesis_path) = en_local_genesis_config.genesis_input_path.clone() {
            let remote_genesis_input = main_node_genesis_input_source.genesis_input().await?;
            let local_genesis_input = FileGenesisInputSource::new(local_genesis_path)
                .genesis_input()
                .await?;

            let remote_json = serde_json::to_string(&remote_genesis_input)?;
            let local_json = serde_json::to_string(&local_genesis_input)?;

            anyhow::ensure!(
                local_genesis_input == remote_genesis_input,
                "Genesis input mismatch: remote = {remote_json}, local = {local_json}",
            );

            // SYSCOIN: Bind all later lazy genesis initialization to the value that was
            // verified against the main node, rather than refetching mutable remote RPC data.
            Arc::new(CachedGenesisInputSource::new(local_genesis_input))
        } else {
            main_node_genesis_input_source
        };

    Ok((
        remote_bridgehub_address,
        bytecode_supplier_address,
        remote_chain_id,
        genesis_input_source,
    ))
}

#[derive(Debug)]
struct CachedGenesisInputSource {
    genesis_input: GenesisInput,
}

impl CachedGenesisInputSource {
    fn new(genesis_input: GenesisInput) -> Self {
        Self { genesis_input }
    }
}

#[async_trait::async_trait]
impl GenesisInputSource for CachedGenesisInputSource {
    async fn genesis_input(&self) -> anyhow::Result<GenesisInput> {
        Ok(self.genesis_input.clone())
    }
}

#[derive(Debug)]
pub struct MainNodeGenesisInputSource {
    rpc_client: HttpClient,
}

impl MainNodeGenesisInputSource {
    pub fn new(rpc_client: HttpClient) -> Self {
        Self { rpc_client }
    }
}

#[async_trait::async_trait]
impl GenesisInputSource for MainNodeGenesisInputSource {
    async fn genesis_input(&self) -> anyhow::Result<zksync_os_genesis::GenesisInput> {
        let genesis = self.rpc_client.get_genesis().await?;
        Ok(genesis)
    }
}

#[cfg(test)]
mod tests {
    use super::{CachedGenesisInputSource, GenesisInputSource};
    use alloy::primitives::B256;
    use zksync_os_genesis::GenesisInput;

    #[tokio::test]
    async fn cached_genesis_input_source_returns_validated_input() {
        let genesis_input = GenesisInput {
            initial_contracts: Vec::new(),
            additional_storage: Default::default(),
            additional_storage_raw: Vec::new(),
            additional_preimages: Vec::new(),
            genesis_root: B256::repeat_byte(0x01),
        };
        let source = CachedGenesisInputSource::new(genesis_input.clone());

        assert_eq!(source.genesis_input().await.unwrap(), genesis_input);
        assert_eq!(source.genesis_input().await.unwrap(), genesis_input);
    }
}
