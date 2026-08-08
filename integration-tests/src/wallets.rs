use crate::config::ChainLayout;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct WalletEntry {
    pub private_key: String,
}

#[derive(Debug, Deserialize)]
struct ChainWallets {
    pub operator_prove_sk: WalletEntry,
}

fn wallets_path(layout: ChainLayout<'_>) -> PathBuf {
    PathBuf::from(
        std::env::var("WORKSPACE_DIR").expect("WORKSPACE_DIR environment variable is not set"),
    )
    .join("local-chains")
    .join(layout.protocol_version())
    .join("default")
    .join("wallets.yaml")
}

// SYSCOIN: Resolve the flat per-chain wallet files used by the pinned v31 fixture.
fn syscoin_wallets_path(layout: ChainLayout<'_>, chain_id: u64) -> PathBuf {
    PathBuf::from(
        std::env::var("WORKSPACE_DIR").expect("WORKSPACE_DIR environment variable is not set"),
    )
    .join("local-chains")
    .join(layout.protocol_version())
    .join("multi_chain")
    .join(format!("wallets_{chain_id}.yaml"))
}

/// Loads the private key holding REVERTER_ROLE on the ValidatorTimelock from the fixture's
/// `default/wallets.yaml`. zk-deployer grants that role to the chain's prove operator.
pub fn load_reverter_private_key(layout: ChainLayout<'_>, chain_id: u64) -> anyhow::Result<String> {
    let path = wallets_path(layout);
    let wallets: HashMap<String, serde_yaml::Value> =
        serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
    if let Some(chain) = wallets.get(&chain_id.to_string()) {
        let chain: ChainWallets = serde_yaml::from_value(chain.clone())?;
        return Ok(chain.operator_prove_sk.private_key);
    }

    // SYSCOIN: The pinned v31 multi-chain fixture predates zk-deployer's nested wallet format.
    // Its `operator` key holds REVERTER_ROLE, so support that layout without changing v32 files.
    let path = syscoin_wallets_path(layout, chain_id);
    let wallets: HashMap<String, WalletEntry> =
        serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
    Ok(wallets
        .get("operator")
        .ok_or_else(|| anyhow::anyhow!("no operator wallet for chain {chain_id} in {path:?}"))?
        .private_key
        .clone())
}
