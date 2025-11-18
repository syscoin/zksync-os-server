use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use zksync_os_l1_sender::commands::L1SenderCommand;
use zksync_os_l1_sender::commands::prove::ProofCommand;
use zksync_os_observability::{ComponentStateReporter, GenericComponentState};
use zksync_os_pipeline::{PeekableReceiver, PipelineComponent};

/// Receives L1SenderCommands with ProofCommand - potentially out of order.
/// Fixes the order and sends downstream.
///
/// This is a lightweight version without persistence logic.
pub struct GaplessCommitterPassthrough {
    pub next_expected_batch_number: u64,
}

impl GaplessCommitterPassthrough {
    pub fn new(next_expected_batch_number: u64) -> Self {
        Self {
            next_expected_batch_number,
        }
    }
}

#[async_trait]
impl PipelineComponent for GaplessCommitterPassthrough {
    type Input = L1SenderCommand<ProofCommand>;
    type Output = L1SenderCommand<ProofCommand>;

    const NAME: &'static str = "gapless_committer_passthrough";
    const OUTPUT_BUFFER_SIZE: usize = 5;

    async fn run(
        self,
        mut input: PeekableReceiver<Self::Input>,
        output: mpsc::Sender<Self::Output>,
    ) -> anyhow::Result<()> {
        let latency_tracker = ComponentStateReporter::global().handle_for(
            "gapless_committer_passthrough",
            GenericComponentState::WaitingRecv,
        );

        let mut buffer: BTreeMap<u64, L1SenderCommand<ProofCommand>> = BTreeMap::new();
        let mut next_expected_batch_number = self.next_expected_batch_number;

        loop {
            latency_tracker.enter_state(GenericComponentState::WaitingRecv);
            match input.recv().await {
                Some(command) => {
                    latency_tracker.enter_state(GenericComponentState::Processing);

                    let first_batch_number = match &command {
                        L1SenderCommand::SendToL1(cmd) => {
                            cmd.as_ref().first().unwrap().batch_number()
                        }
                        L1SenderCommand::Passthrough(batch) => batch.batch_number(),
                    };

                    buffer.insert(first_batch_number, command);

                    // Flush ready commands
                    let mut ready: Vec<L1SenderCommand<ProofCommand>> = Vec::new();
                    while let Some(next_command) = buffer.remove(&next_expected_batch_number) {
                        // Calculate how many batches this command spans
                        let batch_count = match &next_command {
                            L1SenderCommand::SendToL1(cmd) => cmd.as_ref().len() as u64,
                            L1SenderCommand::Passthrough(_) => 1,
                        };
                        ready.push(next_command);
                        next_expected_batch_number += batch_count;
                    }

                    if !ready.is_empty() {
                        let first_batch = next_expected_batch_number
                            - ready
                                .iter()
                                .map(|cmd| match cmd {
                                    L1SenderCommand::SendToL1(c) => c.as_ref().len() as u64,
                                    L1SenderCommand::Passthrough(_) => 1,
                                })
                                .sum::<u64>();
                        tracing::info!(
                            buffer_size = buffer.len(),
                            "Flushing {} commands (batches {}-{})",
                            ready.len(),
                            first_batch,
                            next_expected_batch_number - 1
                        );
                        for command in ready {
                            latency_tracker.enter_state(GenericComponentState::WaitingSend);
                            output.send(command).await?;
                            latency_tracker.enter_state(GenericComponentState::Processing);
                        }
                    }
                }
                None => {
                    anyhow::bail!("GaplessCommitterPassthrough input stream ended unexpectedly");
                }
            }
        }
    }
}
