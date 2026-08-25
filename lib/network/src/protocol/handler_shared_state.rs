use super::events::ProtocolEvent;
use reth_network_peers::PeerId;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, mpsc, oneshot};

type SessionActivationKey = (PeerId, SocketAddr);

/// SYSCOIN: Bridges reth's accepted-session event back to protocol handlers, which are installed
/// while an RLPx session is still tentative. Keying by both authenticated identity and the exact
/// remote socket distinguishes the two physical connections created by a simultaneous dial.
#[derive(Debug, Clone, Default)]
pub struct SessionActivationRegistry {
    inner: Arc<Mutex<SessionActivationState>>,
}

#[derive(Debug)]
struct SessionActivationState {
    next_token: u64,
    waiters: HashMap<SessionActivationKey, SessionActivationWaiters>,
}

#[derive(Debug, Default)]
struct SessionActivationWaiters {
    replay: HashMap<u64, oneshot::Sender<()>>,
    twofa: HashMap<u64, oneshot::Sender<()>>,
}

impl SessionActivationWaiters {
    fn get_mut(&mut self, lane: SessionActivationLane) -> &mut HashMap<u64, oneshot::Sender<()>> {
        match lane {
            SessionActivationLane::Replay => &mut self.replay,
            SessionActivationLane::Twofa => &mut self.twofa,
        }
    }

    fn is_empty(&self) -> bool {
        self.replay.is_empty() && self.twofa.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
enum SessionActivationLane {
    Replay,
    Twofa,
}

impl Default for SessionActivationState {
    fn default() -> Self {
        Self {
            next_token: 1,
            waiters: HashMap::new(),
        }
    }
}

/// SYSCOIN: Dropping a tentative handler's waiter removes only its exact monotonic token. Thus a
/// rejected pending session cannot be activated by a later event for another physical connection.
#[derive(Debug)]
pub(crate) struct SessionActivationWaiter {
    registry: SessionActivationRegistry,
    key: SessionActivationKey,
    token: u64,
    lane: SessionActivationLane,
    receiver: oneshot::Receiver<()>,
}

impl SessionActivationRegistry {
    fn subscribe(
        &self,
        peer_id: PeerId,
        remote_addr: SocketAddr,
        lane: SessionActivationLane,
    ) -> SessionActivationWaiter {
        let key = (peer_id, remote_addr);
        let (sender, receiver) = oneshot::channel();
        let token = {
            let mut inner = self
                .inner
                .lock()
                .expect("session activation registry lock poisoned");
            let token = inner.next_token;
            inner.next_token = inner
                .next_token
                .checked_add(1)
                .expect("session activation token exhausted");
            inner
                .waiters
                .entry(key)
                .or_default()
                .get_mut(lane)
                .insert(token, sender);
            token
        };
        SessionActivationWaiter {
            registry: self.clone(),
            key,
            token,
            lane,
            receiver,
        }
    }

    /// SYSCOIN: Activates replay handlers belonging to the exact `(PeerId, remote_addr)` RLPx
    /// session accepted by reth.
    ///
    /// `zks_2fa` remains gated until replay reaches its role-specific mutual-stream proof point.
    pub fn activate(&self, peer_id: PeerId, remote_addr: SocketAddr) {
        let waiters = self.take_waiters((peer_id, remote_addr), SessionActivationLane::Replay);
        for sender in waiters.into_values() {
            let _ = sender.send(());
        }
    }

    // SYSCOIN: The zks owner calls this only at its role-specific mutual-stream proof point. On
    // the MN that follows `Established`, so verifier events cannot precede session creation.
    fn activate_twofa(&self, peer_id: PeerId, remote_addr: SocketAddr) {
        let waiters = self.take_waiters((peer_id, remote_addr), SessionActivationLane::Twofa);
        for sender in waiters.into_values() {
            let _ = sender.send(());
        }
    }

    fn take_waiters(
        &self,
        key: SessionActivationKey,
        lane: SessionActivationLane,
    ) -> HashMap<u64, oneshot::Sender<()>> {
        let mut inner = self
            .inner
            .lock()
            .expect("session activation registry lock poisoned");
        let (waiters, remove_key) = if let Some(waiters) = inner.waiters.get_mut(&key) {
            let selected = std::mem::take(waiters.get_mut(lane));
            (selected, waiters.is_empty())
        } else {
            (HashMap::new(), false)
        };
        if remove_key {
            inner.waiters.remove(&key);
        }
        waiters
    }

    fn remove_waiter(&self, key: SessionActivationKey, token: u64, lane: SessionActivationLane) {
        let mut inner = self
            .inner
            .lock()
            .expect("session activation registry lock poisoned");
        let remove_key = if let Some(waiters) = inner.waiters.get_mut(&key) {
            waiters.get_mut(lane).remove(&token);
            waiters.is_empty()
        } else {
            false
        };
        if remove_key {
            inner.waiters.remove(&key);
        }
    }

    #[cfg(test)]
    fn waiter_count(&self) -> usize {
        self.inner
            .lock()
            .expect("session activation registry lock poisoned")
            .waiters
            .values()
            .map(|waiters| waiters.replay.len() + waiters.twofa.len())
            .sum()
    }
}

impl SessionActivationWaiter {
    /// SYSCOIN: An accepted-session edge can be dropped by Reth's bounded event broadcast. Bound
    /// every exact waiter so its protocol wrapper eventually closes or becomes inert and releases
    /// admission state instead of waiting forever.
    pub(crate) async fn wait_for(mut self, timeout: std::time::Duration) -> bool {
        tokio::time::timeout(timeout, &mut self.receiver)
            .await
            .is_ok_and(|result| result.is_ok())
    }
}

impl Drop for SessionActivationWaiter {
    fn drop(&mut self) {
        self.registry.remove_waiter(self.key, self.token, self.lane);
    }
}

#[derive(Debug, Clone)]
pub struct HandlerSharedState {
    /// Protocol event sender.
    events_sender: mpsc::UnboundedSender<ProtocolEvent>,
    /// The maximum number of active connections.
    max_active_connections: usize,
    active_connections_semaphore: Arc<Semaphore>,
    /// Peers that bypass the connection cap, so a pinned serving node is never locked out by other
    /// peers filling the pool.
    trusted_peers: Arc<HashSet<PeerId>>,
    // SYSCOIN: After exact accepted-session activation, claiming PeerId ownership provides a final
    // first-wins guard against inconsistent duplicate activation without tentative side effects.
    connection_owners: Arc<Mutex<HashMap<PeerId, u64>>>,
    // SYSCOIN: Exact monotonically increasing tokens prevent a stale connection drop from closing
    // or unregistering a later replay owner for the same PeerId.
    next_connection_token: Arc<AtomicU64>,
    // SYSCOIN: Protocol work begins only after reth identifies the exact accepted RLPx socket.
    session_activations: SessionActivationRegistry,
}

impl HandlerSharedState {
    /// Create new protocol state.
    pub fn new(
        events_sender: mpsc::UnboundedSender<ProtocolEvent>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
    ) -> Self {
        Self::new_with_session_activations(
            events_sender,
            max_active_connections,
            trusted_peers,
            SessionActivationRegistry::default(),
        )
    }

    /// SYSCOIN: Creates protocol state tied to the node-wide accepted-session activation bridge.
    pub fn new_with_session_activations(
        events_sender: mpsc::UnboundedSender<ProtocolEvent>,
        max_active_connections: usize,
        trusted_peers: HashSet<PeerId>,
        session_activations: SessionActivationRegistry,
    ) -> Self {
        Self {
            events_sender,
            max_active_connections,
            active_connections_semaphore: Arc::new(Semaphore::new(max_active_connections)),
            trusted_peers: Arc::new(trusted_peers),
            connection_owners: Arc::new(Mutex::new(HashMap::new())),
            next_connection_token: Arc::new(AtomicU64::new(1)),
            session_activations,
        }
    }

    /// Returns the current number of active connections.
    pub fn active_connections(&self) -> u64 {
        (self.max_active_connections - self.active_connections_semaphore.available_permits()) as u64
    }

    /// Whether `peer_id` is a trusted peer that bypasses the connection cap.
    pub(crate) fn is_trusted(&self, peer_id: &PeerId) -> bool {
        self.trusted_peers.contains(peer_id)
    }

    pub(crate) fn try_acquire_connection_slot(
        &self,
    ) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        self.active_connections_semaphore
            .clone()
            .try_acquire_owned()
    }

    pub(crate) fn events_sender(&self) -> mpsc::UnboundedSender<ProtocolEvent> {
        self.events_sender.clone()
    }

    pub(crate) fn emit_max_active_connections_exceeded(&self) {
        let _ = self
            .events_sender
            .send(ProtocolEvent::MaxActiveConnectionsExceeded {
                max_connections: self.max_active_connections,
            });
    }

    pub(crate) fn max_active_connections(&self) -> usize {
        self.max_active_connections
    }

    /// SYSCOIN: Subscribe before returning the tentative protocol wrapper, so the accepted-session
    /// event cannot race ahead of registration and be lost.
    pub(crate) fn session_activation(
        &self,
        peer_id: PeerId,
        remote_addr: SocketAddr,
    ) -> SessionActivationWaiter {
        self.session_activations
            .subscribe(peer_id, remote_addr, SessionActivationLane::Replay)
    }

    /// SYSCOIN: Optional verifier work is separately gated behind replay's role-specific proof.
    pub(crate) fn twofa_session_activation(
        &self,
        peer_id: PeerId,
        remote_addr: SocketAddr,
    ) -> SessionActivationWaiter {
        self.session_activations
            .subscribe(peer_id, remote_addr, SessionActivationLane::Twofa)
    }

    /// SYSCOIN: Release the exact verifier waiter only at replay's role-specific proof point.
    pub(crate) fn activate_twofa_session(&self, peer_id: PeerId, remote_addr: SocketAddr) {
        self.session_activations
            .activate_twofa(peer_id, remote_addr);
    }

    /// SYSCOIN: Claims the first live replay handler for `peer_id`. Tentative duplicate RLPx
    /// sessions receive no token and therefore cannot emit connection lifecycle events.
    pub(crate) fn try_claim_connection(&self, peer_id: PeerId) -> Option<u64> {
        let mut owners = self
            .connection_owners
            .lock()
            .expect("zks connection owner lock poisoned");
        if owners.contains_key(&peer_id) {
            return None;
        }
        let token = self
            .next_connection_token
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
                token.checked_add(1)
            })
            .expect("zks connection token exhausted");
        owners.insert(peer_id, token);
        Some(token)
    }

    /// SYSCOIN: Finishes ownership only for the exact connection token. A mutually established
    /// owner enqueues `Closed`; a crossed/tentative socket releases silently. The owner mutex stays
    /// held through any nonblocking send, so reconnect `Established` can never precede old `Closed`.
    pub(crate) fn finish_connection_if_owner(
        &self,
        peer_id: PeerId,
        token: u64,
        established: bool,
    ) -> bool {
        let mut owners = self
            .connection_owners
            .lock()
            .expect("zks connection owner lock poisoned");
        if owners.get(&peer_id).copied() != Some(token) {
            return false;
        }
        if established {
            let _ = self.events_sender.send(ProtocolEvent::Closed { peer_id });
        }
        owners.remove(&peer_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // SYSCOIN: Duplicate handler setup is pre-acceptance in reth, so only the first exact token may
    // own lifecycle state. Its `Closed` must precede a reconnect's `Established`, and stale cleanup
    // must remain harmless after that genuine reconnect.
    #[test]
    fn connection_claim_is_first_wins_and_cleanup_is_exact_owner() {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let state = HandlerSharedState::new(events_tx, 1, HashSet::new());
        let peer_id = PeerId::repeat_byte(0xA1);

        let first = state.try_claim_connection(peer_id).unwrap();
        assert!(state.try_claim_connection(peer_id).is_none());
        assert!(!state.finish_connection_if_owner(peer_id, first + 1, true));
        assert!(state.try_claim_connection(peer_id).is_none());
        assert!(state.finish_connection_if_owner(peer_id, first, true));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::Closed { peer_id: closed }) if closed == peer_id
        ));

        let replacement = state.try_claim_connection(peer_id).unwrap();
        assert!(replacement > first);
        assert!(!state.finish_connection_if_owner(peer_id, first, true));
        assert!(events_rx.try_recv().is_err());
        assert!(state.finish_connection_if_owner(peer_id, replacement, true));

        let reconnect = state.try_claim_connection(peer_id).unwrap();
        let remote_addr: SocketAddr = "127.0.0.1:30305".parse().unwrap();
        state
            .events_sender()
            .send(ProtocolEvent::Established {
                direction: reth_network::Direction::Incoming,
                peer_id,
                remote_addr,
            })
            .unwrap();
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::Closed { peer_id: closed }) if closed == peer_id
        ));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::Established {
                peer_id: established,
                remote_addr: established_addr,
                ..
            }) if established == peer_id && established_addr == remote_addr
        ));
        assert!(state.finish_connection_if_owner(peer_id, reconnect, true));
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ProtocolEvent::Closed { peer_id: closed }) if closed == peer_id
        ));

        let crossed = state.try_claim_connection(peer_id).unwrap();
        assert!(state.finish_connection_if_owner(peer_id, crossed, false));
        assert!(events_rx.try_recv().is_err());
    }

    // SYSCOIN: Only the physical socket named by reth's accepted-session event may start work;
    // verifier activation remains ordered behind replay, and dropping a rejected tentative waiter
    // must remove its exact registry entry.
    #[tokio::test]
    async fn session_activation_is_exact_ordered_and_cancel_safe() {
        let registry = SessionActivationRegistry::default();
        let peer_id = PeerId::repeat_byte(0xA2);
        let accepted_addr: SocketAddr = "127.0.0.1:30303".parse().unwrap();
        let rejected_addr: SocketAddr = "127.0.0.1:30304".parse().unwrap();
        let accepted_replay =
            registry.subscribe(peer_id, accepted_addr, SessionActivationLane::Replay);
        let accepted_twofa =
            registry.subscribe(peer_id, accepted_addr, SessionActivationLane::Twofa);
        let rejected = registry.subscribe(peer_id, rejected_addr, SessionActivationLane::Replay);
        assert_eq!(registry.waiter_count(), 3);

        registry.activate(peer_id, accepted_addr);
        assert!(accepted_replay.wait_for(Duration::from_secs(1)).await);
        let mut accepted_twofa = Box::pin(accepted_twofa.wait_for(Duration::from_secs(1)));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut accepted_twofa)
                .await
                .is_err()
        );
        assert_eq!(registry.waiter_count(), 2);
        registry.activate_twofa(peer_id, accepted_addr);
        assert!(accepted_twofa.await);
        assert_eq!(registry.waiter_count(), 1);

        drop(rejected);
        assert_eq!(registry.waiter_count(), 0);

        // Activation is deliberately edge-triggered: a reconnect must subscribe for its own
        // accepted event rather than inheriting stale acceptance for the same address.
        registry.activate(peer_id, rejected_addr);
        let reconnect = registry.subscribe(peer_id, rejected_addr, SessionActivationLane::Replay);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                reconnect.wait_for(Duration::from_secs(1))
            )
            .await
            .is_err()
        );
        assert_eq!(registry.waiter_count(), 0);
    }

    // SYSCOIN: A missed lossy Reth activation edge must remove only its exact waiter after the
    // bounded watchdog; otherwise churn can retain mandatory replay and optional 2FA permits.
    #[tokio::test(start_paused = true)]
    async fn session_activation_watchdog_releases_missed_waiter() {
        let registry = SessionActivationRegistry::default();
        let waiter = registry.subscribe(
            PeerId::repeat_byte(0xA3),
            "127.0.0.1:30306".parse().unwrap(),
            SessionActivationLane::Replay,
        );
        assert_eq!(registry.waiter_count(), 1);

        let task = tokio::spawn(waiter.wait_for(Duration::from_secs(10)));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;

        assert!(!task.await.unwrap());
        assert_eq!(registry.waiter_count(), 0);
    }
}
