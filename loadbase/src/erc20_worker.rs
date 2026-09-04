//! ERC‑20 worker; keeps bounded windows of signed transactions in flight.
//! Adds gas‑price (legacy) so nodes don’t reject with “feeCap 0 below chain minimum”.

use crate::{erc20::SimpleERC20, metrics::Metrics};
use ethers::{prelude::*, types::U256};
use parking_lot::RwLock;
use rand::{rngs::StdRng, seq::SliceRandom};
use rand_distr::{Distribution, Normal};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, Semaphore};

const JITTER_SIGMA: f64 = 0.20;
type EthSigner = SignerMiddleware<Provider<Http>, LocalWallet>;

enum ReceiptOutcome {
    Included { refund: U256 },
    Uncertain,
}

struct RunControl {
    running:     Arc<AtomicBool>,
    window_size: usize,
}

struct WalletSubmitter<'a> {
    signer:     &'a EthSigner,
    token:      &'a SimpleERC20<EthSigner>,
    sem:        &'a Arc<Semaphore>,
    cfg:        &'a WorkerConfig,
    provider:   &'a Provider<Http>,
    metrics:    &'a Metrics,
    outcome_tx: &'a mpsc::UnboundedSender<ReceiptOutcome>,
}

pub struct WorkerConfig {
    pub gas_limit:   U256,
    pub mean_amt:    U256,
    pub token_addr:  Address,
    pub dest_random: bool,
    pub all_addrs:   Vec<Address>,
    pub rng:         Arc<RwLock<StdRng>>,
    pub receipt_timeout: Duration,
}

fn try_reserve(spendable: &mut U256, max_cost: U256) -> bool {
    if max_cost > *spendable {
        return false;
    }
    *spendable -= max_cost;
    true
}

fn fair_window_size(max_in_flight: u32, wallet_count: usize) -> usize {
    if max_in_flight == 0 || wallet_count == 0 {
        return 0;
    }
    (max_in_flight as usize).div_ceil(wallet_count)
}

fn receipt_refund(max_cost: U256, gas_used: Option<U256>, gas_price: Option<U256>) -> U256 {
    gas_used
        .zip(gas_price)
        .map_or(U256::zero(), |(gas_used, gas_price)| {
            max_cost.saturating_sub(gas_used.saturating_mul(gas_price))
        })
}

fn jitter_amount(mean: U256, rng: &RwLock<StdRng>) -> U256 {
    let delta = {
        let mut g = rng.write();
        Normal::new(0.0, JITTER_SIGMA).unwrap().sample(&mut *g)
    };
    if delta == 0.0 {
        return mean;
    }
    let d = U256::from((mean.as_u128() as f64 * delta.abs()) as u128);
    if delta.is_sign_positive() { mean + d } else { mean - d }
}

fn choose_dest(dest_random: bool, all_addrs: &[Address], self_addr: Address, rng: &RwLock<StdRng>) -> Address {
    if dest_random {
        return H160::random();
    }
    loop {
        let cand = {
            let mut g = rng.write();
            *all_addrs.choose(&mut *g).unwrap()
        };
        if cand != self_addr {
            return cand;
        }
    }
}

fn spawn_receipt_waiter(
    tx_hash:  H256,
    permit:   tokio::sync::OwnedSemaphorePermit,
    max_cost: U256,
    provider: Provider<Http>,
    metrics:  Metrics,
    receipt_timeout: Duration,
    outcome_tx: mpsc::UnboundedSender<ReceiptOutcome>,
) {
    tokio::spawn(async move {
        let t_inc = Instant::now();
        loop {
            if t_inc.elapsed() >= receipt_timeout {
                eprintln!(
                    "tx {tx_hash:?} receipt unconfirmed after {}s; status uncertain",
                    receipt_timeout.as_secs()
                );
                let _ = outcome_tx.send(ReceiptOutcome::Uncertain);
                break;
            }
            match provider.get_transaction_receipt(tx_hash).await {
                Ok(Some(receipt)) => {
                    metrics.record_included(t_inc.elapsed().as_millis() as u64);
                    let _ = outcome_tx.send(ReceiptOutcome::Included {
                        refund: receipt_refund(
                            max_cost,
                            receipt.gas_used,
                            receipt.effective_gas_price,
                        ),
                    });
                    break;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                Err(e)   => {
                    eprintln!("receipt poll error for {tx_hash:?}: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        drop(permit); // free slot
    });
}

impl WalletSubmitter<'_> {
    async fn submit_window(
        &self,
        nonce: &mut u64,
        gas_price: U256,
        spendable: &mut U256,
        allowance: usize,
    ) -> Result<usize, ()> {
        let mut submitted = 0;
        let max_cost = self.cfg.gas_limit.saturating_mul(gas_price);

        // SYSCOIN: JSON-RPC batches are not atomic and may admit a higher nonce after
        // rejecting a lower one. Admit each wallet's dependent nonce chain in order;
        // wallets and receipt waits remain concurrent.
        for _ in 0..allowance {
            // SYSCOIN: Pool validation checks transactions individually. Reserve the
            // full cost across outstanding transactions so every admitted nonce can run.
            if !try_reserve(spendable, max_cost) {
                break;
            }
            let permit = match self.sem.clone().try_acquire_owned() {
                Ok(p)  => p,
                Err(_) => {
                    *spendable += max_cost;
                    break;
                }
            };
            let dest = choose_dest(
                self.cfg.dest_random,
                &self.cfg.all_addrs,
                self.signer.address(),
                &self.cfg.rng,
            );
            let mut call = self
                .token
                .transfer(dest, jitter_amount(self.cfg.mean_amt, &self.cfg.rng));
            call.tx.set_gas(self.cfg.gas_limit);
            call.tx.set_gas_price(gas_price);
            call.tx.set_nonce(*nonce);
            let sig = self
                .signer
                .signer()
                .sign_transaction(&call.tx)
                .await
                .expect("sign");
            let raw = call.tx.rlp_signed(&sig);
            let sent_at = Instant::now();

            match self.provider.send_raw_transaction(raw).await {
                Ok(pending) => {
                    self.metrics
                        .record_submitted(sent_at.elapsed().as_millis() as u64);
                    *nonce += 1;
                    submitted += 1;
                    spawn_receipt_waiter(
                        pending.tx_hash(),
                        permit,
                        max_cost,
                        self.provider.clone(),
                        self.metrics.clone(),
                        self.cfg.receipt_timeout,
                        self.outcome_tx.clone(),
                    );
                }
                Err(err) => {
                    *spendable += max_cost;
                    eprintln!("❗ wallet submission stopped after {submitted} txs: {err}");
                    return Err(());
                }
            }
        }
        Ok(submitted)
    }
}

async fn run_wallet(
    idx:     usize,
    wallet:  LocalWallet,
    provider: Provider<Http>,
    sem:     Arc<Semaphore>,
    metrics: Metrics,
    control: Arc<RunControl>,
    cfg:     Arc<WorkerConfig>,
) {
    let signer = SignerMiddleware::new(provider.clone(), wallet);
    let token  = SimpleERC20::new(cfg.token_addr, Arc::new(signer.clone()));

    let latest_nonce = signer
        .get_transaction_count(signer.address(), Some(BlockNumber::Latest.into()))
        .await
        .expect("latest nonce")
        .as_u64();
    let mut nonce = signer
        .get_transaction_count(signer.address(), Some(BlockNumber::Pending.into()))
        .await
        .expect("nonce")
        .as_u64();
    // SYSCOIN: Load wallets must be exclusive to this process. Standard RPC can
    // detect contiguous leftovers here, but cannot enumerate a gapped pool tail.
    if nonce != latest_nonce {
        eprintln!(
            "❗ erc20 wallet {idx} has contiguous pending transactions: latest={latest_nonce}, pending={nonce}; skipping"
        );
        return;
    }
    let mut spendable = signer
        .get_balance(signer.address(), Some(BlockNumber::Latest.into()))
        .await
        .expect("balance");
    let (outcome_tx, mut outcome_rx) = mpsc::unbounded_channel();
    let submitter = WalletSubmitter {
        signer: &signer,
        token: &token,
        sem: &sem,
        cfg: &cfg,
        provider: &provider,
        metrics: &metrics,
        outcome_tx: &outcome_tx,
    };
    let mut in_flight = 0usize;
    println!("erc20 wallet {idx} start‑nonce {nonce}");

    while control.running.load(Ordering::Relaxed) {
        while let Ok(outcome) = outcome_rx.try_recv() {
            in_flight = in_flight.saturating_sub(1);
            match outcome {
                ReceiptOutcome::Included { refund } => spendable += refund,
                ReceiptOutcome::Uncertain => {
                    eprintln!("❗ erc20 wallet {idx} stopped after an uncertain receipt");
                    return;
                }
            }
        }
        let allowance = control.window_size.saturating_sub(in_flight);
        if allowance == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        let gas_price = match provider.get_gas_price().await {
            Ok(p)  => p,
            Err(e) => {
                // SYSCOIN: An arbitrary fallback can falsely exhaust low-balance wallets.
                eprintln!("❗ gas‑price fetch error {e} – retrying");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let submitted = submitter
            .submit_window(&mut nonce, gas_price, &mut spendable, allowance)
            .await;

        let Ok(submitted) = submitted else {
            break;
        };
        if submitted == 0 {
            if in_flight == 0
                && spendable < cfg.gas_limit.saturating_mul(gas_price)
            {
                println!(
                    "erc20 wallet {idx} exhausted its safe gas budget; stopping at nonce {nonce}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        in_flight += submitted;
    }
}

pub fn spawn_erc20_workers(
    provider: Provider<Http>,
    wallets: Vec<LocalWallet>,
    metrics: Metrics,
    running: Arc<AtomicBool>,
    max_in_flight: u32,
    cfg: WorkerConfig,
) -> Vec<tokio::task::JoinHandle<()>> {
    let window_size = fair_window_size(max_in_flight, wallets.len());
    let control = Arc::new(RunControl { running, window_size });
    let cfg  = Arc::new(cfg);
    let sem  = Arc::new(Semaphore::new(max_in_flight as usize));

    wallets
        .into_iter()
        .enumerate()
        .map(|(idx, wallet)| {
            tokio::spawn(run_wallet(
                idx, wallet,
                provider.clone(), sem.clone(),
                metrics.clone(), control.clone(),
                cfg.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{fair_window_size, receipt_refund, try_reserve};
    use ethers::types::U256;

    #[test]
    fn reserves_each_outstanding_transaction_max_cost() {
        let mut spendable = U256::from(100u64);

        assert!(try_reserve(&mut spendable, U256::from(40u64)));
        assert!(try_reserve(&mut spendable, U256::from(40u64)));
        assert!(!try_reserve(&mut spendable, U256::from(40u64)));
        assert_eq!(spendable, U256::from(20u64));
    }

    #[test]
    fn refunds_only_receipt_proven_unused_gas() {
        assert_eq!(
            receipt_refund(
                U256::from(100u64),
                Some(U256::from(30u64)),
                Some(U256::from(2u64)),
            ),
            U256::from(40u64)
        );
        assert_eq!(receipt_refund(U256::from(100u64), None, None), U256::zero());
    }

    #[test]
    fn divides_global_limit_into_fair_wallet_windows() {
        assert_eq!(fair_window_size(200, 20), 10);
        assert_eq!(fair_window_size(201, 20), 11);
        assert_eq!(fair_window_size(5, 20), 1);
        assert_eq!(fair_window_size(0, 20), 0);
        assert_eq!(fair_window_size(200, 0), 0);
    }
}
