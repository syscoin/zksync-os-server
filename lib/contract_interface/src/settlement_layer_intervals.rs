use crate::{Bridgehub, IChainAssetHandler, ZkChain, is_method_missing};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use anyhow::Context;
use std::fmt;
use std::sync::Arc;
use zksync_os_provider::NodeProvider;

/// Settlement layer that a chain was committing to during a given batch range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntervalSettlementLayer {
    /// Settling on L1 directly.
    L1,
    /// Settling on a Gateway, identified by its chain ID.
    Gateway(u64),
}

impl fmt::Display for IntervalSettlementLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntervalSettlementLayer::L1 => f.write_str("L1"),
            IntervalSettlementLayer::Gateway(chain_id) => write!(f, "Gateway({chain_id})"),
        }
    }
}

/// Inclusive batch-number range during which the chain committed to a single settlement layer,
/// paired with the diamond proxy for that settlement layer.
///
/// `last_batch` is `None` for the currently-active (open-ended) interval.
#[derive(Clone)]
pub struct SettlementLayerInterval {
    pub settlement_layer: IntervalSettlementLayer,
    pub first_batch: u64,
    pub last_batch: Option<u64>,
    /// Diamond proxy on `settlement_layer`.
    pub proxy: ZkChain<NodeProvider>,
}

impl fmt::Debug for SettlementLayerInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SYSCOIN: do not print the provider-backed proxy Debug output; RPC URLs may contain
        // credentials. The proxy address is enough to diagnose interval routing.
        f.debug_struct("SettlementLayerInterval")
            .field("settlement_layer", &self.settlement_layer)
            .field("first_batch", &self.first_batch)
            .field("last_batch", &self.last_batch)
            .field("proxy_address", self.proxy.address())
            .finish()
    }
}

impl fmt::Display for SettlementLayerInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.last_batch {
            Some(last) => write!(
                f,
                "{} batches {}..={}",
                self.settlement_layer, self.first_batch, last
            ),
            None => write!(
                f,
                "{} batches {}..",
                self.settlement_layer, self.first_batch
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RawSettlementLayerInterval {
    settlement_layer: IntervalSettlementLayer,
    first_batch: u64,
    last_batch: Option<u64>,
}

// SYSCOIN: The pinned V32 contract permits exactly two migration operations: one L1→Gateway and
// one Gateway→L1 return. It stores that round-trip in slot 1; slot 2 remains unpopulated.
const MAX_ALLOWED_MIGRATION_OPERATIONS: u64 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MigrationSlot {
    settlement_layer_chain_id: u64,
    migrate_to_gateway_batch: u64,
    migrate_from_gateway_batch: u64,
    settlement_layer_batch_lower_bound: u64,
    settlement_layer_batch_upper_bound: u64,
    is_active: bool,
}

fn validate_migration_count(total_migrations: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        total_migrations <= MAX_ALLOWED_MIGRATION_OPERATIONS,
        "migrationNumber {total_migrations} exceeds pinned V32 maximum {MAX_ALLOWED_MIGRATION_OPERATIONS}"
    );
    Ok(())
}

// SYSCOIN: Convert the pinned one-round-trip storage layout into non-overlapping, ordered batch
// intervals with checked cursor arithmetic. Faulty RPC values fail closed before routing proofs.
fn build_raw_intervals(
    total_migrations: u64,
    slots: Vec<Option<MigrationSlot>>,
) -> anyhow::Result<Vec<RawSettlementLayerInterval>> {
    validate_migration_count(total_migrations)?;
    anyhow::ensure!(
        slots.len() == total_migrations as usize,
        "migration slot count does not match migrationNumber"
    );
    if total_migrations == 0 {
        return Ok(vec![RawSettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::L1,
            first_batch: 1,
            last_batch: None,
        }]);
    }

    let first = slots[0]
        .as_ref()
        .context("pinned V32 migration slot 1 is unpopulated")?;
    anyhow::ensure!(
        slots.iter().skip(1).all(Option::is_none),
        "pinned V32 stores the sole round-trip in migration slot 1; later slots must be empty"
    );
    anyhow::ensure!(
        (total_migrations == 1 && first.is_active) || (total_migrations == 2 && !first.is_active),
        "migration slot activity contradicts migrationNumber {total_migrations}"
    );
    if first.is_active {
        anyhow::ensure!(
            first.migrate_from_gateway_batch == 0 && first.settlement_layer_batch_upper_bound == 0,
            "active migration slot contains return-migration bounds"
        );
    } else {
        anyhow::ensure!(
            first.settlement_layer_batch_upper_bound >= first.settlement_layer_batch_lower_bound,
            "completed migration has inverted settlement-layer batch bounds"
        );
    }

    let gateway_first = first
        .migrate_to_gateway_batch
        .checked_add(1)
        .context("migrateToGWBatchNumber overflows the next batch cursor")?;
    let mut intervals = Vec::with_capacity(3);
    if first.migrate_to_gateway_batch >= 1 {
        intervals.push(RawSettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::L1,
            first_batch: 1,
            last_batch: Some(first.migrate_to_gateway_batch),
        });
    }

    if first.is_active {
        intervals.push(RawSettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::Gateway(first.settlement_layer_chain_id),
            first_batch: gateway_first,
            last_batch: None,
        });
        return Ok(intervals);
    }

    let l1_return_first = first
        .migrate_from_gateway_batch
        .checked_add(1)
        .context("migrateFromGWBatchNumber overflows the next batch cursor")?;
    anyhow::ensure!(
        l1_return_first >= gateway_first,
        "completed Gateway interval ends before it begins: {} < {}",
        first.migrate_from_gateway_batch,
        gateway_first
    );
    if first.migrate_from_gateway_batch >= gateway_first {
        intervals.push(RawSettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::Gateway(first.settlement_layer_chain_id),
            first_batch: gateway_first,
            last_batch: Some(first.migrate_from_gateway_batch),
        });
    }
    intervals.push(RawSettlementLayerInterval {
        settlement_layer: IntervalSettlementLayer::L1,
        first_batch: l1_return_first,
        last_batch: None,
    });
    Ok(intervals)
}

/// Settlement layer intervals for a chain. Each entry carries the diamond proxy needed to route
/// batch lookups to the correct RPC.
///
/// The intervals cover all batches from `1` upwards in ascending order, with the last entry being
/// open-ended (`last_batch = None`).
#[derive(Debug, Clone)]
pub struct SettlementLayerIntervals {
    intervals: Arc<Vec<SettlementLayerInterval>>,
}

impl SettlementLayerIntervals {
    /// Constructs the canonical open-ended direct-L1 layout when discovery is unnecessary
    /// (for example, for components assembled from already-known state in tests).
    pub fn direct_l1(proxy: ZkChain<NodeProvider>) -> Self {
        Self {
            intervals: Arc::new(vec![SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::L1,
                first_batch: 1,
                last_batch: None,
                proxy,
            }]),
        }
    }

    /// Discovers the intervals on-chain from `IL1ChainAssetHandler.migrationInterval` and attaches
    /// the matching diamond proxy to each. Fails if a historical Gateway interval references a
    /// chain that the configured `gateway_provider` cannot serve.
    pub async fn discover(
        chain_asset_handler: Address,
        diamond_proxy_l1: ZkChain<NodeProvider>,
        gateway_provider: Option<NodeProvider>,
        l2_chain_id: u64,
    ) -> anyhow::Result<Self> {
        let raw_intervals = find_settlement_layer_intervals(
            chain_asset_handler,
            diamond_proxy_l1.provider().clone(),
            l2_chain_id,
        )
        .await
        .context("failed to discover settlement layer intervals")?;
        // Resolve historical Gateway diamond proxy if the chain has any Gateway interval AND
        // gateway_provider is configured.
        let has_historical_gateway = raw_intervals
            .iter()
            .any(|i| matches!(i.settlement_layer, IntervalSettlementLayer::Gateway(_)));
        let diamond_proxy_gw =
            if has_historical_gateway && let Some(gateway_provider) = &gateway_provider {
                let gw_chain_id = gateway_provider.get_chain_id().await?;
                let bridgehub_gw = Bridgehub::new(
                    crate::l1_discovery::L2_BRIDGEHUB_ADDRESS,
                    gateway_provider.clone(),
                    l2_chain_id,
                );
                let historical_diamond_proxy_gw = bridgehub_gw
                    .zk_chain()
                    .await
                    .context("failed to resolve historical Gateway diamond proxy")?;
                Some((gw_chain_id, historical_diamond_proxy_gw))
            } else {
                None
            };

        let mut intervals = Vec::with_capacity(raw_intervals.len());
        for raw in raw_intervals {
            let proxy = match raw.settlement_layer {
                IntervalSettlementLayer::L1 => diamond_proxy_l1.clone(),
                IntervalSettlementLayer::Gateway(chain_id) => match &diamond_proxy_gw {
                    Some((gw_chain_id, gw)) if *gw_chain_id == chain_id => gw.clone(),
                    Some((gw_chain_id, _)) => anyhow::bail!(
                        "interval {}..{} was committed on Gateway with chain ID {chain_id} but \
                         the chain's current Gateway is {gw_chain_id}; no provider is available \
                         for the historical Gateway",
                        raw.first_batch,
                        raw.last_batch
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                    ),
                    None => anyhow::bail!(
                        "interval {}..{} was committed on Gateway with chain ID {chain_id} but \
                         the chain currently settles on L1; no Gateway provider is configured",
                        raw.first_batch,
                        raw.last_batch
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                    ),
                },
            };
            intervals.push(SettlementLayerInterval {
                settlement_layer: raw.settlement_layer,
                first_batch: raw.first_batch,
                last_batch: raw.last_batch,
                proxy,
            });
        }
        Ok(Self {
            intervals: Arc::new(intervals),
        })
    }

    pub fn intervals(&self) -> &[SettlementLayerInterval] {
        &self.intervals
    }

    /// Settlement layer of the currently-active (open-ended) interval — i.e. where the chain is
    /// currently committing batches.
    pub fn current_settlement_layer(&self) -> &IntervalSettlementLayer {
        &self
            .intervals
            .last()
            .expect("settlement layer intervals are never empty")
            .settlement_layer
    }

    /// `true` when the chain is currently committing batches to a Gateway.
    pub fn settles_on_gateway(&self) -> bool {
        matches!(
            self.current_settlement_layer(),
            IntervalSettlementLayer::Gateway(_)
        )
    }

    /// Returns the settlement layer interval containing `batch_number`.
    pub fn find_interval(&self, batch_number: u64) -> Option<&SettlementLayerInterval> {
        self.intervals.iter().find(|i| {
            batch_number >= i.first_batch && i.last_batch.is_none_or(|last| batch_number <= last)
        })
    }
}

/// Returns all batch-number intervals during which the chain committed to a single settlement
/// layer, in ascending order and covering all batches from `1` upwards.
///
/// The intervals are reconstructed from `IL1ChainAssetHandler.migrationInterval(chainId, i)`
/// for each known migration slot (`i ∈ [1, migrationNumber(chainId)]`):
///
/// - Each populated slot describes one L1 → Gateway → L1 round-trip, giving us the chain's own
///   batch number at which the migration to the Gateway happened (`migrateToGWBatchNumber`) and
///   the one at which it returned (`migrateFromGWBatchNumber`, or `isActive = true` if the
///   chain has not returned yet).
/// - Slot `0` is reserved for the legacy Gateway and is intentionally skipped — legacy-GW chains
///   are not supported here.
/// - `MAX_ALLOWED_NUMBER_OF_MIGRATIONS = 2` on-chain means one L1→Gateway→L1 round-trip (two
///   migration operations). The interval itself is stored in slot `1`; slot `2` is empty.
async fn find_settlement_layer_intervals(
    chain_asset_handler: Address,
    provider: NodeProvider,
    chain_id: u64,
) -> anyhow::Result<Vec<RawSettlementLayerInterval>> {
    let cah = IChainAssetHandler::new(chain_asset_handler, provider);
    let total_migrations: u64 = match cah.migrationNumber(U256::from(chain_id)).call().await {
        Ok(n) => n
            .try_into()
            .map_err(|e| anyhow::anyhow!("migrationNumber overflow: {e}"))?,
        // Pre-V31 `ChainAssetHandler` does not expose `migrationNumber`. In that era Gateway
        // migrations are not possible, so the chain has always committed to L1.
        Err(e) if is_pre_v31_migration_number_error(&e) => {
            tracing::debug!(
                "ChainAssetHandler does not expose migrationNumber; assuming pre-V31 protocol \
                 with no Gateway migrations: {e}"
            );
            return Ok(vec![RawSettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::L1,
                first_batch: 1,
                last_batch: None,
            }]);
        }
        Err(e) => return Err(anyhow::Error::new(e).context("failed to fetch migrationNumber")),
    };
    // SYSCOIN: Bound allocation and RPC fan-out before materializing the migration-slot futures.
    validate_migration_count(total_migrations)?;

    let raw = futures::future::try_join_all((1..=total_migrations).map(|i| {
        let cah = &cah;
        async move {
            let interval = cah
                .migrationInterval(U256::from(chain_id), U256::from(i))
                .call()
                .await
                .with_context(|| format!("failed to fetch migrationInterval({chain_id}, {i})"))?;
            anyhow::Ok(interval)
        }
    }))
    .await?;
    let slots = raw
        .into_iter()
        .enumerate()
        .map(|(index, raw)| {
            let slot_number = index + 1;
            let all_numeric_zero = raw.migrateToGWBatchNumber.is_zero()
                && raw.migrateFromGWBatchNumber.is_zero()
                && raw.settlementLayerBatchLowerBound.is_zero()
                && raw.settlementLayerBatchUpperBound.is_zero()
                && raw.settlementLayerChainId.is_zero();
            if raw.settlementLayerChainId.is_zero() {
                anyhow::ensure!(
                    all_numeric_zero && !raw.isActive,
                    "migration slot {slot_number} is partially populated"
                );
                return Ok(None);
            }
            let convert = |value: U256, field: &str| -> anyhow::Result<u64> {
                value.try_into().map_err(|error| {
                    anyhow::anyhow!("migration slot {slot_number} {field} overflow: {error}")
                })
            };
            Ok(Some(MigrationSlot {
                settlement_layer_chain_id: convert(
                    raw.settlementLayerChainId,
                    "settlementLayerChainId",
                )?,
                migrate_to_gateway_batch: convert(
                    raw.migrateToGWBatchNumber,
                    "migrateToGWBatchNumber",
                )?,
                migrate_from_gateway_batch: convert(
                    raw.migrateFromGWBatchNumber,
                    "migrateFromGWBatchNumber",
                )?,
                settlement_layer_batch_lower_bound: convert(
                    raw.settlementLayerBatchLowerBound,
                    "settlementLayerBatchLowerBound",
                )?,
                settlement_layer_batch_upper_bound: convert(
                    raw.settlementLayerBatchUpperBound,
                    "settlementLayerBatchUpperBound",
                )?,
                is_active: raw.isActive,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    build_raw_intervals(total_migrations, slots)
}

// SYSCOIN: Anvil reports an unknown selector against the pre-V31 ChainAssetHandler as an empty
// EVM revert instead of returning empty call data. Keep this compatibility exception local to the
// read-only migration counter; the generic method-missing check deliberately propagates transport
// errors so privileged upgrade-data calls cannot silently fall back after a real contract revert.
fn is_pre_v31_migration_number_error(err: &alloy::contract::Error) -> bool {
    if is_method_missing(err) {
        return true;
    }
    let alloy::contract::Error::TransportError(err) = err else {
        return false;
    };
    err.as_error_resp().is_some_and(|response| {
        response.code == 3
            && response.message == "execution reverted"
            && response
                .data
                .as_ref()
                .is_some_and(|data| data.get() == "\"0x\"")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(to: u64, from: u64, active: bool) -> MigrationSlot {
        MigrationSlot {
            settlement_layer_chain_id: 506,
            migrate_to_gateway_batch: to,
            migrate_from_gateway_batch: from,
            settlement_layer_batch_lower_bound: 7,
            settlement_layer_batch_upper_bound: if active { 0 } else { 9 },
            is_active: active,
        }
    }

    #[test]
    fn migration_count_is_bounded_before_slot_materialization() {
        let error = validate_migration_count(3).unwrap_err();
        assert!(error.to_string().contains("exceeds pinned V32 maximum"));
    }

    #[test]
    fn active_and_completed_round_trips_are_ordered_and_open_ended() {
        let active = build_raw_intervals(1, vec![Some(slot(10, 0, true))]).unwrap();
        assert_eq!(
            active,
            vec![
                RawSettlementLayerInterval {
                    settlement_layer: IntervalSettlementLayer::L1,
                    first_batch: 1,
                    last_batch: Some(10),
                },
                RawSettlementLayerInterval {
                    settlement_layer: IntervalSettlementLayer::Gateway(506),
                    first_batch: 11,
                    last_batch: None,
                },
            ]
        );

        let completed = build_raw_intervals(2, vec![Some(slot(10, 20, false)), None]).unwrap();
        assert_eq!(
            completed,
            vec![
                RawSettlementLayerInterval {
                    settlement_layer: IntervalSettlementLayer::L1,
                    first_batch: 1,
                    last_batch: Some(10),
                },
                RawSettlementLayerInterval {
                    settlement_layer: IntervalSettlementLayer::Gateway(506),
                    first_batch: 11,
                    last_batch: Some(20),
                },
                RawSettlementLayerInterval {
                    settlement_layer: IntervalSettlementLayer::L1,
                    first_batch: 21,
                    last_batch: None,
                },
            ]
        );
    }

    #[test]
    fn migration_at_batch_zero_has_no_inverted_l1_prefix() {
        assert_eq!(
            build_raw_intervals(1, vec![Some(slot(0, 0, true))]).unwrap(),
            vec![RawSettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::Gateway(506),
                first_batch: 1,
                last_batch: None,
            }]
        );
    }

    #[test]
    fn malformed_migration_slots_fail_closed() {
        let mut overflow_to = slot(u64::MAX, 0, true);
        assert!(
            build_raw_intervals(1, vec![Some(overflow_to.clone())])
                .unwrap_err()
                .to_string()
                .contains("migrateToGWBatchNumber overflows")
        );

        overflow_to.is_active = false;
        overflow_to.migrate_to_gateway_batch = 10;
        overflow_to.migrate_from_gateway_batch = u64::MAX;
        overflow_to.settlement_layer_batch_upper_bound = 9;
        assert!(
            build_raw_intervals(2, vec![Some(overflow_to), None])
                .unwrap_err()
                .to_string()
                .contains("migrateFromGWBatchNumber overflows")
        );

        assert!(
            build_raw_intervals(2, vec![Some(slot(10, 9, false)), None])
                .unwrap_err()
                .to_string()
                .contains("ends before it begins")
        );
        assert!(
            build_raw_intervals(
                2,
                vec![Some(slot(10, 20, false)), Some(slot(30, 40, false))]
            )
            .unwrap_err()
            .to_string()
            .contains("later slots must be empty")
        );
        assert!(
            build_raw_intervals(2, vec![Some(slot(10, 0, true)), None])
                .unwrap_err()
                .to_string()
                .contains("activity contradicts")
        );
    }
}
