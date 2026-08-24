//! Canonical Syscoin block execution and transaction simulation.

use zk_os_forward_system::run::RunBlockForward;
use zksync_os_interface::error::InvalidTransaction;
use zksync_os_interface::tracing::{AnyTracer, AnyTxValidator};
use zksync_os_interface::traits::{
    EncodedTx, NoFriProofSidecar, PreimageSource, ReadStorage, RunBlock, SimulateTx,
    TxResultCallback, TxSource,
};
use zksync_os_interface::types::TxOutput;
use zksync_os_storage_api::BlockContext;

use zksync_os_types::{BlockOutput, BlockPubdata, ExecutionVersion};

fn into_block_output(output: zk_os_forward_system::run::output::BlockOutput) -> BlockOutput {
    BlockOutput {
        header: output.header,
        tx_results: output.tx_results,
        storage_writes: output.storage_writes,
        account_diffs: output.account_diffs,
        published_preimages: output.published_preimages,
        pubdata: BlockPubdata::new(output.pubdata_used),
        computational_native_used: output.computational_native_used,
    }
}

// SYSCOIN: Treat unsupported execution versions from replay/RPC data as recoverable errors.
fn execution_version_from_context(
    block_context: &BlockContext,
) -> Result<ExecutionVersion, anyhow::Error> {
    block_context.execution_version.try_into().map_err(|_| {
        anyhow::anyhow!(
            "Unsupported ZKsync OS execution version: {}",
            block_context.execution_version
        )
    })
}

pub fn run_block<
    Storage: ReadStorage,
    PreimgSrc: PreimageSource,
    TrSrc: TxSource,
    TrCallback: TxResultCallback,
    Tracer: AnyTracer,
    Validator: AnyTxValidator,
>(
    block_context: BlockContext,
    storage: Storage,
    preimage_source: PreimgSrc,
    tx_source: TrSrc,
    tx_result_callback: TrCallback,
    tracer: &mut Tracer,
    validator: &mut Validator,
) -> Result<BlockOutput, anyhow::Error> {
    execution_version_from_context(&block_context)?;
    let chain_config = zksync_os_native_pig::chain_config(block_context.chain_id)?;
    let object = RunBlockForward {
        fri_verifier_artifacts: None,
    };
    let output = object
        .run_block(
            chain_config,
            block_context,
            storage,
            preimage_source,
            tx_source,
            NoFriProofSidecar,
            tx_result_callback,
            tracer,
            validator,
        )
        .map_err(|err| anyhow::anyhow!(err))
        .map(into_block_output)?;
    Ok(output)
}

pub fn simulate_tx<
    Storage: ReadStorage,
    PreimgSrc: PreimageSource,
    Tracer: AnyTracer,
    Validator: AnyTxValidator,
>(
    transaction: EncodedTx,
    block_context: BlockContext,
    storage: Storage,
    preimage_source: PreimgSrc,
    tracer: &mut Tracer,
    validator: &mut Validator,
) -> Result<Result<TxOutput, InvalidTransaction>, anyhow::Error> {
    execution_version_from_context(&block_context)?;
    let chain_config = zksync_os_native_pig::chain_config(block_context.chain_id)?;
    let object = RunBlockForward {
        fri_verifier_artifacts: None,
    };
    object
        .simulate_tx(
            chain_config,
            transaction,
            block_context,
            storage,
            preimage_source,
            tracer,
            validator,
        )
        .map_err(|err| anyhow::anyhow!(err))
}

#[cfg(test)]
mod tests {
    use super::execution_version_from_context;
    use zksync_os_storage_api::BlockContext;
    use zksync_os_types::ExecutionVersion;

    #[test]
    fn unsupported_execution_version_returns_error() {
        let block_context = BlockContext {
            execution_version: u32::MAX,
            ..Default::default()
        };

        let error = execution_version_from_context(&block_context).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Unsupported ZKsync OS execution version: 4294967295"
        );
    }

    #[test]
    fn supported_execution_version_is_resolved() {
        let block_context = BlockContext {
            execution_version: ExecutionVersion::V7 as u32,
            ..Default::default()
        };

        assert_eq!(
            execution_version_from_context(&block_context).unwrap(),
            ExecutionVersion::V7
        );
    }
}
