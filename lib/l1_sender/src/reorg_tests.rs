use super::*;
use crate::commands::commit::CommitCommand;
use crate::config::L1SenderConfig;
use alloy::consensus::{Receipt, ReceiptEnvelope, ReceiptWithBloom};
use alloy::network::EthereumWallet;
use alloy::primitives::U64;
use alloy::providers::ProviderBuilder;
use alloy::rpc::json_rpc::ErrorPayload;
use alloy::rpc::types::{Block, Header};
use alloy::signers::k256::ecdsa::SigningKey;
use alloy::transports::mock::Asserter;
use zksync_os_operator_signer::SignerConfig;
use zksync_os_provider::NodeProvider;

async fn provider(asserter: &Asserter) -> NodeProvider {
    let header: Header = Header::default();
    asserter.push_success(&header);
    asserter.push_success(&header);
    asserter.push_failure(ErrorPayload {
        code: METHOD_NOT_FOUND_CODE,
        message: "eth_config unavailable".into(),
        data: None,
    });
    asserter.push_success(&U64::from(zksync_os_provider::ANVIL_L1_CHAIN_ID));
    NodeProvider::new(
        ProviderBuilder::new()
            .disable_recommended_fillers()
            .wallet(EthereumWallet::default())
            .connect_mocked_client(asserter.clone()),
    )
    .await
    .expect("mock provider")
}

fn sender(provider: NodeProvider, pipelined: bool, liveness: bool) -> L1Sender<CommitCommand> {
    L1Sender {
        provider,
        config: L1SenderConfig {
            operator_signer: SignerConfig::Local(SigningKey::from_slice(&[1; 32]).unwrap()),
            fee_config: L1SenderFeeConfig {
                max_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
                max_priority_fee_per_gas_wei: SYSCOIN_L1_PRIORITY_FEE_FLOOR_WEI,
                max_fee_per_gas_replacement_multiplier: 2.0,
                max_priority_fee_per_gas_replacement_multiplier: 2.0,
            },
            force_transaction_resubmission: false,
            command_limit: 1,
            pipelining_enabled: pipelined,
            poll_interval: Duration::from_millis(1),
            transaction_timeout: Duration::from_secs(1),
            tx_liveness_poll_interval: Duration::from_nanos(1),
            tx_liveness_max_missing_polls: u32::from(liveness),
            gateway_da_admission_retry_timeout: Duration::from_secs(1),
            gateway_da_admission_retry_interval: Duration::from_millis(1),
            required_confirmations: 2,
            nonce_error_max_attempts: 1,
            nonce_error_retry_backoff: Duration::from_millis(1),
            phantom_data: Default::default(),
        },
        to_address: Address::ZERO,
        gateway: false,
        commit_submitted_tx: None,
        sl_block_number: 42,
    }
}

fn receipt(tx: B256, block_hash: B256, height: u64) -> TransactionReceipt {
    TransactionReceipt {
        inner: ReceiptEnvelope::Eip1559(ReceiptWithBloom {
            receipt: Receipt {
                status: true.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
        transaction_hash: tx,
        transaction_index: Some(0),
        block_hash: Some(block_hash),
        block_number: Some(height),
        gas_used: 21_000,
        effective_gas_price: 1,
        blob_gas_used: None,
        blob_gas_price: None,
        from: Address::with_last_byte(1),
        to: Some(Address::ZERO),
        contract_address: None,
    }
}

fn block(hash: B256, height: u64) -> Block {
    let mut block: Block = Block::default();
    block.header.hash = hash;
    block.header.inner.number = height;
    block
}

fn resend() -> ConfirmationResend {
    ConfirmationResend {
        request: TransactionRequest {
            from: Some(Address::with_last_byte(1)),
            nonce: Some(7),
            ..Default::default()
        },
        raw_transactions: vec![],
    }
}

fn signed_resend() -> (B256, Bytes, ConfirmationResend) {
    // Public test key 0x0101... signs this EIP-1559 transaction for chain 31337.
    let raw = Bytes::from(alloy::primitives::hex::decode(
        "02f868827a6907824e20824e208252089400000000000000000000000000000000000000008080c001a0fd1d41851151787a744fcc7907b27c732a834610b1c8804f4cce9a8bd14a93f9a0201a8dd84874e987ea0166b7714219a6c9d2b3db94852fa9f3332028416694eb",
    ).unwrap());
    let hash = alloy::primitives::keccak256(&raw);
    let request = TransactionRequest {
        from: Some(
            "0x1a642f0e3c3af545e7acbd38b07251b3990914f1"
                .parse()
                .unwrap(),
        ),
        to: Some(Address::ZERO.into()),
        nonce: Some(7),
        gas: Some(21_000),
        input: Bytes::new().into(),
        ..Default::default()
    };
    (
        hash,
        raw.clone(),
        ConfirmationResend {
            request,
            raw_transactions: vec![(hash, raw)],
        },
    )
}

#[test]
fn simulation_prefix_stays_bounded_until_predecessors_confirm() {
    let mut prefix: std::collections::VecDeque<_> = (0..256)
        .map(|nonce| SimPrefixEntry {
            nonce,
            calldata: Bytes::new(),
            gas_limit: 21_000,
            fee_params: FeeParams {
                max_fee_per_gas: 20_000,
                max_priority_fee_per_gas: 20_000,
            },
        })
        .collect();
    // First inclusion does not advance the canonical confirmation floor.
    assert_eq!(pipelined::prune_confirmed_prefix(&mut prefix, 0), 0);
    assert_eq!(prefix.front().unwrap().nonce, 0);
    assert_eq!(pipelined::prune_confirmed_prefix(&mut prefix, 1), 1);
    assert_eq!(prefix.front().unwrap().nonce, 1);
    assert_eq!(pipelined::prune_confirmed_prefix(&mut prefix, 256), 256);
    assert!(prefix.is_empty());
}

#[tokio::test]
async fn confirms_any_previously_submitted_hash_on_the_canonical_chain() {
    for pipelined in [false, true] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let original = B256::with_last_byte(1);
        let replacement = B256::with_last_byte(2);
        let block_hash = B256::with_last_byte(3);
        a.push_success(&U64::from(11));
        a.push_success(&Option::<TransactionReceipt>::None);
        a.push_success(&Some(receipt(original, block_hash, 10)));
        a.push_success(&Some(block(block_hash, 10)));
        a.push_success(&U64::from(11));
        a.push_success(&Some(block(block_hash, 10)));
        let sender = sender(provider, pipelined, false);
        let confirmed = tokio::time::timeout(
            Duration::from_secs(2),
            sender.wait_for_confirmed_receipt(vec![replacement, original], resend()),
        )
        .await
        .expect("wait timed out")
        .expect("canonical original should confirm");
        assert_eq!(confirmed.transaction_hash, original);
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn orphaned_receipt_is_never_forwarded_and_later_canonical_receipt_confirms() {
    for pipelined in [false, true] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let tx = B256::with_last_byte(1);
        let orphan = B256::with_last_byte(2);
        let canonical = B256::with_last_byte(3);
        a.push_success(&U64::from(11));
        a.push_success(&Some(receipt(tx, orphan, 10)));
        a.push_success(&Some(block(canonical, 10)));
        a.push_success(&U64::from(12));
        a.push_success(&Some(receipt(tx, canonical, 10)));
        a.push_success(&Some(block(canonical, 10)));
        a.push_success(&U64::from(12));
        a.push_success(&Some(block(canonical, 10)));
        let sender = sender(provider, pipelined, false);
        let confirmed = tokio::time::timeout(
            Duration::from_secs(2),
            sender.wait_for_confirmed_receipt(vec![tx], resend()),
        )
        .await
        .expect("wait timed out")
        .expect("later canonical receipt should confirm");
        assert_eq!(confirmed.block_hash, Some(canonical));
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn block_changed_during_depth_check_is_retried_without_forwarding_stale_receipt() {
    for pipelined in [false, true] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let tx = B256::with_last_byte(1);
        let first = B256::with_last_byte(2);
        let second = B256::with_last_byte(3);
        a.push_success(&U64::from(11));
        a.push_success(&Some(receipt(tx, first, 10)));
        a.push_success(&Some(block(first, 10)));
        a.push_success(&U64::from(11));
        a.push_success(&Some(block(second, 10)));
        a.push_success(&U64::from(12));
        a.push_success(&Some(receipt(tx, second, 10)));
        a.push_success(&Some(block(second, 10)));
        a.push_success(&U64::from(12));
        a.push_success(&Some(block(second, 10)));
        let sender = sender(provider, pipelined, false);
        let confirmed = tokio::time::timeout(
            Duration::from_secs(2),
            sender.wait_for_confirmed_receipt(vec![tx], resend()),
        )
        .await
        .expect("wait timed out")
        .expect("changed block must be retried");
        assert_eq!(confirmed.block_hash, Some(second));
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn refuses_to_resend_after_sender_nonce_was_consumed() {
    for pipelined in [false, true] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let original = B256::with_last_byte(1);
        a.push_success(&U64::from(11));
        a.push_success(&Option::<TransactionReceipt>::None);
        a.push_success(&Option::<alloy::rpc::types::Transaction>::None);
        a.push_success(&U64::from(8));
        a.push_success(&Option::<TransactionReceipt>::None);
        let sender = sender(provider, pipelined, true);
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            sender.wait_for_confirmed_receipt(vec![original], resend()),
        )
        .await
        .expect("wait timed out")
        .expect_err("consumed nonce must fail closed");
        assert!(error.to_string().contains("nonce consumed"), "{error:#}");
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn missing_previously_mined_transaction_resends_saved_signed_hash() {
    for pipelined in [false, true] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let (tx, _raw, original) = signed_resend();
        let included = B256::with_last_byte(3);
        a.push_success(&U64::from(11));
        a.push_success(&Option::<TransactionReceipt>::None);
        a.push_success(&Option::<alloy::rpc::types::Transaction>::None);
        a.push_success(&U64::from(7));
        a.push_success(&U64::from(7));
        a.push_success(&tx);
        a.push_success(&U64::from(12));
        a.push_success(&Some(receipt(tx, included, 10)));
        a.push_success(&Some(block(included, 10)));
        a.push_success(&U64::from(12));
        a.push_success(&Some(block(included, 10)));
        let sender = sender(provider, pipelined, true);
        let confirmed = tokio::time::timeout(
            Duration::from_secs(2),
            sender.wait_for_confirmed_receipt(vec![tx], original),
        )
        .await
        .expect("wait timed out")
        .expect("saved transaction should be observed again");
        assert_eq!(confirmed.transaction_hash, tx);
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn refuses_unknown_pending_nonce_and_missing_signed_bytes() {
    for pipelined in [false, true] {
        for occupied in [false, true] {
            let a = Asserter::new();
            let provider = provider(&a).await;
            a.push_success(&U64::from(11));
            a.push_success(&Option::<TransactionReceipt>::None);
            a.push_success(&Option::<alloy::rpc::types::Transaction>::None);
            a.push_success(&U64::from(7));
            a.push_success(&U64::from(if occupied { 8 } else { 7 }));
            let sender = sender(provider, pipelined, true);
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                sender.wait_for_confirmed_receipt(vec![B256::with_last_byte(1)], resend()),
            )
            .await
            .expect("wait timed out")
            .expect_err("unverified replacement must fail closed");
            let expected = if occupied {
                "unknown pending transaction"
            } else {
                "signed raw transaction unavailable"
            };
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(a.read_q().is_empty());
        }
    }
}

#[tokio::test]
async fn raw_capture_rejects_bytes_for_a_different_hash() {
    let a = Asserter::new();
    let provider = provider(&a).await;
    a.push_success(&Some(Bytes::from_static(&[2, 1, 2, 3])));
    let sender = sender(provider, false, false);
    let error = sender
        .capture_raw_transaction(B256::with_last_byte(42), &resend().request)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("raw transaction hash mismatch"),
        "{error:#}"
    );
}

#[tokio::test]
async fn raw_capture_checks_real_signature_and_full_request_identity() {
    for field in [
        "ok", "sender", "nonce", "target", "value", "input", "gas", "chain",
    ] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let (hash, raw, mut original) = signed_resend();
        match field {
            "sender" => original.request.from = Some(Address::with_last_byte(7)),
            "nonce" => original.request.nonce = Some(8),
            "target" => original.request.to = Some(Address::with_last_byte(8).into()),
            "value" => original.request.value = Some(U256::from(1)),
            "input" => original.request.input = Bytes::from_static(&[1]).into(),
            "gas" => original.request.gas = Some(21_001),
            _ => {}
        }
        a.push_success(&Some(raw.clone()));
        a.push_success(&U64::from(if field == "chain" { 31338 } else { 31337 }));
        let sender = sender(provider, false, false);
        let result = sender
            .capture_raw_transaction(hash, &original.request)
            .await;
        if field == "ok" {
            assert_eq!(result.unwrap(), Some(raw));
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("raw transaction identity mismatch"),
                "{field}"
            );
        }
        assert!(a.read_q().is_empty());
    }
}

#[tokio::test]
async fn raw_capture_checks_real_signature_and_full_request_identity() {
    for field in [
        "ok", "sender", "nonce", "target", "value", "input", "gas", "chain",
    ] {
        let a = Asserter::new();
        let provider = provider(&a).await;
        let (hash, raw, mut original) = signed_resend();
        match field {
            "sender" => original.request.from = Some(Address::with_last_byte(7)),
            "nonce" => original.request.nonce = Some(8),
            "target" => original.request.to = Some(Address::with_last_byte(8).into()),
            "value" => original.request.value = Some(U256::from(1)),
            "input" => original.request.input = Bytes::from_static(&[1]).into(),
            "gas" => original.request.gas = Some(21_001),
            _ => {}
        }
        a.push_success(&Some(raw.clone()));
        a.push_success(&U64::from(if field == "chain" { 31338 } else { 31337 }));
        let sender = sender(provider, false, false);
        let result = sender
            .capture_raw_transaction(hash, &original.request)
            .await;
        if field == "ok" {
            assert_eq!(result.unwrap(), Some(raw));
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("raw transaction identity mismatch"),
                "{field}"
            );
        }
        assert!(a.read_q().is_empty());
    }
}
