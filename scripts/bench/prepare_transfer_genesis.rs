use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{fs, io::Write, str::FromStr};
use zk_os_api::helpers::set_properties_balance;
use zk_os_basic_system::system_implementation::flat_storage_model::{
    ACCOUNT_PROPERTIES_STORAGE_ADDRESS, AccountProperties,
};
use zksync_os_genesis_gen::{InitialGenesisInput, build_genesis_root_hash};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "expected seed genesis, public sender address and new output file"
    );
    let mut genesis: Value = serde_json::from_slice(&fs::read(&args[1])?)?;
    let seed: InitialGenesisInput = serde_json::from_value(genesis.clone())?;
    let seed_root = build_genesis_root_hash(&seed)?;
    ensure!(
        genesis["genesis_root"] == json!(seed_root),
        "seed genesis root mismatch"
    );
    let address = Address::from_str(&args[2])?;
    let store: Address = ACCOUNT_PROPERTIES_STORAGE_ADDRESS.to_be_bytes().into();
    let mut slot = [0; 32];
    slot[12..].copy_from_slice(address.as_slice());
    let slot = B256::from(slot);
    let mut props = AccountProperties::default();
    set_properties_balance(&mut props, U256::from(10u64.pow(18)));
    let hash = B256::from(props.compute_hash().as_u8_array());
    let alloc = &mut genesis["additional_storage"][format!("{store:#x}")][format!("{slot:#x}")];
    ensure!(
        alloc.is_null(),
        "fixture account already has a genesis allocation"
    );
    *alloc = json!(hash);
    if genesis.get("additional_preimages").is_none() {
        genesis["additional_preimages"] = json!([]);
    }
    genesis["additional_preimages"]
        .as_array_mut()
        .context("invalid preimage array")?
        .push(json!([hash, hex::encode(props.encoding())]));
    let funded: InitialGenesisInput = serde_json::from_value(genesis.clone())?;
    let root = build_genesis_root_hash(&funded)?;
    ensure!(
        root != seed_root,
        "funded state must have a distinct commitment"
    );
    genesis["genesis_root"] = json!(root);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])?;
    file.write_all(&serde_json::to_vec_pretty(&genesis)?)?;
    file.sync_all()?;
    println!("source root {seed_root:#x}, funded root {root:#x}");
    Ok(())
}
