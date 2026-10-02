//! Real, bounded fee-payer transactions for the explicit localhost component fixture.

use crate::assert_traits::DEFAULT_TIMEOUT;
use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256, address};
use alloy::providers::{Provider, ProviderBuilder};
use anyhow::{Context, ensure};
use zksync_os_operator_signer::SignerConfig;
use zksync_os_server::config::Config;

const TRACKER: Address = address!("0000000000000000000000000000000000010010");
const VAULT: Address = address!("0000000000000000000000000000000000010004");
const TOKEN_WEI: u128 = 1_000_000_000_000_000_000;
const OPERATIONS: u64 = 5;
const GAS_LIMIT: u64 = 500_000;

alloy::sol! {
    #[sol(rpc)]
    interface ComponentFeeTracker {
        function gatewaySettlementFee() external view returns (uint256);
        function wrappedZKToken() external view returns (address);
        function settlementFeePayerAgreement(address payer, uint256 chainId) external view returns (bool);
        function setSettlementFeePayerAgreement(uint256 chainId, bool agreed) external;
    }

    #[sol(rpc)]
    interface ComponentTokenVault {
        function WETH_TOKEN() external view returns (address);
    }

    #[sol(rpc)]
    interface ComponentWrappedToken {
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function deposit() external payable;
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

fn validate_endpoint(endpoint: &str, chain_id: u64) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(endpoint)?;
    ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_some()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "component fee payer requires an explicit, credential-free localhost HTTP endpoint"
    );
    ensure!(
        matches!(chain_id, 6565 | 6566),
        "component fee payer requires a registered component edge chain"
    );
    Ok(())
}

fn fee_target(fee: U256) -> anyhow::Result<U256> {
    let target = fee
        .checked_mul(U256::from(OPERATIONS))
        .context("component settlement-fee target overflow")?;
    ensure!(
        !target.is_zero() && target <= U256::from(TOKEN_WEI),
        "component settlement-fee target must be positive and at most one native token"
    );
    Ok(target)
}

fn native_requirement(deficit: U256, max_fee: u128) -> anyhow::Result<U256> {
    ensure!(
        deficit <= U256::from(TOKEN_WEI) && max_fee > 0,
        "component provisioning exceeds its wrap bound or has a zero gas price"
    );
    // All three transactions are budgeted even when an existing balance or
    // approval lets us omit one. These operands fit in U256 without overflow.
    let gas_budget = U256::from(3 * GAS_LIMIT) * U256::from(max_fee);
    ensure!(
        gas_budget <= U256::from(TOKEN_WEI),
        "component provisioning gas liability exceeds one native token"
    );
    Ok(deficit + gas_budget + U256::from(TOKEN_WEI))
}

/// Must run before the edge's execute sender starts using this signer on Gateway.
pub(crate) async fn prepare(gateway_rpc_url: &str, config: &Config) -> anyhow::Result<()> {
    tokio::time::timeout(DEFAULT_TIMEOUT, prepare_inner(gateway_rpc_url, config))
        .await
        .context("component settlement-fee provisioning timed out")?
}

async fn prepare_inner(gateway_rpc_url: &str, config: &Config) -> anyhow::Result<()> {
    let chain_id = config
        .genesis_config
        .chain_id
        .context("component edge is missing chain id")?;
    validate_endpoint(gateway_rpc_url, chain_id)?;
    let signer = config
        .gateway_sender_config
        .operator_execute_sk
        .as_ref()
        .context("component edge is missing its Gateway execute signer")?;
    ensure!(
        matches!(signer, SignerConfig::Local(_)),
        "component fee-payer provisioning does not use remote signers"
    );
    let mut wallet = EthereumWallet::default();
    let payer = signer.register_with_wallet(&mut wallet).await?;
    wallet.set_default_signer(payer)?;
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect(gateway_rpc_url)
        .await?;
    ensure!(
        provider.get_chain_id().await? == 57001,
        "component fee payer requires the registered Gateway chain"
    );
    ensure!(
        provider.get_transaction_count(payer).await?
            == provider.get_transaction_count(payer).pending().await?,
        "component execute signer has pending Gateway transactions"
    );
    ensure!(
        !provider.get_code_at(TRACKER).await?.is_empty()
            && !provider.get_code_at(VAULT).await?.is_empty(),
        "component Gateway fee contracts are missing"
    );
    let tracker = ComponentFeeTracker::new(TRACKER, &provider);
    let fee = tracker.gatewaySettlementFee().call().await?;
    let target = fee_target(fee)?;
    let wrapped_address = tracker.wrappedZKToken().call().await?;
    ensure!(
        wrapped_address != Address::ZERO
            && wrapped_address
                == ComponentTokenVault::new(VAULT, &provider)
                    .WETH_TOKEN()
                    .call()
                    .await?
            && !provider.get_code_at(wrapped_address).await?.is_empty(),
        "component Gateway wrapped-token identity mismatch"
    );
    let wrapped = ComponentWrappedToken::new(wrapped_address, &provider);
    let balance = wrapped.balanceOf(payer).call().await?;
    let allowance = wrapped.allowance(payer, TRACKER).call().await?;
    let agreed = tracker
        .settlementFeePayerAgreement(payer, U256::from(chain_id))
        .call()
        .await?;
    let deficit = target.saturating_sub(balance);
    let max_fee = provider
        .get_gas_price()
        .await?
        .checked_mul(2)
        .context("component provisioning gas-price overflow")?;
    ensure!(
        provider.get_balance(payer).await? >= native_requirement(deficit, max_fee)?,
        "component execute signer cannot fund bounded provisioning and retained reserve"
    );

    if !deficit.is_zero() {
        let receipt = wrapped
            .deposit()
            .value(deficit)
            .gas(GAS_LIMIT)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(0)
            .send()
            .await?
            .with_timeout(Some(DEFAULT_TIMEOUT))
            .get_receipt()
            .await?;
        ensure!(receipt.status(), "component wrapped-token deposit reverted");
    }
    if allowance != target {
        let receipt = wrapped
            .approve(TRACKER, target)
            .gas(GAS_LIMIT)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(0)
            .send()
            .await?
            .with_timeout(Some(DEFAULT_TIMEOUT))
            .get_receipt()
            .await?;
        ensure!(receipt.status(), "component fee-payer approval reverted");
    }
    if !agreed {
        let receipt = tracker
            .setSettlementFeePayerAgreement(U256::from(chain_id), true)
            .gas(GAS_LIMIT)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(0)
            .send()
            .await?
            .with_timeout(Some(DEFAULT_TIMEOUT))
            .get_receipt()
            .await?;
        ensure!(receipt.status(), "component fee-payer agreement reverted");
    }

    ensure!(
        tracker.gatewaySettlementFee().call().await? == fee
            && wrapped.balanceOf(payer).call().await? >= target
            && wrapped.allowance(payer, TRACKER).call().await? == target
            && tracker
                .settlementFeePayerAgreement(payer, U256::from(chain_id))
                .call()
                .await?
            && provider.get_balance(payer).await? >= U256::from(TOKEN_WEI),
        "component fee-payer post-transaction readback failed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_confined_to_registered_local_component_chains() {
        for endpoint in ["http://localhost:3050", "http://127.0.0.1:3050"] {
            assert!(validate_endpoint(endpoint, 6565).is_ok());
            assert!(validate_endpoint(endpoint, 6566).is_ok());
            assert!(validate_endpoint(endpoint, 57001).is_err());
        }
        for endpoint in [
            "https://localhost:3050",
            "http://example.com:3050",
            "http://localhost",
            "http://user:pass@localhost:3050",
            "http://localhost:3050/path",
            "http://localhost:3050/?query",
            "http://localhost:3050/#fragment",
        ] {
            assert!(validate_endpoint(endpoint, 6565).is_err());
        }
    }

    #[test]
    fn fee_target_is_bounded_without_overflow_or_zero_fee_bypass() {
        assert_eq!(
            fee_target(U256::from(1_000_000_000)).unwrap(),
            U256::from(5_000_000_000u64)
        );
        assert_eq!(
            fee_target(U256::from(TOKEN_WEI / 5)).unwrap(),
            U256::from(TOKEN_WEI)
        );
        assert!(fee_target(U256::ZERO).is_err());
        assert!(fee_target(U256::from(TOKEN_WEI / 5 + 1)).is_err());
        assert!(fee_target(U256::MAX).is_err());
    }

    #[test]
    fn native_liability_retains_reserve_and_all_three_transaction_budgets() {
        assert_eq!(
            native_requirement(U256::from(5), 2).unwrap(),
            U256::from(TOKEN_WEI + 3_000_005)
        );
        assert!(native_requirement(U256::from(TOKEN_WEI + 1), 2).is_err());
        assert!(native_requirement(U256::ZERO, 0).is_err());
        assert!(native_requirement(U256::ZERO, TOKEN_WEI).is_err());
        assert!(native_requirement(U256::ZERO, u128::MAX).is_err());
    }
}
