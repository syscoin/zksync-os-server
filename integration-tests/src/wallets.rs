use crate::config::{ChainLayout, FixtureScope, fixture_backend};
use serde::Deserialize;
use std::collections::HashMap;
#[derive(Debug, Deserialize)]
struct WalletEntry {
    pub private_key: String,
}

#[derive(Debug, Deserialize)]
struct ChainWallets {
    pub operator_prove_sk: WalletEntry,
}

/// Loads the private key holding REVERTER_ROLE on the ValidatorTimelock from the fixture's
/// `default/wallets.yaml`. zk-deployer grants that role to the chain's prove operator.
pub fn load_reverter_private_key(layout: ChainLayout<'_>, chain_id: u64) -> anyhow::Result<String> {
    if layout.fixture_scope() == FixtureScope::AnvilComponentOnly {
        anyhow::ensure!(
            [6565, 6566, 57001].contains(&chain_id),
            "unknown component chain"
        );
        // Registration binds the fresh deployment's REVERTER_ROLE to this public
        // development account. Never load or publish generated operator wallets.
        layout.backend_inventory().map_err(anyhow::Error::msg)?;
        return Ok(fixture_backend::component::PUBLIC_ANVIL_REVERTER_KEY.into());
    }
    let path = layout.protocol_dir().join("default/wallets.yaml");
    let wallets: HashMap<String, serde_yaml::Value> =
        serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
    let chain: ChainWallets = serde_yaml::from_value(
        wallets
            .get(&chain_id.to_string())
            .ok_or_else(|| anyhow::anyhow!("no wallets for chain {chain_id} in {path:?}"))?
            .clone(),
    )?;
    Ok(chain.operator_prove_sk.private_key)
}
