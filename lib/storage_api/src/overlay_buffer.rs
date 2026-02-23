use std::collections::{BTreeMap, HashMap};

use alloy::primitives::B256;
use anyhow::bail;
use zksync_os_interface::types::StorageWrite;

use crate::{OverriddenStateView, ReadStateHistory, ViewState};

#[derive(Debug, Clone)]
pub struct BlockOverlay {
    pub storage_writes: Vec<StorageWrite>,
    pub preimages: Vec<(B256, Vec<u8>)>,
}

#[derive(Debug, Default)]
pub struct OverlayBuffer {
    overlays: BTreeMap<u64, BlockOverlay>,
}

impl OverlayBuffer {
    /// Drops records for blocks that already persist in `base`;
    /// Builds a state view to accommodate for the execution of `block_number_to_execute` block.
    /// Note: to execute block N, we need state (+ overlays) to reach N - 1.
    pub fn sync_with_base_and_build_view_for_block<'a, S>(
        &'a mut self,
        base: &'a S,
        block_number_to_execute: u64,
    ) -> anyhow::Result<OverriddenStateView<impl ViewState + 'a>>
    where
        S: ReadStateHistory + 'a,
    {
        let base_latest = *base.block_range_available().end();
        tracing::debug!(
            "before Synced overlay buffer with base (base_latest={base_latest}, overlays_len={}, overlays_range={:?}..={:?}). \
            Preparing storage view to execute block {block_number_to_execute}.",
            self.overlays.len(),
            self.overlays.keys().next().copied(),
            self.overlays.keys().next_back().copied(),
        );
        self.purge_already_persisted_blocks(base_latest)?;
        let first_overlay = self.overlays.keys().next().copied();
        let last_overlay = self.overlays.keys().next_back().copied();
        tracing::debug!(
            "Synced overlay buffer with base (base_latest={base_latest}, overlays_len={}, overlays_range={first_overlay:?}..={last_overlay:?}). \
            Preparing storage view to execute block {block_number_to_execute}.",
            self.overlays.len(),
        );
        if base_latest >= block_number_to_execute - 1 {
            let base_view = base
                .state_view_at(block_number_to_execute - 1)
                .map_err(|e| anyhow::anyhow!(e))?;
            return Ok(OverriddenStateView::new(
                base_view,
                HashMap::new(),
                HashMap::new(),
            ));
        }

        let base_view = base
            .state_view_at(base_latest)
            .map_err(|e| anyhow::anyhow!(e))?;
        if first_overlay != Some(base_latest + 1)
            || last_overlay != Some(block_number_to_execute - 1)
        {
            // This assert is defensive - we could build overlay maps from a subset of overlay records,
            // but this behaviour is unexpected as we execute blocks in strict accession.
            bail!(
                "Unexpected state of `overlay_buffer` when preparing state view for block {} from base_latest {}; overlays_range={:?}..={:?}",
                block_number_to_execute,
                base_latest,
                first_overlay,
                last_overlay
            );
        }
        let (overrides, preimages) = self.build_maps();
        Ok(OverriddenStateView::new(base_view, overrides, preimages))
    }

    pub fn add_block(
        &mut self,
        block_number: u64,
        storage_writes: Vec<StorageWrite>,
        preimages: Vec<(B256, Vec<u8>)>,
    ) -> anyhow::Result<()> {
        if let Some(&last) = self.overlays.keys().next_back()
            && block_number != last + 1
        {
            bail!(
                "Overlay head must be contiguous: got {}, expected {}",
                block_number,
                last + 1
            );
        }
        self.overlays.insert(
            block_number,
            BlockOverlay {
                storage_writes,
                preimages,
            },
        );
        Ok(())
    }

    fn purge_already_persisted_blocks(&mut self, base_latest: u64) -> anyhow::Result<()> {
        if let Some(&last) = self.overlays.keys().next() && base_latest + 1 < last {
            bail!(
                "Cannot clean tail: base_latest {} is behind overlay tail {}",
                base_latest,
                last
            );
        }
        self.overlays.retain(|block, _| *block > base_latest);
        Ok(())
    }

    fn build_maps(&self) -> (HashMap<B256, B256>, HashMap<B256, Vec<u8>>) {
        let mut overrides: HashMap<B256, B256> = HashMap::new();
        let mut preimages: HashMap<B256, Vec<u8>> = HashMap::new();

        for (_, overlay) in self.overlays.iter() {
            for write in &overlay.storage_writes {
                overrides.insert(write.key, write.value);
            }
            for (hash, bytes) in &overlay.preimages {
                preimages.insert(*hash, bytes.clone());
            }
        }

        (overrides, preimages)
    }
}
