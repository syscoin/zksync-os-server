use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::{Address, B256, U256};
use zksync_os_evm_errors::EvmError;
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::tracing::{
    AnyTracer, AnyTxValidator, CallModifier, CallResult, EvmFrameInterface, EvmRequest,
    EvmResources, EvmTracer, TxValidationResult, TxValidator,
};

/// Configuration for the deployment filter.
#[derive(Clone, Debug, Default)]
pub enum Config {
    /// Anyone can deploy contracts (default).
    #[default]
    Unrestricted,
    /// Only the listed addresses can deploy contracts.
    AllowList(HashSet<Address>),
}

/// Tracer that detects unauthorized contract deployments.
///
/// On `on_new_execution_frame`, checks if the frame is a Constructor and the caller
/// is not in the allow-list. If so, sets a shared flag that the validator checks at `finish_tx`.
pub struct Tracer {
    unauthorized_deployment: Arc<AtomicBool>,
    config: Config,
}

impl Tracer {
    pub fn new(unauthorized_deployment: Arc<AtomicBool>, config: Config) -> Self {
        Self {
            unauthorized_deployment,
            config,
        }
    }
}

impl AnyTracer for Tracer {
    fn as_evm(&mut self) -> Option<&mut impl EvmTracer> {
        Some(self)
    }
}

impl EvmTracer for Tracer {
    fn on_new_execution_frame(&mut self, request: impl EvmRequest) {
        if let Config::AllowList(allowed) = &self.config {
            let modifier = request.modifier();
            let caller = request.caller();
            if modifier == CallModifier::Constructor && !allowed.contains(&caller) {
                self.unauthorized_deployment.store(true, Ordering::Relaxed);
            }
        }
    }

    fn after_execution_frame_completed(&mut self, _result: Option<(EvmResources, CallResult)>) {}
    fn on_storage_read(&mut self, _: bool, _: Address, _: B256, _: B256) {}
    fn on_storage_write(&mut self, _: bool, _: Address, _: B256, _: B256) {}
    fn on_bytecode_change(&mut self, _: Address, _: Option<&[u8]>, _: B256, _: u32) {}
    fn on_event(&mut self, _: Address, _: Vec<B256>, _: &[u8]) {}

    fn begin_tx(&mut self, _calldata: &[u8]) {
        // Reset the flag at the start of each transaction.
        self.unauthorized_deployment.store(false, Ordering::Relaxed);
    }

    fn finish_tx(&mut self) {}
    fn before_evm_interpreter_execution_step(&mut self, _: u8, _: impl EvmFrameInterface) {}
    fn after_evm_interpreter_execution_step(&mut self, _: u8, _: impl EvmFrameInterface) {}
    fn on_opcode_error(&mut self, _: &EvmError, _: impl EvmFrameInterface) {}
    fn on_call_error(&mut self, _: &EvmError) {}
    fn on_selfdestruct(&mut self, _: Address, _: U256, _: impl EvmFrameInterface) {}
    fn on_create_request(&mut self, _: bool) {}
}

/// Validator that rejects transactions containing unauthorized deployments.
///
/// Checks the shared flag set by `Tracer` at `finish_tx`.
pub struct Validator {
    unauthorized_deployment: Arc<AtomicBool>,
}

impl Validator {
    pub fn new(unauthorized_deployment: Arc<AtomicBool>) -> Self {
        Self {
            unauthorized_deployment,
        }
    }
}

impl AnyTxValidator for Validator {
    fn as_evm(&mut self) -> Option<&mut impl TxValidator> {
        Some(self)
    }
}

impl TxValidator for Validator {
    fn begin_tx(&mut self, _calldata: &[u8]) -> TxValidationResult {
        Ok(())
    }

    fn finish_tx(&mut self) -> TxValidationResult {
        if self.unauthorized_deployment.load(Ordering::Relaxed) {
            return Err(InvalidTransaction::FilteredByValidator);
        }
        Ok(())
    }
}
