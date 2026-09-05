//! SYSCOIN: Exercise the actual confirmation wait without scheduling sleeps or live peers.
use super::*;
use openraft::{CommittedLeaderId, LogId};

fn leader_metrics(applied: u64) -> RaftMetrics<PeerId, RaftNode> {
    let mut metrics = RaftMetrics::new_initial(PeerId::ZERO);
    metrics.state = ServerState::Leader;
    metrics.current_leader = Some(PeerId::ZERO);
    metrics.current_term = 7;
    metrics.vote = Vote::new_committed(7, PeerId::ZERO);
    metrics.last_log_index = Some(6);
    metrics.last_applied = Some(LogId::new(CommittedLeaderId::new(7, PeerId::ZERO), applied));
    metrics
}

#[tokio::test]
async fn syscoin_restarted_leader_waits_beyond_reused_noop() {
    // OpenRaft 0.9.24 reuses its first current-vote entry as noop on restart. Its
    // successful read barrier can therefore be index 4 while retained logs 5..=6
    // have not yet applied. Neither that noop nor partial tail application suffices.
    let metrics = leader_metrics(4);
    let frontier = LeaderReplayFrontier {
        vote: metrics.vote,
        last_log_index: metrics.last_log_index,
    };
    let (tx, rx) = watch::channel(metrics);
    let wait = wait_for_replay_frontier(rx, &frontier);
    tokio::pin!(wait);
    assert!(futures::poll!(&mut wait).is_pending());
    tx.send(leader_metrics(5)).unwrap();
    assert!(futures::poll!(&mut wait).is_pending());
    tx.send(leader_metrics(6)).unwrap();
    assert_eq!(wait.await, Ok(()));
}

#[tokio::test]
async fn syscoin_changed_vote_invalidates_wait_even_when_role_stays_leader() {
    let metrics = leader_metrics(4);
    let frontier = LeaderReplayFrontier {
        vote: metrics.vote,
        last_log_index: metrics.last_log_index,
    };
    let (tx, rx) = watch::channel(metrics);
    let wait = wait_for_replay_frontier(rx, &frontier);
    tokio::pin!(wait);
    assert!(futures::poll!(&mut wait).is_pending());
    let mut changed = leader_metrics(6);
    changed.current_term = 8;
    changed.vote = Vote::new_committed(8, PeerId::ZERO);
    tx.send(changed).unwrap();
    assert_eq!(wait.await, Err(ProbeFailure::ForwardToLeader));
}

#[tokio::test]
async fn syscoin_demoted_or_closed_frontier_never_confirms() {
    let metrics = leader_metrics(4);
    let frontier = LeaderReplayFrontier {
        vote: metrics.vote,
        last_log_index: metrics.last_log_index,
    };
    let (tx, rx) = watch::channel(metrics);
    let wait = wait_for_replay_frontier(rx, &frontier);
    tokio::pin!(wait);
    assert!(futures::poll!(&mut wait).is_pending());
    let mut demoted = leader_metrics(6);
    demoted.state = ServerState::Follower;
    tx.send(demoted).unwrap();
    assert_eq!(wait.await, Err(ProbeFailure::ForwardToLeader));

    let (tx, rx) = watch::channel(leader_metrics(4));
    drop(tx);
    assert_eq!(
        wait_for_replay_frontier(rx, &frontier).await,
        Err(ProbeFailure::Fatal)
    );
}

#[tokio::test]
async fn syscoin_unflushed_new_term_invalidates_old_durable_vote() {
    let metrics = leader_metrics(4);
    let frontier = LeaderReplayFrontier {
        vote: metrics.vote,
        last_log_index: metrics.last_log_index,
    };
    let (tx, rx) = watch::channel(metrics);
    let wait = wait_for_replay_frontier(rx, &frontier);
    tokio::pin!(wait);
    assert!(futures::poll!(&mut wait).is_pending());
    let mut changed = leader_metrics(6);
    // Accepted term is newer while the durable vote still reports term 7.
    changed.current_term = 8;
    tx.send(changed).unwrap();
    assert_eq!(wait.await, Err(ProbeFailure::ForwardToLeader));
}

#[test]
fn syscoin_leadership_signal_exposes_published_replay_frontier() {
    let (tx, rx) = watch::channel(ConfirmedLeadership {
        role: ConsensusRole::Leader,
        replay_watermark: 3,
    });
    let signal = crate::LeadershipSignal::Watch(rx);
    assert_eq!(signal.current_role(), ConsensusRole::Leader);
    assert_eq!(signal.required_replay_watermark(), 3);
    drop(tx);
    assert_eq!(
        crate::LeadershipSignal::AlwaysLeader.required_replay_watermark(),
        0
    );
}
