//!
//! Component-state reporter: centralized accounting of time spent by components in states.
//! - Components create a lightweight handle via `ComponentStateReporter::global().handle_for(...)`.
//! - The handle sends EnterState events over an async mpsc channel.
//! - A background task periodically (every TICK_SECS) increments a single metric family
//!   `component_time_spent_in_state[component, GenericComponentState, specific_state]` with
//!   time spent in the current state. Transitions are also finalized immediately on EnterState.
//!
//! ### Backpressure detection
//! Components that call `handle_for_with_backpressure` supply a `backpressure_after` threshold.
//! When the component stays in `WaitingSend` longer than this threshold the reporter calls
//! [`zksync_os_types::BackpressureHandle::set_overloaded`].  The signal is cleared the moment
//! the component leaves `WaitingSend`.
//!
use crate::generic_component_state::GenericComponentState;
use crate::metrics::GENERAL_METRICS;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, Receiver, Sender};
use zksync_os_types::BackpressureHandle;

/// How often to report time spent in the current state for each component.
/// Must be lower than reporting window in Prometheus (usually 15 or 30 seconds)
const TICK_SECS: u64 = 2;

/// Default retry-after hint sent to RPC clients when backpressure fires.
const DEFAULT_RETRY_AFTER_MS: u64 = 5_000;

/// Individual component state.
/// Usually an `enum` - each state is reported as:
/// * `GenericComponentState`
/// * `specific` string
pub trait StateLabel: Send + Sync + 'static {
    fn generic(&self) -> GenericComponentState;
    fn specific(&self) -> &'static str;
}

/// Individual messages that are sent to `ComponentStateHandle`
struct ReporterMsg {
    component: &'static str,
    new_label: Box<dyn StateLabel>,
    /// Set only on the first (registration) message.
    backpressure_after: Option<Duration>,
}

/// `ComponentStateHandle` stores this per component
/// to track the current state and time spent in it.
struct RegistryEntry {
    component: &'static str,
    current_state: Box<dyn StateLabel>,
    last_report_at: Instant,
    /// When did this component enter `WaitingSend` (reset on every leave).
    entered_waiting_send_at: Option<Instant>,
    /// Threshold after which we call `BackpressureHandle::set_overloaded`.
    backpressure_after: Option<Duration>,
    /// Whether we have already fired backpressure for the current `WaitingSend` stretch.
    backpressured: bool,
}

impl RegistryEntry {
    fn flush(&mut self) {
        let secs = self.last_report_at.elapsed().as_secs_f64();
        GENERAL_METRICS.component_time_spent_in_state[&(
            self.component,
            self.current_state.generic(),
            self.current_state.specific(),
        )]
            .inc_by(secs);
        self.last_report_at = Instant::now();
    }

    /// Check whether we should fire (or clear) backpressure based on current state.
    fn tick_backpressure(&mut self) {
        let Some(threshold) = self.backpressure_after else {
            return;
        };
        if self.current_state.generic() == GenericComponentState::WaitingSend
            && let Some(entered_at) = self.entered_waiting_send_at
            && !self.backpressured
            && entered_at.elapsed() >= threshold
        {
            BackpressureHandle::global().set_overloaded(self.component, DEFAULT_RETRY_AFTER_MS);
            self.backpressured = true;
        }
    }

    /// Called whenever the component transitions to a new state.
    fn on_transition(&mut self, new_label: Box<dyn StateLabel>) {
        let leaving_waiting_send =
            self.current_state.generic() == GenericComponentState::WaitingSend;
        let entering_waiting_send = new_label.generic() == GenericComponentState::WaitingSend;

        // Clear backpressure if we're leaving WaitingSend
        if leaving_waiting_send && !entering_waiting_send {
            self.entered_waiting_send_at = None;
            if self.backpressured {
                BackpressureHandle::global().clear_overloaded(self.component);
                self.backpressured = false;
            }
        }

        // Record when we enter WaitingSend
        if entering_waiting_send && !leaving_waiting_send {
            self.entered_waiting_send_at = Some(Instant::now());
        }

        self.current_state = new_label;
    }
}

#[derive(Clone)]
pub struct ComponentStateReporter {
    tx: Sender<ReporterMsg>,
}

impl ComponentStateReporter {
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<ComponentStateReporter> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    fn new() -> Self {
        let (tx, rx) = mpsc::channel(512);
        // Spawn background task
        tokio::spawn(run_reporter(rx));
        Self { tx }
    }

    pub fn handle_for<S>(
        &self,
        component: &'static str,
        initial_state: S,
    ) -> ComponentStateHandle<S>
    where
        S: StateLabel,
    {
        let _ = self.tx.try_send(ReporterMsg {
            component,
            new_label: Box::new(initial_state),
            backpressure_after: None,
        });

        ComponentStateHandle {
            component,
            tx: self.tx.clone(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Like `handle_for` but enables automatic backpressure detection.
    ///
    /// If the component stays in `WaitingSend` for longer than `backpressure_after`,
    /// [`BackpressureHandle::set_overloaded`] is called, causing the RPC layer to start
    /// rejecting new transactions with `-32003`.
    pub fn handle_for_with_backpressure<S>(
        &self,
        component: &'static str,
        initial_state: S,
        backpressure_after: Duration,
    ) -> ComponentStateHandle<S>
    where
        S: StateLabel,
    {
        let _ = self.tx.try_send(ReporterMsg {
            component,
            new_label: Box::new(initial_state),
            backpressure_after: Some(backpressure_after),
        });

        ComponentStateHandle {
            component,
            tx: self.tx.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComponentStateHandle<S> {
    component: &'static str,
    tx: Sender<ReporterMsg>,
    _marker: std::marker::PhantomData<S>,
}

impl<S: StateLabel> ComponentStateHandle<S> {
    pub fn enter_state(&self, new_state: S) {
        let _ = self.tx.try_send(ReporterMsg {
            component: self.component,
            new_label: Box::new(new_state),
            backpressure_after: None,
        });
    }
}

async fn run_reporter(mut rx: Receiver<ReporterMsg>) {
    let mut registry: HashMap<&'static str, RegistryEntry> = HashMap::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(TICK_SECS));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                for (_, entry) in registry.iter_mut() {
                    entry.flush();
                    entry.tick_backpressure();
                }
            },
            Some(ReporterMsg { component, new_label, backpressure_after }) = rx.recv() => {
                if let Some(entry) = registry.get_mut(&component) {
                    // finalize the previous period up until now
                    entry.flush();
                    entry.on_transition(new_label);
                    // Allow upgrading an existing entry's backpressure threshold on re-registration.
                    if backpressure_after.is_some() {
                        entry.backpressure_after = backpressure_after;
                    }
                } else {
                    let is_waiting_send =
                        new_label.generic() == GenericComponentState::WaitingSend;
                    registry.insert(component, RegistryEntry {
                        component,
                        current_state: new_label,
                        last_report_at: Instant::now(),
                        entered_waiting_send_at: is_waiting_send.then(Instant::now),
                        backpressure_after,
                        backpressured: false,
                    });
                }
            }
        }
    }
}
