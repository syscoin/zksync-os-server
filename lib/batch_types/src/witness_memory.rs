use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WitnessMemoryError {
    #[error("witness memory budget must be greater than zero")]
    InvalidLimit,
    #[error("witness reservation of {requested} bytes exceeds the {limit}-byte budget")]
    ExceedsLimit { requested: usize, limit: usize },
    #[error("witness allocation capacity overflows its byte count")]
    AllocationSizeOverflow,
    #[error("witness allocation needs {allocated} bytes but only {reserved} were reserved")]
    ReservationTooSmall { allocated: usize, reserved: usize },
    #[error("cannot grow a witness reservation from {reserved} to {requested} bytes")]
    ReservationWouldGrow { reserved: usize, requested: usize },
}

/// Accounts for retained witness allocations independently of batch-count admission.
#[derive(Debug)]
pub struct WitnessMemoryBudget {
    limit: usize,
    used: Mutex<usize>,
    available: Notify,
}

impl WitnessMemoryBudget {
    pub fn new(limit: usize) -> Result<Arc<Self>, WitnessMemoryError> {
        if limit == 0 {
            return Err(WitnessMemoryError::InvalidLimit);
        }
        Ok(Arc::new(Self {
            limit,
            used: Mutex::new(0),
            available: Notify::new(),
        }))
    }

    pub fn limit_bytes(&self) -> usize {
        self.limit
    }

    pub fn used_bytes(&self) -> usize {
        *self.used.lock().expect("witness budget mutex poisoned")
    }

    /// Reserve before allocating or generating a witness. Cancellation while waiting owns no bytes.
    pub async fn reserve(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<WitnessMemoryReservation, WitnessMemoryError> {
        if bytes > self.limit {
            return Err(WitnessMemoryError::ExceedsLimit {
                requested: bytes,
                limit: self.limit,
            });
        }
        loop {
            let notified = self.available.notified();
            tokio::pin!(notified);
            // Register before checking capacity, so a refund between the check and await is seen.
            notified.as_mut().enable();
            {
                let mut used = self.used.lock().expect("witness budget mutex poisoned");
                if bytes <= self.limit - *used {
                    *used += bytes;
                    return Ok(WitnessMemoryReservation {
                        budget: Arc::clone(self),
                        bytes,
                    });
                }
            }
            notified.await;
        }
    }

    fn refund(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        {
            let mut used = self.used.lock().expect("witness budget mutex poisoned");
            *used = used
                .checked_sub(bytes)
                .expect("witness budget over-refunded");
        }
        self.available.notify_waiters();
    }
}

/// A move-only reservation that follows retained witness ownership through handoff and rollback.
#[derive(Debug)]
#[must_use = "a witness reservation must remain owned until its allocation is released"]
pub struct WitnessMemoryReservation {
    budget: Arc<WitnessMemoryBudget>,
    bytes: usize,
}

impl WitnessMemoryReservation {
    pub fn reserved_bytes(&self) -> usize {
        self.bytes
    }

    pub fn shrink_to(&mut self, bytes: usize) -> Result<(), WitnessMemoryError> {
        if bytes > self.bytes {
            return Err(WitnessMemoryError::ReservationWouldGrow {
                reserved: self.bytes,
                requested: bytes,
            });
        }
        let refunded = self.bytes - bytes;
        self.bytes = bytes;
        self.budget.refund(refunded);
        Ok(())
    }
}

impl Drop for WitnessMemoryReservation {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}

struct WitnessAllocation {
    // Fields drop in declaration order: free the backing buffer before quota can wake a generator.
    words: Vec<u32>,
    reservation: Option<WitnessMemoryReservation>,
}

/// Clones share both the immutable witness allocation and its byte reservation.
#[derive(Clone)]
pub struct WitnessInput(Arc<WitnessAllocation>);

impl WitnessInput {
    pub fn with_reservation(
        words: Vec<u32>,
        reservation: WitnessMemoryReservation,
    ) -> Result<Self, WitnessMemoryError> {
        // Establish buffer-before-quota drop ordering even on capacity validation failures.
        let mut allocation = WitnessAllocation {
            words,
            reservation: Some(reservation),
        };
        let allocated = allocation
            .words
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or(WitnessMemoryError::AllocationSizeOverflow)?;
        let reservation = allocation.reservation.as_mut().unwrap();
        if allocated > reservation.reserved_bytes() {
            return Err(WitnessMemoryError::ReservationTooSmall {
                allocated,
                reserved: reservation.reserved_bytes(),
            });
        }
        reservation.shrink_to(allocated)?;
        Ok(Self(Arc::new(allocation)))
    }

    pub fn as_slice(&self) -> &[u32] {
        self.0.words.as_slice()
    }

    pub fn capacity(&self) -> usize {
        self.0.words.capacity()
    }
}

// Compatibility constructors and deserialization have no process-local admission context.
// Production generation must use `with_reservation` before publishing the witness.
impl From<Vec<u32>> for WitnessInput {
    fn from(words: Vec<u32>) -> Self {
        Self(Arc::new(WitnessAllocation {
            words,
            reservation: None,
        }))
    }
}

impl Deref for WitnessInput {
    type Target = [u32];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl AsRef<[u32]> for WitnessInput {
    fn as_ref(&self) -> &[u32] {
        self.as_slice()
    }
}

impl fmt::Debug for WitnessInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

// Delegating to the original Vec representation preserves existing JSON and binary formats.
impl Serialize for WitnessInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.words.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WitnessInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<u32>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batcher_model::ProverInput;
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::task::Poll;
    use std::time::Duration;

    async fn assert_pending_once<F: Future>(mut future: Pin<&mut F>) {
        poll_fn(|context| {
            assert!(future.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn exact_limit_and_oversize_are_explicit_and_overflow_safe() {
        assert!(matches!(
            WitnessMemoryBudget::new(0),
            Err(WitnessMemoryError::InvalidLimit)
        ));
        let budget = WitnessMemoryBudget::new(8).unwrap();
        assert_eq!(budget.limit_bytes(), 8);
        assert!(matches!(
            budget.reserve(9).await,
            Err(WitnessMemoryError::ExceedsLimit {
                requested: 9,
                limit: 8
            })
        ));
        assert_eq!(budget.used_bytes(), 0);
        let full = budget.reserve(8).await.unwrap();
        assert_eq!(budget.used_bytes(), 8);
        let zero = budget.reserve(0).await.unwrap();
        drop(zero);
        assert_eq!(budget.used_bytes(), 8);
        drop(full);
        let maximum = WitnessMemoryBudget::new(usize::MAX).unwrap();
        let full = maximum.reserve(usize::MAX).await.unwrap();
        let mut waiting = Box::pin(maximum.reserve(1));
        assert_pending_once(waiting.as_mut()).await;
        drop(waiting);
        drop(full);
        assert_eq!(maximum.used_bytes(), 0);
    }

    #[tokio::test]
    async fn refund_resumes_a_waiter_without_holding_the_counter_lock() {
        let budget = WitnessMemoryBudget::new(8).unwrap();
        let full = budget.reserve(8).await.unwrap();
        let mut waiting = Box::pin(budget.reserve(4));
        assert_pending_once(waiting.as_mut()).await;
        assert_eq!(budget.used_bytes(), 8);
        drop(full);
        let acquired = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(budget.used_bytes(), 4);
        drop(acquired);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_capacity_wait_does_not_consume_quota() {
        let budget = WitnessMemoryBudget::new(8).unwrap();
        let full = budget.reserve(8).await.unwrap();
        let mut waiting = Box::pin(budget.reserve(4));
        assert_pending_once(waiting.as_mut()).await;
        drop(waiting);
        assert_eq!(budget.used_bytes(), 8);
        drop(full);
        let next = budget.reserve(8).await.unwrap();
        assert_eq!(budget.used_bytes(), 8);
        drop(next);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn cancelling_an_owner_refunds_its_reservation() {
        let budget = WitnessMemoryBudget::new(8).unwrap();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let owner_budget = Arc::clone(&budget);
        let owner = tokio::spawn(async move {
            let _reservation = owner_budget.reserve(8).await.unwrap();
            ready_sender.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready_receiver.await.unwrap();
        assert_eq!(budget.used_bytes(), 8);
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn shrink_refunds_exactly_and_never_grows() {
        let budget = WitnessMemoryBudget::new(16).unwrap();
        let mut reservation = budget.reserve(16).await.unwrap();
        let mut waiting = Box::pin(budget.reserve(8));
        assert_pending_once(waiting.as_mut()).await;
        assert!(matches!(
            reservation.shrink_to(17),
            Err(WitnessMemoryError::ReservationWouldGrow { .. })
        ));
        assert_eq!(budget.used_bytes(), 16);
        reservation.shrink_to(8).unwrap();
        let next = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(budget.used_bytes(), 16);
        assert_eq!(reservation.reserved_bytes(), 8);
        reservation.shrink_to(0).unwrap();
        assert_eq!(budget.used_bytes(), 8);
        drop(reservation);
        assert_eq!(budget.used_bytes(), 8);
        drop(next);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn clones_share_allocation_and_hold_quota_until_the_last_owner() {
        let budget = WitnessMemoryBudget::new(32).unwrap();
        let reservation = budget.reserve(32).await.unwrap();
        let mut words = Vec::with_capacity(4);
        words.extend([3, 5]);
        let input = WitnessInput::with_reservation(words, reservation).unwrap();
        assert_eq!(input.capacity(), 4);
        assert_eq!(budget.used_bytes(), 16);
        let clone = input.clone();
        assert_eq!(input.as_slice().as_ptr(), clone.as_slice().as_ptr());
        let mut waiting = Box::pin(budget.reserve(32));
        assert_pending_once(waiting.as_mut()).await;
        drop(input);
        assert_eq!(budget.used_bytes(), 16);
        assert_pending_once(waiting.as_mut()).await;
        drop(clone);
        let next = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(budget.used_bytes(), 32);
        drop(next);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn a_refund_wakes_all_waiters_that_can_fit_together() {
        let budget = WitnessMemoryBudget::new(8).unwrap();
        let full = budget.reserve(8).await.unwrap();
        let mut first = Box::pin(budget.reserve(2));
        let mut second = Box::pin(budget.reserve(3));
        assert_pending_once(first.as_mut()).await;
        assert_pending_once(second.as_mut()).await;
        drop(full);
        let first = tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap();
        let second = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(budget.used_bytes(), 5);
        drop(first);
        drop(second);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[tokio::test]
    async fn allocation_capacity_not_word_length_must_fit_the_reservation() {
        let budget = WitnessMemoryBudget::new(32).unwrap();
        let reservation = budget.reserve(8).await.unwrap();
        let mut words = Vec::with_capacity(4);
        words.push(7);
        assert!(matches!(
            WitnessInput::with_reservation(words, reservation),
            Err(WitnessMemoryError::ReservationTooSmall {
                allocated: 16,
                reserved: 8
            })
        ));
        assert_eq!(budget.used_bytes(), 0);
    }

    #[test]
    fn serde_preserves_word_vectors_and_prover_input_enum_tags() {
        #[derive(Serialize)]
        enum LegacyProverInput {
            Real(Vec<u32>),
            Fake,
        }
        for words in [vec![], vec![0, 1, u32::MAX]] {
            let input = WitnessInput::from(words.clone());
            assert_eq!(
                serde_json::to_vec(&input).unwrap(),
                serde_json::to_vec(&words).unwrap()
            );
            let current = ProverInput::Real(input);
            assert_eq!(
                serde_json::to_vec(&current).unwrap(),
                serde_json::to_vec(&LegacyProverInput::Real(words.clone())).unwrap()
            );
            let decoded: ProverInput =
                serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap();
            assert_eq!(decoded.unwrap_real(), words);
        }
        assert_eq!(
            serde_json::to_vec(&ProverInput::Fake).unwrap(),
            serde_json::to_vec(&LegacyProverInput::Fake).unwrap()
        );
    }
}
