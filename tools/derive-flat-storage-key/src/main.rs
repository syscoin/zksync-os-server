use alloy::primitives::ruint::aliases::B160;
use alloy::primitives::{Address, B256};
use clap::Parser;
use zk_ee_dev::common_structs::derive_flat_storage_key;
use zk_ee_dev::utils::Bytes32;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Derive zk_ee flat storage keys from an address and 32-byte storage slot"
)]
struct Args {
    /// 20-byte account / storage address, e.g. 0x0000000000000000000000000000000000010005
    address: Address,
    /// 32-byte storage slot key, e.g. 0x000...004
    key: B256,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let address = B160::from_be_bytes(args.address.into_array());
    let key = Bytes32::from_array(args.key.0);
    let flat_key = derive_flat_storage_key(&address, &key);

    println!("{flat_key:?}");
    Ok(())
}
