use crate::model::{ConfirmedLeadership, ConsensusRole};
use crate::status::RaftConsensusStatus;
use openraft::error::{CheckIsLeaderError, RaftError};
use openraft::{Raft, RaftMetrics, ServerState, Vote};
use reth_network_peers::PeerId;
use reth_tasks::Runtime;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval, timeout};
use zksync_os_consensus_types::{RaftNode, RaftTypeConfig};

/// How often we re-probe `ensure_linearizable` while holding the Leader state but waiting
/// for confirmation. We still wake on every OpenRaft metrics change (to keep the status
/// watch and the role-loss reaction responsive), but the probe call itself is rate-limited
/// to one per `PROBE_INTERVAL` — without this, a metrics-churn storm against unreachable
/// voters produces one openraft-side ERROR (`timeout while confirming leadership for read
/// request`) per unreachable voter per metrics tick. The `claims_leader: false → true` edge
/// bypasses the rate limit so a fresh election confirms without paying the full interval.
const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// Per-probe budget for quorum confirmation and the SYSCOIN retained-tail apply barrier.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// While confirmation keeps failing with the same cause, re-emit the log at most this often
/// so a stuck cluster keeps a "still degraded" reminder in the log without flooding it.
const STUCK_REMINDER_INTERVAL: Duration = Duration::from_secs(30);

type LinearizableErr = RaftError<PeerId, CheckIsLeaderError<PeerId, RaftNode>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeFailure {
    /// This node has been deposed; another node is now leader. Routine after a clean
    /// failover or on a rejoining stale leader — informational, not an alarm.
    ForwardToLeader,
    /// Could not collect a quorum ack within the probe timeout. The cluster cannot make
    /// progress; this is the operational alarm condition.
    QuorumNotEnough,
    /// Probe call did not return within `PROBE_TIMEOUT`. Usually indicates the same problem
    /// as `QuorumNotEnough` but caught by our local timer.
    Timeout,
    /// `Raft` task is no longer running. Typically a fatal startup or shutdown condition.
    Fatal,
}

struct FailureStreak {
    kind: ProbeFailure,
    started_at: Instant,
    last_logged_at: Instant,
}

// SYSCOIN: The retained tail matters on same-leader restart: OpenRaft may reuse an old
// noop log for ensure_linearizable(), with a later pre-crash tail still uncommitted.
#[derive(Debug, Clone)]
struct LeaderReplayFrontier {
    vote: Vote<PeerId>,
    last_log_index: Option<u64>,
}

impl LeaderReplayFrontier {
    fn same_leader(&self, metrics: &RaftMetrics<PeerId, RaftNode>) -> bool {
        metrics.running_state.is_ok()
            && metrics.state == ServerState::Leader
            && metrics.current_leader == Some(metrics.id)
            && metrics.vote == self.vote
            // SYSCOIN: metrics.vote is the durable vote; current_term can advance before
            // that vote flushes. Do not retain an older confirmation in that interval.
            && metrics.current_term == self.vote.leader_id.get_term()
    }
}

struct ConfirmedReplayLeader {
    frontier: LeaderReplayFrontier,
    replay_watermark: u64,
}

// SYSCOIN: Watch the actual apply frontier, not queue length or the older read-log/noop.
// A changed vote invalidates the wait even if a watch skips intermediate role changes.
async fn wait_for_replay_frontier(
    mut metrics_rx: watch::Receiver<RaftMetrics<PeerId, RaftNode>>,
    frontier: &LeaderReplayFrontier,
) -> Result<(), ProbeFailure> {
    loop {
        let metrics = metrics_rx.borrow_and_update().clone();
        if !frontier.same_leader(&metrics) {
            return Err(ProbeFailure::ForwardToLeader);
        }
        if metrics.last_applied.map(|id| id.index) >= frontier.last_log_index {
            return Ok(());
        }
        metrics_rx
            .changed()
            .await
            .map_err(|_| ProbeFailure::Fatal)?;
    }
}

// SYSCOIN: No local producer is enabled before this completes. Thus under the confirmed
// vote, the captured tail includes every Normal entry this process could inherit. A newer
// leader invalidates confirmation; after promotion, any leadership loss tears down runtime.
async fn confirm_replay_leader(
    raft: &Raft<RaftTypeConfig>,
    expected_vote: Vote<PeerId>,
    forwarded_records: &AtomicU64,
) -> Result<ConfirmedReplayLeader, (ProbeFailure, Option<LinearizableErr>)> {
    raft.ensure_linearizable()
        .await
        .map_err(|error| (classify(&error), Some(error)))?;
    // SYSCOIN: OpenRaft publishes the restored tail before handling API requests; on a
    // fresh election its read barrier covers the appended noop. Refresh after the probe
    // so same-leader restarts additionally wait for the entire retained tail.
    let metrics = raft.metrics().borrow().clone();
    let vote = metrics.vote;
    let frontier = LeaderReplayFrontier {
        vote,
        last_log_index: metrics.last_log_index,
    };
    if vote != expected_vote || !frontier.same_leader(&metrics) {
        return Err((ProbeFailure::ForwardToLeader, None));
    }
    wait_for_replay_frontier(raft.metrics(), &frontier)
        .await
        .map_err(|failure| (failure, None))?;
    // SYSCOIN: Recheck via RaftCore after the asynchronous apply wait, not stale metrics.
    let still_leader = raft
        .with_raft_state(move |state| {
            state.server_state == ServerState::Leader && *state.vote_ref() == vote
        })
        .await
        .map_err(|_| (ProbeFailure::Fatal, None))?;
    if !still_leader {
        return Err((ProbeFailure::ForwardToLeader, None));
    }
    Ok(ConfirmedReplayLeader {
        frontier,
        replay_watermark: forwarded_records.load(Ordering::Acquire),
    })
}

/// Spawns a background task that translates OpenRaft metrics into two node-facing signals:
/// a confirmed role and replay watermark used by the sequencer, and a richer
/// `RaftConsensusStatus` watch channel exposed by the status server.
///
/// OpenRaft may briefly report `Leader` while a node is still replaying committed entries after
/// an election. To avoid producing blocks too early, this monitor only upgrades the node to
/// `ConsensusRole::Leader` after quorum confirmation and applying the retained log tail.
/// SYSCOIN: The source must consume the accompanying replay watermark before proposals;
/// OpenRaft application only enqueues records into an asynchronous canonizer bridge.
/// If the node steps down or the confirmation probe fails, the role falls back to `Replica`.
///
/// The task exits automatically when the OpenRaft metrics channel closes or when all receivers
/// for both output watch channels are dropped.
pub fn spawn_leadership_monitor(
    runtime: &Runtime,
    raft: Raft<RaftTypeConfig>,
    node_id_str: String,
    forwarded_records: Arc<AtomicU64>,
    leader_tx: watch::Sender<ConfirmedLeadership>,
    status_tx: watch::Sender<Option<RaftConsensusStatus>>,
) {
    let mut metrics_rx = raft.metrics();
    runtime.spawn_critical_task("raft leadership monitor", async move {
        let mut last_metrics_key = None;
        let mut confirmed_leader: Option<ConfirmedReplayLeader> = None;
        let mut prev_role = ConsensusRole::Replica;
        let mut streak: Option<FailureStreak> = None;
        let mut last_probe_at: Option<Instant> = None;
        let mut probe_timer = interval(PROBE_INTERVAL);
        probe_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            // Wake on either an OpenRaft metrics change or the periodic probe tick. Both
            // are needed: metrics changes drive the status watch and the role-loss
            // reaction; the probe tick ensures we periodically re-attempt confirmation
            // even if metrics go quiet. The `ensure_linearizable` call below is separately
            // rate-limited by `last_probe_at` — see `PROBE_INTERVAL`.
            tokio::select! {
                biased;
                changed = metrics_rx.changed() => {
                    if changed.is_err() {
                        // OpenRaft has dropped its metrics sender — the engine is gone, which
                        // happens on graceful shutdown after `raft.shutdown()`.
                        tracing::info!("OpenRaft metrics channel closed; leadership monitor exiting");
                        break;
                    }
                }
                _ = probe_timer.tick() => {}
            }

            let mut metrics = metrics_rx.borrow().clone();
            let metrics_key = (metrics.state, metrics.current_term, metrics.current_leader);
            if last_metrics_key.as_ref() != Some(&metrics_key) {
                tracing::debug!(
                    "OpenRaft metrics changed: state={:?}, term={}, leader={:?}",
                    metrics.state,
                    metrics.current_term,
                    metrics.current_leader
                );
                last_metrics_key = Some(metrics_key);
            }

            let claims_leader = matches!(metrics.state, ServerState::Leader);
            // SYSCOIN: A watch may coalesce Leader -> Replica -> Leader. The confirmed
            // vote is process-scoped, so a different vote must still tear down a producer.
            if confirmed_leader
                .as_ref()
                .is_some_and(|leader| !leader.frontier.same_leader(&metrics))
            {
                if prev_role == ConsensusRole::Leader {
                    panic!("raft leadership lost or vote changed; tearing down node");
                }
                confirmed_leader = None;
                last_probe_at = None;
            }
            if !claims_leader {
                // Once we stop claiming leader, any in-progress streak is moot; the role
                // change itself is logged below. Clearing `last_probe_at` ensures the next
                // false→true edge probes immediately rather than waiting out an interval.
                streak = None;
                confirmed_leader = None;
                last_probe_at = None;
            } else if confirmed_leader.is_none()
                && last_probe_at.is_none_or(|t| t.elapsed() >= PROBE_INTERVAL)
            {
                last_probe_at = Some(Instant::now());
                let confirmation = timeout(
                    PROBE_TIMEOUT,
                    confirm_replay_leader(&raft, metrics.vote, &forwarded_records),
                )
                .await;
                // SYSCOIN: Never publish a role from the pre-await metrics snapshot.
                metrics = metrics_rx.borrow().clone();
                match confirmation {
                    Ok(Ok(leader)) if leader.frontier.same_leader(&metrics) => {
                        if let Some(s) = streak.take() {
                            tracing::info!(
                                "raft leader confirmed (recovered from {:?} after {:?})",
                                s.kind,
                                s.started_at.elapsed()
                            );
                        } else {
                            tracing::info!("raft leader confirmed");
                        }
                        confirmed_leader = Some(leader);
                    }
                    Ok(Ok(_)) => {
                        note_failure(&mut streak, ProbeFailure::ForwardToLeader, None);
                    }
                    Ok(Err((failure, error))) => {
                        note_failure(&mut streak, failure, error.as_ref());
                    }
                    Err(_) => {
                        note_failure(&mut streak, ProbeFailure::Timeout, None);
                    }
                }
            }

            let role = if confirmed_leader
                .as_ref()
                .is_some_and(|leader| leader.frontier.same_leader(&metrics))
            {
                ConsensusRole::Leader
            } else {
                ConsensusRole::Replica
            };
            if role != prev_role {
                tracing::info!("OpenRaft leadership status changed: {role:?}");
                let was_leader = prev_role == ConsensusRole::Leader;
                prev_role = role;
                // Losing leadership mid-flight leaves the produce pipeline in an unrecoverable
                // state (e.g. a `Produce` parked in `BlockExecutor` waiting on an empty
                // mempool). Tear the runtime down so the orchestrator restarts the node and
                // it rejoins as a follower with fresh raft state.
                if was_leader && role != ConsensusRole::Leader {
                    panic!("raft leadership lost; tearing down node");
                }
            }

            let status = RaftConsensusStatus {
                node_id: node_id_str.clone(),
                state: format!("{:?}", metrics.state),
                is_leader: role == ConsensusRole::Leader,
                current_leader: metrics.current_leader.map(|id| id.to_string()),
                current_term: metrics.current_term,
                last_applied_index: metrics.last_applied.map(|id| id.index),
            };
            // status_tx may have no receivers if the status server is disabled; that's fine.
            let _ = status_tx.send(Some(status));
            if leader_tx
                .send(ConfirmedLeadership {
                    role,
                    replay_watermark: confirmed_leader
                        .as_ref()
                        .map_or(0, |leader| leader.replay_watermark),
                })
                .is_err()
            {
                break;
            }
        }
    });
}

fn classify(err: &LinearizableErr) -> ProbeFailure {
    match err {
        RaftError::APIError(CheckIsLeaderError::ForwardToLeader(_)) => {
            ProbeFailure::ForwardToLeader
        }
        RaftError::APIError(CheckIsLeaderError::QuorumNotEnough(_)) => {
            ProbeFailure::QuorumNotEnough
        }
        RaftError::Fatal(_) => ProbeFailure::Fatal,
    }
}

fn note_failure(
    streak: &mut Option<FailureStreak>,
    kind: ProbeFailure,
    err: Option<&LinearizableErr>,
) {
    let now = Instant::now();
    match streak {
        Some(s) if s.kind == kind => {
            // Same failure as last tick: stay quiet unless the reminder window has elapsed.
            if now.duration_since(s.last_logged_at) >= STUCK_REMINDER_INTERVAL {
                emit_failure(kind, err, Some(now.duration_since(s.started_at)));
                s.last_logged_at = now;
            }
        }
        _ => {
            emit_failure(kind, err, None);
            *streak = Some(FailureStreak {
                kind,
                started_at: now,
                last_logged_at: now,
            });
        }
    }
}

fn emit_failure(kind: ProbeFailure, err: Option<&LinearizableErr>, elapsed: Option<Duration>) {
    let stuck = elapsed
        .map(|e| format!(" (still failing after {e:?})"))
        .unwrap_or_default();
    match kind {
        ProbeFailure::ForwardToLeader => {
            // Expected after a failover or for a stale leader catching up — surface as
            // INFO rather than WARN so it doesn't read like an alarm.
            let leader = err
                .and_then(|e| match e {
                    RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f)) => f.leader_id,
                    _ => None,
                })
                .map(|id| format!("{id}"))
                .unwrap_or_else(|| "(unknown)".to_string());
            tracing::info!("raft node deposed: cluster leader is now {leader}{stuck}");
        }
        ProbeFailure::QuorumNotEnough => {
            // The operational alarm: this node holds the leader role but cannot reach a
            // quorum to commit, so the cluster cannot make progress. The `cluster` field
            // openraft attaches is a pre-formatted Debug dump of the full membership and
            // is too noisy to log; the acked set alone is enough to tell who replied.
            let acked = err
                .and_then(|e| match e {
                    RaftError::APIError(CheckIsLeaderError::QuorumNotEnough(q)) => Some(format!(
                        ", acked by {} of cluster: {:?}",
                        q.got.len(),
                        q.got
                    )),
                    _ => None,
                })
                .unwrap_or_default();
            tracing::warn!("raft cannot reach quorum{acked}{stuck}");
        }
        ProbeFailure::Timeout => {
            tracing::warn!(
                "raft leadership/replay confirmation timed out after {PROBE_TIMEOUT:?}{stuck}"
            );
        }
        ProbeFailure::Fatal => {
            tracing::error!("raft is in a fatal state{stuck}");
        }
    }
}

// SYSCOIN: Deterministic retained-tail and coalesced-election regression coverage.
#[cfg(test)]
mod syscoin_replay_barrier_tests;
