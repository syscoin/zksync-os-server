use crate::{Bridgehub, IChainAssetHandler, ZkChain, is_method_missing};
use alloy::primitives::{Address, U256, address};
use alloy::providers::{DynProvider, Provider};
use anyhow::Context;
use std::fmt;
use std::sync::{Arc, Mutex};

const L2_BRIDGEHUB_ADDRESS: Address = address!("0x0000000000000000000000000000000000010002");

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

/// Inclusive batch-number range during which the chain committed to a single settlement layer.
///
/// `last_batch` is `None` for the currently-active (open-ended) interval.
#[derive(Debug, Clone)]
pub struct SettlementLayerInterval {
    pub settlement_layer: IntervalSettlementLayer,
    pub first_batch: u64,
    pub last_batch: Option<u64>,
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

/// Settlement layer intervals for a chain paired with the diamond proxies needed to route batch
/// lookups to the correct RPC.
///
/// The intervals cover all batches from `1` upwards in ascending order, with the last entry being
/// open-ended (`last_batch = None`).
#[derive(Debug, Clone)]
pub struct SettlementLayerIntervals {
    intervals: Arc<Vec<SettlementLayerInterval>>,
    diamond_proxy_l1: ZkChain<DynProvider>,
    /// Diamond proxy of a configured Gateway provider, paired with that SL's chain ID.
    /// `None` means no Gateway provider is available, in which case lookups for Gateway intervals
    /// are unsupported.
    diamond_proxy_gw: Option<GatewayProxy>,
}

// SYSCOIN: keep historical Gateway access lazy after a chain has returned to L1, so current-L1
// startup does not require Gateway RPC availability unless a Gateway interval is actually read.
#[derive(Debug, Clone)]
enum GatewayProxy {
    Ready {
        chain_id: u64,
        proxy: ZkChain<DynProvider>,
    },
    Lazy {
        chain_id: u64,
        provider: DynProvider,
        l2_chain_id: u64,
        cached_proxy: Arc<Mutex<Option<ZkChain<DynProvider>>>>,
    },
}

impl GatewayProxy {
    fn chain_id(&self) -> u64 {
        match self {
            Self::Ready { chain_id, .. } | Self::Lazy { chain_id, .. } => *chain_id,
        }
    }

    async fn proxy(&self) -> anyhow::Result<ZkChain<DynProvider>> {
        match self {
            Self::Ready { proxy, .. } => Ok(proxy.clone()),
            Self::Lazy {
                chain_id,
                provider,
                l2_chain_id,
                cached_proxy,
            } => {
                if let Some(proxy) = cached_proxy
                    .lock()
                    .expect("gateway proxy cache lock poisoned")
                    .clone()
                {
                    return Ok(proxy);
                }

                let provider_chain_id = provider
                    .get_chain_id()
                    .await
                    .context("failed to fetch configured Gateway chain ID")?;
                anyhow::ensure!(
                    provider_chain_id == *chain_id,
                    "configured Gateway chain ID {provider_chain_id} does not match historical Gateway chain ID {chain_id}"
                );
                let proxy = Bridgehub::new(L2_BRIDGEHUB_ADDRESS, provider.clone(), *l2_chain_id)
                    .zk_chain()
                    .await
                    .with_context(|| {
                        format!("failed to fetch historical Gateway diamond proxy for chain {l2_chain_id}")
                    })?;
                let mut cached_proxy = cached_proxy
                    .lock()
                    .expect("gateway proxy cache lock poisoned");
                if let Some(cached_proxy) = cached_proxy.as_ref() {
                    Ok(cached_proxy.clone())
                } else {
                    *cached_proxy = Some(proxy.clone());
                    Ok(proxy)
                }
            }
        }
    }
}

impl SettlementLayerIntervals {
    /// Discovers the intervals on-chain from `IL1ChainAssetHandler.migrationInterval` and stores
    /// the diamond proxies needed to resolve future lookups.
    pub async fn discover(
        chain_asset_handler: Address,
        diamond_proxy_l1: ZkChain<DynProvider>,
        diamond_proxy_gw: Option<(u64, ZkChain<DynProvider>)>,
        chain_id: u64,
    ) -> anyhow::Result<Self> {
        let allow_legacy_no_migrations = diamond_proxy_gw.is_none();
        let intervals = find_settlement_layer_intervals(
            chain_asset_handler,
            diamond_proxy_l1.provider().clone(),
            chain_id,
            allow_legacy_no_migrations,
        )
        .await
        .context("failed to discover settlement layer intervals")?;
        Ok(Self {
            intervals: Arc::new(intervals),
            diamond_proxy_l1,
            diamond_proxy_gw: diamond_proxy_gw
                .map(|(chain_id, proxy)| GatewayProxy::Ready { chain_id, proxy }),
        })
    }

    pub fn intervals(&self) -> &[SettlementLayerInterval] {
        &self.intervals
    }

    pub fn gateway_chain_ids(&self) -> impl Iterator<Item = u64> + '_ {
        gateway_chain_ids(self.intervals())
    }

    // SYSCOIN: attach the configured Gateway RPC without touching the network until a historical
    // Gateway batch/interval must be resolved.
    pub fn set_lazy_gateway_provider(
        &mut self,
        chain_id: u64,
        provider: DynProvider,
        l2_chain_id: u64,
    ) {
        self.diamond_proxy_gw = Some(GatewayProxy::Lazy {
            chain_id,
            provider,
            l2_chain_id,
            cached_proxy: Arc::new(Mutex::new(None)),
        });
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

    /// Returns the diamond proxy that should be used to fetch data about `batch_number`, based on
    /// which settlement layer interval it falls into.
    pub async fn resolve_proxy(&self, batch_number: u64) -> anyhow::Result<ZkChain<DynProvider>> {
        let interval = self.find_interval(batch_number).with_context(|| {
            format!("batch {batch_number} does not belong to any known settlement layer interval")
        })?;
        match interval.settlement_layer {
            IntervalSettlementLayer::L1 => Ok(self.diamond_proxy_l1.clone()),
            IntervalSettlementLayer::Gateway(chain_id) => match &self.diamond_proxy_gw {
                Some(gateway_proxy) if gateway_proxy.chain_id() == chain_id => {
                    gateway_proxy.proxy().await
                }
                Some(gateway_proxy) => anyhow::bail!(
                    "batch {batch_number} was committed on Gateway with chain ID {chain_id} but \
                     the configured Gateway provider is for chain ID {}",
                    gateway_proxy.chain_id()
                ),
                None => anyhow::bail!(
                    "batch {batch_number} was committed on Gateway with chain ID {chain_id} but \
                     no Gateway provider is configured"
                ),
            },
        }
    }

    fn find_interval(&self, batch_number: u64) -> Option<&SettlementLayerInterval> {
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
/// - `MAX_ALLOWED_NUMBER_OF_MIGRATIONS = 2` on-chain, so at most two cycles are supported.
async fn find_settlement_layer_intervals(
    chain_asset_handler: Address,
    provider: DynProvider,
    chain_id: u64,
    allow_legacy_no_migrations: bool,
) -> anyhow::Result<Vec<SettlementLayerInterval>> {
    let cah = IChainAssetHandler::new(chain_asset_handler, provider);
    let total_migrations: u64 = match cah.migrationNumber(U256::from(chain_id)).call().await {
        Ok(n) => n
            .try_into()
            .map_err(|e| anyhow::anyhow!("migrationNumber overflow: {e}"))?,
        // Pre-V31 `ChainAssetHandler` does not expose `migrationNumber`. In that era Gateway
        // migrations are not possible, so the chain has always committed to L1.
        Err(e) if allow_legacy_no_migrations && is_migration_number_unavailable(&e) => {
            tracing::debug!(
                "ChainAssetHandler does not expose migrationNumber; assuming pre-V31 protocol \
                 with no Gateway migrations: {e}"
            );
            return Ok(vec![SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::L1,
                first_batch: 1,
                last_batch: None,
            }]);
        }
        Err(e) => return Err(anyhow::Error::new(e).context("failed to fetch migrationNumber")),
    };

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

    let mut intervals = Vec::new();
    let mut cursor: u64 = 1;
    let mut on_active_gw = false;
    for raw in raw {
        // Uninitialized slots have all fields zero; skip them.
        if raw.settlementLayerChainId.is_zero() {
            continue;
        }
        let sl_chain_id: u64 = raw
            .settlementLayerChainId
            .try_into()
            .map_err(|e| anyhow::anyhow!("settlementLayerChainId overflow: {e}"))?;
        let to_batch: u64 = raw
            .migrateToGWBatchNumber
            .try_into()
            .map_err(|e| anyhow::anyhow!("migrateToGWBatchNumber overflow: {e}"))?;

        // L1 interval leading up to this migration (if the chain committed any batches to L1
        // before it).
        anyhow::ensure!(
            to_batch + 1 >= cursor,
            "settlement layer interval is not in order: {} < {}",
            to_batch + 1,
            cursor
        );
        intervals.push(SettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::L1,
            first_batch: cursor,
            last_batch: Some(to_batch),
        });
        cursor = to_batch + 1;

        if raw.isActive {
            intervals.push(SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::Gateway(sl_chain_id),
                first_batch: cursor,
                last_batch: None,
            });
            on_active_gw = true;
            break;
        }
        let from_batch: u64 = raw
            .migrateFromGWBatchNumber
            .try_into()
            .map_err(|e| anyhow::anyhow!("migrateFromGWBatchNumber overflow: {e}"))?;
        intervals.push(SettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::Gateway(sl_chain_id),
            first_batch: cursor,
            last_batch: Some(from_batch),
        });
        cursor = from_batch + 1;
    }
    if !on_active_gw {
        intervals.push(SettlementLayerInterval {
            settlement_layer: IntervalSettlementLayer::L1,
            first_batch: cursor,
            last_batch: None,
        });
    }
    Ok(intervals)
}

fn is_migration_number_unavailable(err: &alloy::contract::Error) -> bool {
    if is_method_missing(err) {
        return true;
    }
    // SYSCOIN: some older local-chain fixtures use a ChainAssetHandler implementation that
    // reverts with empty data for unknown selectors instead of returning zero data. Treat that
    // shape as "getter unavailable" only for this discovery probe.
    err.as_revert_data().is_some_and(|data| data.is_empty())
}

fn gateway_chain_ids(intervals: &[SettlementLayerInterval]) -> impl Iterator<Item = u64> + '_ {
    intervals
        .iter()
        .filter_map(|interval| match interval.settlement_layer {
            IntervalSettlementLayer::L1 => None,
            IntervalSettlementLayer::Gateway(chain_id) => Some(chain_id),
        })
}

#[cfg(test)]
mod tests {
    use super::{IntervalSettlementLayer, SettlementLayerInterval, gateway_chain_ids};

    #[test]
    fn reports_gateway_chain_ids_from_discovered_intervals() {
        let intervals = vec![
            SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::L1,
                first_batch: 1,
                last_batch: Some(5),
            },
            SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::Gateway(506),
                first_batch: 6,
                last_batch: Some(8),
            },
            SettlementLayerInterval {
                settlement_layer: IntervalSettlementLayer::L1,
                first_batch: 9,
                last_batch: None,
            },
        ];

        assert_eq!(gateway_chain_ids(&intervals).collect::<Vec<_>>(), vec![506]);
    }
}
