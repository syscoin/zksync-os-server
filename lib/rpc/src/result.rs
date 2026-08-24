// The code in this file was copied from reth with some minor changes. Source:
// https://github.com/paradigmxyz/reth/blob/fcf58cb5acc2825e7c046f6741e90a8c5dab7847/crates/rpc/rpc-server-types/src/result.rs
#![allow(dead_code)]

//! Additional helpers for converting errors.

use crate::debug_impl::DebugError;
use crate::eth_call_handler::EthCallError;
use crate::eth_filter::EthFilterError;
use crate::eth_impl::EthError;
use crate::rpc_storage::RpcStorageError;
use crate::tx_forwarder::TxForwardError;
use crate::tx_handler::{EthSendRawTransactionError, EthSendRawTransactionSyncError};
use crate::unstable_impl::UnstableError;
use crate::zks_impl::ZksError;
use alloy::primitives::Bytes;
use alloy::rpc::types::error::EthRpcErrorCode;
use alloy::sol_types::{ContractError, RevertReason};
use alloy::transports::RpcError;
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

impl_to_rpc_result!(UnstableError);

// SYSCOIN: An explicitly unsupported proof target is a stable client/topology error, not an
// internal provider failure. Every other ZKS failure is public only as a fixed generic error;
// provider transport errors may embed credential-bearing request URLs in their Display output.
const ZKS_INTERNAL_ERROR_MESSAGE: &str = "Internal error";

// SYSCOIN: Preserve an operator-useful failure category without formatting nested provider,
// repository, or state errors. In particular, reqwest documents that its Display output may
// contain the full URL, including userinfo or query-string API keys.
fn zks_internal_error_log_summary(err: &ZksError) -> &'static str {
    match err {
        ZksError::BlockNotAvailable(_) => "historical block unavailable",
        ZksError::TxNotAvailable(_) => "historical transaction unavailable",
        ZksError::TransactionNotInBatch { .. } => "transaction/batch index inconsistency",
        ZksError::IndexOutOfBounds(..) => "L2-to-L1 log index out of bounds",
        ZksError::UnsupportedProofTargetForDirectL1 { .. } => "unsupported direct-L1 proof target",
        ZksError::CommitmentTree(_) => "interop commitment-tree proof failed",
        ZksError::Batch(_) => "batch or settlement-proof construction failed",
        ZksError::Repository(_) => "repository read failed",
        ZksError::GenesisSource(_) => "genesis input read failed",
        ZksError::State(_) => "historical state read failed",
    }
}

impl<Ok> ToRpcResult<Ok, ZksError> for Result<Ok, ZksError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            err @ ZksError::UnsupportedProofTargetForDirectL1 { .. } => {
                invalid_params_rpc_err(err.to_string())
            }
            err => {
                // SYSCOIN: Never attach the raw error to a public response or log field; both
                // paths could otherwise expose a provider URL secret during a transport failure.
                tracing::error!(
                    zks_error_summary = zks_internal_error_log_summary(&err),
                    "ZKS RPC request failed"
                );
                internal_rpc_err(ZKS_INTERNAL_ERROR_MESSAGE)
            }
        })
    }
}

impl<Ok> ToRpcResult<Ok, EthError> for Result<Ok, EthError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            EthError::BlockNotFound(_)
            | EthError::NonceMaxValue
            | EthError::InvalidRewardPercentiles
            // SYSCOIN:
            | EthError::PageSizeTooLarge { .. } => invalid_params_rpc_err(err.to_string()),
            EthError::RpcStorage(RpcStorageError::BlockNotFound(_)) => {
                invalid_params_rpc_err(err.to_string())
            }
            EthError::ReceiptMetadataUnavailable(_)
            | EthError::RpcStorage(_)
            | EthError::Repository(_)
            | EthError::State(_) => internal_rpc_err(err.to_string()),
        })
    }
}

impl<Ok> ToRpcResult<Ok, DebugError> for Result<Ok, DebugError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            DebugError::UnsupportedDefaultTracer
            | DebugError::UnsupportedTracer(_)
            | DebugError::UnsupportedTxIndex
            | DebugError::InvalidTracerConfig
            | DebugError::TransactionNotFound
            | DebugError::BlockNotFound => invalid_params_rpc_err(err.to_string()),
            DebugError::InternalError | DebugError::Repository(_) | DebugError::State(_) => {
                internal_rpc_err(err.to_string())
            }
            DebugError::Call(e) => Result::<(), _>::Err(e).to_rpc_result().unwrap_err(),
        })
    }
}

impl<Ok> ToRpcResult<Ok, EthSendRawTransactionError> for Result<Ok, EthSendRawTransactionError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            EthSendRawTransactionError::FailedToDecodeSignedTransaction
            | EthSendRawTransactionError::InvalidTransactionSignature
            | EthSendRawTransactionError::BlacklistedSigner
            | EthSendRawTransactionError::BlacklistedTransaction
            | EthSendRawTransactionError::EdgeDaAdmissionCheckFailed(_)
            | EthSendRawTransactionError::PoolError(_) => invalid_params_rpc_err(err.to_string()),
            EthSendRawTransactionError::NotAcceptingTransactions(_) => {
                internal_rpc_err(err.to_string())
            }
            EthSendRawTransactionError::GasRateLimited { ref retry_after } => {
                rate_limited_rpc_err(err.to_string(), retry_after.as_millis() as u64)
            }
            EthSendRawTransactionError::ForwardError(ref forward_err) => {
                forward_error_to_rpc_err(forward_err, &err)
            }
            EthSendRawTransactionError::PolicyDenied => rpc_err(
                EthRpcErrorCode::TransactionRejected.code(),
                err.to_string(),
                None,
            ),
            EthSendRawTransactionError::JudgeSimFailed(_) => internal_rpc_err(err.to_string()),
        })
    }
}

impl<Ok> ToRpcResult<Ok, EthFilterError> for Result<Ok, EthFilterError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            EthFilterError::BlockNotFound(_)
            | EthFilterError::FilterNotFound(_)
            | EthFilterError::QueryExceedsMaxBlocks(_)
            | EthFilterError::QueryExceedsMaxResults { .. } => {
                invalid_params_rpc_err(err.to_string())
            }
            EthFilterError::RepositoryError(_) => internal_rpc_err(err.to_string()),
        })
    }
}

impl<Ok> ToRpcResult<Ok, EthCallError> for Result<Ok, EthCallError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            EthCallError::Revert(revert) => rpc_err(
                EthRpcErrorCode::ExecutionError.code(),
                revert.to_string(),
                revert.output.as_ref().map(|out| out.as_ref()),
            ),
            EthCallError::SimulateInvalidParams(_)
            | EthCallError::SimulateInvalidBlockOverride(_) => {
                invalid_params_rpc_err(err.to_string())
            }
            // Error codes -380xx follow the reth implementation of the eth_simulateV1 spec.
            EthCallError::SimulateBlockNumberInvalid { .. } => {
                rpc_error_with_code(-38020, err.to_string())
            }
            EthCallError::SimulateBlockTimestampInvalid { .. } => {
                rpc_error_with_code(-38021, err.to_string())
            }
            EthCallError::SimulateBlockGasLimitExceeded => {
                rpc_error_with_code(-38015, err.to_string())
            }
            EthCallError::SimulateMovePrecompileNotSupported => {
                invalid_params_rpc_err(err.to_string())
            }
            EthCallError::PolicyDenied => rpc_err(
                EthRpcErrorCode::TransactionRejected.code(),
                err.to_string(),
                None,
            ),
            EthCallError::CallFees(_) => invalid_params_rpc_err(err.to_string()),
            EthCallError::Storage(RpcStorageError::BlockNotFound(_)) => {
                invalid_params_rpc_err(err.to_string())
            }
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
            EthSendRawTransactionSyncError::Regular(inner) => {
                Result::<(), _>::Err(inner).to_rpc_result().unwrap_err()
            }
            err @ EthSendRawTransactionSyncError::Timeout(_) => {
                // Code 4 is used as per EIP-7966 (see https://eips.ethereum.org/EIPS/eip-7966)
                rpc_error_with_code(4, err.to_string())
            }
            err @ EthSendRawTransactionSyncError::RejectedDuringExecution(_) => rpc_err(
                EthRpcErrorCode::TransactionRejected.code(),
                err.to_string(),
                None,
            ),
        })
    }
}

/// Converts tx forwarding errors into a jsonrpsee error object.
/// Preserves the original JSON-RPC error code when the remote node returned one.
/// Local routing failures and transport failures fall back to internal error (-32603).
fn forward_error_to_rpc_err(
    forward_err: &TxForwardError,
    display: &impl fmt::Display,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    match forward_err {
        TxForwardError::Rpc(RpcError::ErrorResp(payload)) => {
            // Preserve structured data (e.g. the gas rate limiter's `retryAfterMs`)
            // so hints survive EN → main forwarding.
            jsonrpsee::types::error::ErrorObject::owned(
                payload.code as i32,
                display.to_string(),
                payload.data.clone(),
            )
        }
        TxForwardError::Rpc(_) | TxForwardError::NoKnownLeader | TxForwardError::NoProvider(_) => {
            internal_rpc_err(display.to_string())
        }
    }
}

/// Constructs an unimplemented JSON-RPC error.
pub fn unimplemented_rpc_err() -> jsonrpsee::types::error::ErrorObject<'static> {
    internal_rpc_err("unimplemented")
}

/// EIP-1474 "Limit exceeded" — the de facto Ethereum rate-limit code (Infura, Alchemy, etc.);
/// clients treat it as retriable, unlike -32003 (transaction rejected).
pub const RATE_LIMIT_ERROR_CODE: i32 = -32005;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryData {
    retry_after_ms: u64,
}

/// Constructs a rate-limit JSON-RPC error with a structured `retryAfterMs` hint.
pub fn rate_limited_rpc_err(
    msg: impl Into<String>,
    retry_after_ms: u64,
) -> jsonrpsee::types::error::ErrorObject<'static> {
    let data = jsonrpsee::core::to_json_raw_value(&RetryData { retry_after_ms })
        .expect("infallible serialization");
    jsonrpsee::types::error::ErrorObject::owned(RATE_LIMIT_ERROR_CODE, msg.into(), Some(data))
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

#[cfg(test)]
mod tests {
    use super::*;
    use zksync_os_rpc_api::types::LogProofTarget;

    // SYSCOIN: Freeze the public distinction between an unsupported topology/target request and
    // an actual internal proof-provider failure so clients do not retry the former indefinitely.
    #[test]
    fn direct_l1_message_root_is_an_invalid_params_error() {
        let unsupported: Result<(), ZksError> = Err(ZksError::UnsupportedProofTargetForDirectL1 {
            batch_number: 7,
            proof_target: LogProofTarget::MessageRoot,
        });
        let expected_message = unsupported.as_ref().unwrap_err().to_string();
        let unsupported = unsupported.to_rpc_result().unwrap_err();
        assert_eq!(
            unsupported.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert_eq!(unsupported.message(), expected_message);

        let internal: Result<(), ZksError> =
            Err(ZksError::Batch(anyhow::anyhow!("provider failed")));
        assert_eq!(
            internal.to_rpc_result().unwrap_err().code(),
            jsonrpsee::types::error::INTERNAL_ERROR_CODE
        );
    }

    // SYSCOIN: Provider failures may carry credentials in reqwest's URL-bearing Display output.
    // Neither the public JSON-RPC error nor the deliberately coarse operator summary may retain it.
    #[test]
    fn zks_internal_errors_redact_provider_url_secrets_from_response_and_log_summary() {
        let secret_url =
            "https://rpc-user:rpc-password@example.invalid/v3/project?api_key=super-secret";
        let error = ZksError::Batch(anyhow::anyhow!(
            "error sending request for url ({secret_url})"
        ));
        let summary = zks_internal_error_log_summary(&error);
        assert_eq!(summary, "batch or settlement-proof construction failed");
        assert!(!summary.contains(secret_url));
        assert!(!summary.contains("rpc-password"));
        assert!(!summary.contains("super-secret"));

        let public = Result::<(), _>::Err(error).to_rpc_result().unwrap_err();
        assert_eq!(public.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
        assert_eq!(public.message(), ZKS_INTERNAL_ERROR_MESSAGE);
        assert!(!public.to_string().contains(secret_url));
        assert!(!public.to_string().contains("rpc-password"));
        assert!(!public.to_string().contains("super-secret"));
    }
}
