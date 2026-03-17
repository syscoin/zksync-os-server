// The code in this file was copied from reth with some minor changes. Source:
// https://github.com/paradigmxyz/reth/blob/fcf58cb5acc2825e7c046f6741e90a8c5dab7847/crates/rpc/rpc-server-types/src/result.rs
#![allow(dead_code)]

//! Additional helpers for converting errors.

use crate::debug_impl::DebugError;
use crate::eth_call_handler::EthCallError;
use crate::eth_filter_impl::EthFilterError;
use crate::eth_impl::EthError;
use crate::tx_handler::{EthSendRawTransactionError, EthSendRawTransactionSyncError};
use crate::unstable_impl::UnstableError;
use crate::zks_impl::ZksError;
use alloy::primitives::Bytes;
use alloy::rpc::types::error::EthRpcErrorCode;
use alloy::sol_types::{ContractError, RevertReason};
use jsonrpsee::core::RpcResult;
use std::fmt;
use std::fmt::Display;

/// Helper trait to easily convert various `Result` types into [`RpcResult`]
pub trait ToRpcResult<Ok, Err>: Sized {
    /// Converts result to [`RpcResult`] by converting error variant to
    /// [`jsonrpsee::types::error::ErrorObject`]
    fn to_rpc_result(self) -> RpcResult<Ok>
    where
        Err: fmt::Display;
}

/// A macro that implements the `ToRpcResult` for a specific error type
#[macro_export]
macro_rules! impl_to_rpc_result {
    ($err:ty) => {
        impl<Ok> ToRpcResult<Ok, $err> for Result<Ok, $err> {
            fn to_rpc_result(self) -> RpcResult<Ok> {
                self.map_err(|err| $crate::result::internal_rpc_err(err.to_string()))
            }
        }
    };
}

impl<Ok> ToRpcResult<Ok, EthSendRawTransactionError> for Result<Ok, EthSendRawTransactionError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            // Backpressure: use TransactionRejected (-32003) so clients can distinguish
            // "node is overloaded, retry later" from -32603 "server has a bug".
            // Include a structured `data` field with machine-readable reason and retry hint.
            EthSendRawTransactionError::NotAcceptingTransactions(reason) => rpc_err_with_json_data(
                EthRpcErrorCode::TransactionRejected.code(),
                err.to_string(),
                reason.to_rpc_data(),
            ),
            EthSendRawTransactionError::PoolError(_) => {
                rpc_error_with_code(EthRpcErrorCode::TransactionRejected.code(), err.to_string())
            }
            // All other variants are client errors or internal bugs.
            _ => internal_rpc_err(err.to_string()),
        })
    }
}
impl_to_rpc_result!(EthFilterError);
impl_to_rpc_result!(EthError);
impl_to_rpc_result!(ZksError);
impl_to_rpc_result!(DebugError);
impl_to_rpc_result!(UnstableError);

impl<Ok> ToRpcResult<Ok, EthCallError> for Result<Ok, EthCallError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            EthCallError::Revert(revert) => rpc_err(
                EthRpcErrorCode::ExecutionError.code(),
                revert.to_string(),
                revert.output.as_ref().map(|out| out.as_ref()),
            ),
            err => internal_rpc_err(err.to_string()),
        })
    }
}

impl<Ok> ToRpcResult<Ok, EthSendRawTransactionSyncError>
    for Result<Ok, EthSendRawTransactionSyncError>
{
    fn to_rpc_result(self) -> RpcResult<Ok>
    where
        EthSendRawTransactionSyncError: Display,
    {
        self.map_err(|err| match err {
            err @ EthSendRawTransactionSyncError::Regular(_) => internal_rpc_err(err.to_string()),
            err @ EthSendRawTransactionSyncError::Timeout(_) => {
                // Code 4 is used as per EIP-7966 (see https://eips.ethereum.org/EIPS/eip-7966)
                rpc_error_with_code(4, err.to_string())
            }
        })
    }
}

/// Constructs an unimplemented JSON-RPC error.
pub fn unimplemented_rpc_err() -> jsonrpsee::types::error::ErrorObject<'static> {
    internal_rpc_err("unimplemented")
}

/// Constructs an invalid params JSON-RPC error.
pub fn invalid_params_rpc_err(
    msg: impl Into<String>,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    rpc_err(jsonrpsee::types::error::INVALID_PARAMS_CODE, msg, None)
}

/// Constructs an internal JSON-RPC error.
pub fn internal_rpc_err(msg: impl Into<String>) -> jsonrpsee::types::error::ErrorObject<'static> {
    rpc_err(jsonrpsee::types::error::INTERNAL_ERROR_CODE, msg, None)
}

/// Constructs an internal JSON-RPC error with data
pub fn internal_rpc_err_with_data(
    msg: impl Into<String>,
    data: &[u8],
) -> jsonrpsee::types::error::ErrorObject<'static> {
    rpc_err(
        jsonrpsee::types::error::INTERNAL_ERROR_CODE,
        msg,
        Some(data),
    )
}

/// Constructs an internal JSON-RPC error with code and message
pub fn rpc_error_with_code(
    code: i32,
    msg: impl Into<String>,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    rpc_err(code, msg, None)
}

/// Constructs a JSON-RPC error with a structured JSON object in the `data` field.
pub fn rpc_err_with_json_data(
    code: i32,
    msg: impl Into<String>,
    data: serde_json::Value,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    jsonrpsee::types::error::ErrorObject::owned(
        code,
        msg.into(),
        Some(
            jsonrpsee::core::to_json_raw_value(&data)
                .expect("serializing serde_json::Value can't fail"),
        ),
    )
}

/// Constructs a JSON-RPC error, consisting of `code`, `message` and optional `data`.
pub fn rpc_err(
    code: i32,
    msg: impl Into<String>,
    data: Option<&[u8]>,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    jsonrpsee::types::error::ErrorObject::owned(
        code,
        msg.into(),
        data.map(|data| {
            jsonrpsee::core::to_json_raw_value(&alloy::primitives::hex::encode_prefixed(data))
                .expect("serializing String can't fail")
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx_handler::EthSendRawTransactionError;
    use alloy::rpc::types::error::EthRpcErrorCode;
    use jsonrpsee::types::error::INTERNAL_ERROR_CODE;
    use zksync_os_types::{NotAcceptingReason, OverloadCause};

    #[test]
    fn not_accepting_overloaded_returns_32003() {
        let err =
            EthSendRawTransactionError::NotAcceptingTransactions(NotAcceptingReason::Overloaded {
                cause: OverloadCause::ProverQueueFull,
                retry_after_ms: 5_000,
            });
        let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
        assert_eq!(rpc_err.code(), EthRpcErrorCode::TransactionRejected.code());
    }

    #[test]
    fn not_accepting_block_production_disabled_returns_32003() {
        let err = EthSendRawTransactionError::NotAcceptingTransactions(
            NotAcceptingReason::BlockProductionDisabled,
        );
        let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
        assert_eq!(rpc_err.code(), EthRpcErrorCode::TransactionRejected.code());
    }

    #[test]
    fn decode_error_returns_internal_error() {
        let err = EthSendRawTransactionError::FailedToDecodeSignedTransaction;
        let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
        // Decode failure is a client error, not a backpressure signal
        assert_eq!(rpc_err.code(), INTERNAL_ERROR_CODE);
    }

    #[test]
    fn overloaded_error_data_contains_reason_and_retry() {
        let err =
            EthSendRawTransactionError::NotAcceptingTransactions(NotAcceptingReason::Overloaded {
                cause: OverloadCause::ProverQueueFull,
                retry_after_ms: 5_000,
            });
        let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
        let data_raw = rpc_err.data().expect("data field must be present");
        let data: serde_json::Value = serde_json::from_str(data_raw.get()).unwrap();
        assert_eq!(data["reason"], "prover_queue_full");
        assert_eq!(data["retry_after_ms"], 5000u64);
    }

    #[test]
    fn block_production_disabled_data_has_no_retry() {
        let err = EthSendRawTransactionError::NotAcceptingTransactions(
            NotAcceptingReason::BlockProductionDisabled,
        );
        let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
        let data_raw = rpc_err.data().expect("data field must be present");
        let data: serde_json::Value = serde_json::from_str(data_raw.get()).unwrap();
        assert_eq!(data["reason"], "block_production_disabled");
        assert!(data.get("retry_after_ms").is_none());
    }
}

/// Represents a reverted transaction and its output data.
///
/// Displays "execution reverted(: reason)?" if the reason is a string.
#[derive(Debug, Clone, thiserror::Error)]
pub struct RevertError {
    /// The transaction output data
    ///
    /// Note: this is `None` if output was empty
    output: Option<Bytes>,
}

impl RevertError {
    /// Wraps the output bytes
    ///
    /// Note: this is intended to wrap a VM output
    pub fn new(output: Bytes) -> Self {
        if output.is_empty() {
            Self { output: None }
        } else {
            Self {
                output: Some(output),
            }
        }
    }

    /// Returns error code to return for this error.
    pub const fn error_code(&self) -> i32 {
        EthRpcErrorCode::ExecutionError.code()
    }
}

impl fmt::Display for RevertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("execution reverted")?;
        if let Some(reason) = self
            .output
            .as_ref()
            .and_then(|out| RevertReason::decode(out))
        {
            let error = reason.to_string();
            let mut error = error.as_str();
            if matches!(
                reason,
                RevertReason::ContractError(ContractError::Revert(_))
            ) {
                // we strip redundant `revert: ` prefix from the revert reason
                error = error.trim_start_matches("revert: ");
            }
            write!(f, ": {error}")?;
        }
        Ok(())
    }
}
