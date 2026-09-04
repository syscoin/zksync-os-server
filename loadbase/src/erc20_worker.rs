//! ERC‑20 worker; keeps bounded windows of signed transactions in flight.
//! Adds gas‑price (legacy) so nodes don’t reject with “feeCap 0 below chain minimum”.

use crate::{erc20::SimpleERC20, metrics::Metrics};
use ethers::{prelude::*, types::U256};
use parking_lot::RwLock;
use rand::{rngs::StdRng, seq::SliceRandom};
use rand_distr::{Distribution, Normal};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, Semaphore};

const JITTER_SIGMA: f64 = 0.20;
const GAS_PRICE_QUOTE_TTL: Duration = Duration::from_secs(1);
type EthSigner = SignerMiddleware<Provider<Http>, LocalWallet>;

enum ReceiptOutcome {
    Included { refund: U256 },
    Uncertain,
}

enum SubmissionEvent {
    Permit(tokio::sync::OwnedSemaphorePermit),
    Outcome(ReceiptOutcome),
    Closed,
}

struct RunControl {
    running:        Arc<AtomicBool>,
    max_in_flight:  u32,
    active_wallets: AtomicUsize,
}

impl RunControl {
    fn window_size(&self) -> usize {
        fair_window_size(
            self.max_in_flight,
            self.active_wallets.load(Ordering::Relaxed),
        )
    }
}

struct ActiveWalletGuard {
    control: Arc<RunControl>,
}

impl Drop for ActiveWalletGuard {
    fn drop(&mut self) {
        let previous = self.control.active_wallets.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0);
    }
}

struct WalletSubmitter<'a> {
    signer:     &'a EthSigner,
    token:      &'a SimpleERC20<EthSigner>,
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

fn apply_receipt_outcome(
    outcome: ReceiptOutcome,
    in_flight: &mut usize,
    spendable: &mut U256,
) -> bool {
    *in_flight = (*in_flight).saturating_sub(1);
    match outcome {
        ReceiptOutcome::Included { refund } => {
            *spendable += refund;
            true
        }
        ReceiptOutcome::Uncertain => false,
    }
}

async fn next_submission_event(
    sem: &Arc<Semaphore>,
    outcome_rx: &mut mpsc::UnboundedReceiver<ReceiptOutcome>,
    has_in_flight: bool,
) -> SubmissionEvent {
    if !has_in_flight {
        return sem
            .clone()
            .acquire_owned()
            .await
            .map_or(SubmissionEvent::Closed, SubmissionEvent::Permit);
    }
    tokio::select! {
        biased;
        outcome = outcome_rx.recv() => outcome
            .map_or(SubmissionEvent::Closed, SubmissionEvent::Outcome),
        permit = sem.clone().acquire_owned() => permit
            .map_or(SubmissionEvent::Closed, SubmissionEvent::Permit),
    }
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
    async fn submit_one(
        &self,
        nonce: &mut u64,
        gas_price: U256,
        spendable: &mut U256,
        in_flight: &mut usize,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), ()> {
        let max_cost = self.cfg.gas_limit.saturating_mul(gas_price);

        // SYSCOIN: JSON-RPC batches are not atomic and may admit a higher nonce after
        // rejecting a lower one. Admit each wallet's dependent nonce chain in order;
        // wallets and receipt waits remain concurrent.
        // SYSCOIN: Pool validation checks transactions individually. Reserve the
        // full cost across outstanding transactions so every admitted nonce can run.
        if !try_reserve(spendable, max_cost) {
            return Err(());
        }
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
                *in_flight += 1;
                spawn_receipt_waiter(
                    pending.tx_hash(),
                    permit,
                    max_cost,
                    self.provider.clone(),
                    self.metrics.clone(),
                    self.cfg.receipt_timeout,
                    self.outcome_tx.clone(),
                );
                Ok(())
            }
            Err(err) => {
                *spendable += max_cost;
                eprintln!("❗ wallet submission stopped: {err}");
                Err(())
            }
        }
    }
}

async fn run_wallet(
    idx:     usize,
    wallet:  LocalWallet,
    provider: Provider<Http>,
    sem:     Arc<Semaphore>,
    metrics: Metrics,
    active_wallet: ActiveWalletGuard,
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
        cfg: &cfg,
        provider: &provider,
        metrics: &metrics,
        outcome_tx: &outcome_tx,
    };
    let mut in_flight = 0usize;
    let mut gas_quote: Option<(Instant, U256)> = None;
    println!("erc20 wallet {idx} start‑nonce {nonce}");

    while active_wallet.control.running.load(Ordering::Relaxed) {
        while let Ok(outcome) = outcome_rx.try_recv() {
            if !apply_receipt_outcome(outcome, &mut in_flight, &mut spendable) {
                eprintln!("❗ erc20 wallet {idx} stopped after an uncertain receipt");
                return;
            }
        }
        if in_flight >= active_wallet.control.window_size() {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        // SYSCOIN: Tokio's queued semaphore acquisition is FIFO. When a receipt
        // is outstanding, observe it first so an uncertain nonce chain stops
        // before this wallet can submit a higher nonce.
        let permit = match next_submission_event(&sem, &mut outcome_rx, in_flight > 0).await {
            SubmissionEvent::Permit(permit) => permit,
            SubmissionEvent::Outcome(outcome) => {
                if !apply_receipt_outcome(outcome, &mut in_flight, &mut spendable) {
                    eprintln!("❗ erc20 wallet {idx} stopped after an uncertain receipt");
                    return;
                }
                continue;
            }
            SubmissionEvent::Closed => return,
        };
        if !active_wallet.control.running.load(Ordering::Relaxed) {
            break;
        }

        // SYSCOIN: Reuse one fresh quote across an immediate refill burst so
        // load generation measures transaction admission, not eth_gasPrice.
        let gas_price = match gas_quote {
            Some((quoted_at, price)) if quoted_at.elapsed() < GAS_PRICE_QUOTE_TTL => price,
            _ => match provider.get_gas_price().await {
                Ok(price) => {
                    gas_quote = Some((Instant::now(), price));
                    price
                }
                Err(e) => {
                    drop(permit);
                    // SYSCOIN: An arbitrary fallback can falsely exhaust low-balance wallets.
                    eprintln!("❗ gas‑price fetch error {e} – retrying");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        if in_flight > 0 {
            if let Ok(outcome) = outcome_rx.try_recv() {
                drop(permit);
                if !apply_receipt_outcome(outcome, &mut in_flight, &mut spendable) {
                    eprintln!("❗ erc20 wallet {idx} stopped after an uncertain receipt");
                    return;
                }
                continue;
            }
        }
        if !active_wallet.control.running.load(Ordering::Relaxed) {
            break;
        }
        let max_cost = cfg.gas_limit.saturating_mul(gas_price);
        if spendable < max_cost {
            drop(permit);
            if in_flight == 0 {
                println!(
                    "erc20 wallet {idx} exhausted its safe gas budget; stopping at nonce {nonce}"
                );
                break;
            }
            let Some(outcome) = outcome_rx.recv().await else {
                return;
            };
            if !apply_receipt_outcome(outcome, &mut in_flight, &mut spendable) {
                eprintln!("❗ erc20 wallet {idx} stopped after an uncertain receipt");
                return;
            }
            continue;
        }
        if submitter
            .submit_one(
                &mut nonce,
                gas_price,
                &mut spendable,
                &mut in_flight,
                permit,
            )
            .await
            .is_err()
        {
            break;
        }
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
    let control = Arc::new(RunControl {
        running,
        max_in_flight,
        active_wallets: AtomicUsize::new(wallets.len()),
    });
    let cfg  = Arc::new(cfg);
    let sem  = Arc::new(Semaphore::new(max_in_flight as usize));

    wallets
        .into_iter()
        .enumerate()
        .map(|(idx, wallet)| {
            // SYSCOIN: A fixed share strands permits as varied-balance workers
            // finish. Create the guard before spawn so even cancellation before
            // the task's first poll redistributes that share.
            let active_wallet = ActiveWalletGuard {
                control: control.clone(),
            };
            tokio::spawn(run_wallet(
                idx,
                wallet,
                provider.clone(),
                sem.clone(),
                metrics.clone(),
                active_wallet,
                cfg.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        fair_window_size, next_submission_event, receipt_refund, try_reserve, ActiveWalletGuard,
        ReceiptOutcome, RunControl, SubmissionEvent,
    };
    use ethers::types::U256;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::{mpsc, Semaphore};

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
        assert_eq!(fair_window_size(50, 100), 1);
        assert_eq!(fair_window_size(50, 49), 2);
        assert_eq!(fair_window_size(0, 20), 0);
        assert_eq!(fair_window_size(200, 0), 0);
    }

    #[test]
    fn redistributes_window_when_a_wallet_exits() {
        let control = Arc::new(RunControl {
            running: Arc::new(AtomicBool::new(true)),
            max_in_flight: 1000,
            active_wallets: AtomicUsize::new(10),
        });
        assert_eq!(control.window_size(), 100);

        drop(ActiveWalletGuard {
            control: control.clone(),
        });

        assert_eq!(control.active_wallets.load(Ordering::Relaxed), 9);
        assert_eq!(control.window_size(), 112);
    }

    #[tokio::test]
    async fn receipt_outcome_preempts_an_available_permit() {
        let sem = Arc::new(Semaphore::new(1));
        let (outcome_tx, mut outcome_rx) = mpsc::unbounded_channel();
        outcome_tx.send(ReceiptOutcome::Uncertain).unwrap();

        assert!(matches!(
            next_submission_event(&sem, &mut outcome_rx, true).await,
            SubmissionEvent::Outcome(ReceiptOutcome::Uncertain)
        ));
        assert_eq!(sem.available_permits(), 1);
    }
}
