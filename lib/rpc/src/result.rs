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

impl_to_rpc_result!(EthSendRawTransactionError);
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

/// Represents a reverted transaction and its output data.
///
/// Displays "execution reverted(: reason)?" if the reason is a string.
#[derive(Debug, Clone, thiserror::Error)]
pub struct RevertError {
    /// The transaction output data
    ///
    /// Note: this is `None` if output was empty
    output: Option<Bytes>,
    /// Optional pubdata exhaustion context for diagnostics
    pubdata_context: Option<PubdataExhaustionContext>,
}

/// Diagnostic context for pubdata-related reverts.
#[derive(Debug, Clone)]
pub(crate) struct PubdataExhaustionContext {
    pub(crate) pubdata_used: u64,
    pub(crate) native_used: u64,
    pub(crate) gas_limit: u64,
}

impl RevertError {
    /// Wraps the output bytes
    ///
    /// Note: this is intended to wrap a VM output
    pub fn new(output: Bytes) -> Self {
        if output.is_empty() {
            Self {
                output: None,
                pubdata_context: None,
            }
        } else {
            Self {
                output: Some(output),
                pubdata_context: None,
            }
        }
    }

    /// Creates a revert error for pubdata exhaustion (gas limit too low to cover pubdata costs).
    pub fn for_pubdata_exhaustion(pubdata_used: u64, native_used: u64, gas_limit: u64) -> Self {
        Self {
            output: None,
            pubdata_context: Some(PubdataExhaustionContext {
                pubdata_used,
                native_used,
                gas_limit,
            }),
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

        if let Some(ctx) = &self.pubdata_context {
            return write!(
                f,
                ": insufficient gas to cover pubdata cost \
                 (pubdata_used: {} bytes, native_used: {}, gas_limit: {})",
                ctx.pubdata_used, ctx.native_used, ctx.gas_limit
            );
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revert_error_display_empty() {
        let err = RevertError::new(Bytes::new());
        assert_eq!(err.to_string(), "execution reverted");
    }

    #[test]
    fn revert_error_display_pubdata_exhaustion() {
        let err = RevertError::for_pubdata_exhaustion(1000, 500, 21000);
        assert_eq!(
            err.to_string(),
            "execution reverted: insufficient gas to cover pubdata cost \
             (pubdata_used: 1000 bytes, native_used: 500, gas_limit: 21000)"
        );
    }

    #[test]
    fn revert_error_display_pubdata_exhaustion_zero_values() {
        let err = RevertError::for_pubdata_exhaustion(0, 0, 0);
        assert_eq!(
            err.to_string(),
            "execution reverted: insufficient gas to cover pubdata cost \
             (pubdata_used: 0 bytes, native_used: 0, gas_limit: 0)"
        );
    }

    #[test]
    fn revert_error_pubdata_has_no_output_data() {
        let err = RevertError::for_pubdata_exhaustion(100, 200, 300);
        assert!(err.output.is_none());
    }

    #[test]
    fn revert_error_error_code() {
        let err = RevertError::for_pubdata_exhaustion(100, 200, 300);
        assert_eq!(err.error_code(), EthRpcErrorCode::ExecutionError.code());
    }
}
