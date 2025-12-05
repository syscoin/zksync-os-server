use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256, address};
use alloy::providers::{DynProvider, Provider, ProviderBuilder, WalletProvider};
use alloy::signers::local::PrivateKeySigner;

use crate::contracts::{DummyFactory, ModExp};
use alloy::rpc::types::{TransactionInput, TransactionReceipt, TransactionRequest};
use std::env;

mod contracts;

async fn close_block(provider: &DynProvider, from: Address) -> anyhow::Result<TransactionReceipt> {
    // Prepare a dummy transaction to trigger block closure
    let tx_req = alloy::rpc::types::TransactionRequest::default()
        .from(from) // Use the second wallet
        .to(from); // Self-transfer

    // Send the transaction
    let pending_tx = provider.send_transaction(tx_req).await?;
    let tx_hash = *pending_tx.tx_hash();
    println!("Sent dummy tx to close block: {tx_hash:?}");

    // Wait for inclusion
    let receipt = pending_tx.get_receipt().await?;
    println!("Block closed: {:?}", receipt.block_number);

    Ok(receipt)
}

async fn deploy_dummy_factory(provider: &DynProvider, from: Address) -> anyhow::Result<Address> {
    let contract_address = DummyFactory::deploy_builder(provider).from(from).deploy().await?;

    println!("Deployed dummy factory at: {:?}", contract_address);
    Ok(contract_address)
}

async fn deploy_modexp_helper(provider: &DynProvider, from: Address) -> anyhow::Result<Address> {
    let contract_address = ModExp::deploy_builder(provider).from(from).deploy().await?;

    println!("Deployed modexp helper at: {:?}", contract_address);
    Ok(contract_address)
}

async fn gen_requests_same_transfer(
    provider: &DynProvider,
    from: Address,
    count: usize,
) -> anyhow::Result<Vec<TransactionRequest>> {
    const TO: Address = address!("0x1000000000000000000000000000000000000000");
    const VALUE: U256 = U256::ONE;

    let mut requests = Vec::new();
    let mut nonce = provider.get_transaction_count(from).await?;
    for _ in 0..count {
        let tx_req = TransactionRequest::default()
            .from(from)
            .to(TO)
            .value(VALUE)
            .nonce(nonce);
        nonce += 1;
        requests.push(tx_req);
    }
    Ok(requests)
}

async fn gen_requests_random_transfer(
    provider: &DynProvider,
    from: Address,
    count: usize,
) -> anyhow::Result<Vec<TransactionRequest>> {
    let mut requests = Vec::new();
    let mut nonce = provider.get_transaction_count(from).await?;
    for _ in 0..count {
        let tx_req = TransactionRequest::default()
            .from(from)
            .to(Address::random())
            .value(U256::random() % U256::from(1_000_000u64))
            .nonce(nonce);
        nonce += 1;
        requests.push(tx_req);
    }
    Ok(requests)
}

async fn gen_requests_factory(
    provider: &DynProvider,
    from: Address,
    factory_address: Address,
    count: usize,
) -> anyhow::Result<Vec<TransactionRequest>> {
    let mut requests = Vec::new();
    let mut nonce = provider.get_transaction_count(from).await?;
    let factory = DummyFactory::new(factory_address, provider.clone());
    for _ in 0..count {
        let calldata = factory
            .createDummy(U256::random() % U256::from(1_000_000u64))
            .calldata()
            .clone();
        let tx_req = TransactionRequest::default()
            .from(from)
            .to(factory_address)
            .input(TransactionInput::both(calldata))
            .nonce(nonce);
        nonce += 1;
        requests.push(tx_req);
    }
    Ok(requests)
}

async fn gen_requests_modexp(
    provider: &DynProvider,
    from: Address,
    modexp_helper_address: Address,
    count: usize,
) -> anyhow::Result<Vec<TransactionRequest>> {
    let mut requests = Vec::new();
    let mut nonce = provider.get_transaction_count(from).await?;
    let modexp = ModExp::new(modexp_helper_address, provider.clone());
    for _ in 0..count {
        let base: [u8; 100] = rand::random();
        let exp: [u8; 100] = rand::random();
        let modul: [u8; 100] = rand::random();
        let calldata = modexp
            .modexp(base.to_vec().into(), exp.to_vec().into(), modul.to_vec().into())
            .calldata()
            .clone();
        let tx_req = TransactionRequest::default()
            .from(from)
            .to(modexp_helper_address)
            .input(TransactionInput::both(calldata))
            .nonce(nonce);
        nonce += 1;
        requests.push(tx_req);
    }
    Ok(requests)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().into_iter().collect();
    let flow = args.get(1).unwrap();
    let txs_count: usize = args.get(2).unwrap().parse().unwrap();

    // Read env vars
    let rpc_url = env::var("RPC_URL").expect("RPC_URL env var must be set");
    let pk_hex =
        env::var("PRIVATE_KEY").expect("PRIVATE_KEY env var must be set (0x-prefixed hex)");
    let pk_hex_close =
        env::var("PRIVATE_KEY_CLOSE").expect("PRIVATE_KEY env var must be set (0x-prefixed hex)");

    // Parse private key
    let signer: PrivateKeySigner = pk_hex
        .parse()
        .expect("Failed to parse PRIVATE_KEY as hex private key");

    let signer_close: PrivateKeySigner = pk_hex_close
        .parse()
        .expect("Failed to parse PRIVATE_KEY_CLOSE as hex private key");

    let from = signer.address();
    let from_close = signer_close.address();
    println!("Using address: {from:?}");
    println!("Using address close: {from_close:?}");

    // Build wallet & provider
    let wallet1 = EthereumWallet::from(signer);

    let mut provider = ProviderBuilder::new()
        .wallet(wallet1)
        .connect_http(rpc_url.parse()?);
    provider.wallet_mut().register_signer(signer_close);
    let provider = provider.erased();

    // Preprocessing.
    let mut factory_address = None;
    let mut modexp_helper_address = None;
    match flow.as_str() {
        "same_transfer" | "random_transfer" => {}
        "factory" => {
            factory_address = Some(deploy_dummy_factory(&provider, from_close).await?);
        }
        "modexp" => {
            modexp_helper_address = Some(deploy_modexp_helper(&provider, from_close).await?);
        }
        _ => {
            panic!("Unknown flow: {}", flow);
        }
    }
    close_block(&provider, from_close).await?;

    // Processing
    let requests = match flow.as_str() {
        "same_transfer" => gen_requests_same_transfer(&provider, from, txs_count).await?,
        "random_transfer" => gen_requests_random_transfer(&provider, from, txs_count).await?,
        "factory" => {
            gen_requests_factory(&provider, from, factory_address.unwrap(), txs_count).await?
        }
        "modexp" => {
            gen_requests_modexp(&provider, from, modexp_helper_address.unwrap(), txs_count).await?
        }
        _ => {
            panic!("Unknown flow: {}", flow);
        }
    };
    let mut hashes = Vec::new();
    for req in requests {
        let pending_tx = provider.send_transaction(req).await?;
        let tx_hash = *pending_tx.tx_hash();
        hashes.push(tx_hash);
        // println!("Sent tx: {tx_hash:?}");
    }
    let close_receipt = close_block(&provider, from_close).await?;

    for hash in &hashes {
        let tx_receipt = provider.get_transaction_receipt(*hash).await?.unwrap();
        assert!(tx_receipt.status());
        assert_eq!(tx_receipt.block_number, close_receipt.block_number);
    }
    let block = provider
        .get_block(close_receipt.block_number.unwrap().into())
        .await?
        .unwrap();
    assert_eq!(block.transactions.len(), hashes.len() + 1);

    println!("Main block for {:?}: {:?}", (flow, txs_count), close_receipt.block_number);

    Ok(())
}
