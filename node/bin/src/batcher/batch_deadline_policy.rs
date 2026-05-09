use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

// SYSCOIN: Preserve the timestamp-based deadline for benign clock skew while bounding malformed
// future replay/canonization timestamps.
const MAX_FUTURE_TIMESTAMP_SKEW: Duration = Duration::from_secs(5 * 60);

/// Converts a block's L2 unix timestamp into an absolute `tokio::time::Instant` at which
/// the batch containing that block should be sealed.
///
/// The deadline is `first_block_timestamp + batch_timeout`, expressed as an absolute wall-clock
/// instant. This makes the deadline restart-resilient: it is derived from the block timestamp
/// (which is deterministic and part of the chain state), not from `std::time::Instant::now()`
/// at the moment the batch was opened.
pub fn deadline_from_block_timestamp(
    block_timestamp: u64,
    batch_timeout: Duration,
) -> (Instant, u64) {
    let deadline_unix = block_timestamp.saturating_add(batch_timeout.as_secs());

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs();

    if deadline_unix <= now_unix {
        // Deadline already passed (e.g. replaying old blocks after a restart).
        // Seal as soon as possible — once catch-up replay is complete.
        (Instant::now(), now_unix)
    } else {
        let delay = Duration::from_secs(deadline_unix - now_unix);
        // SYSCOIN: Replay/canonized timestamps are not a public transaction input in the current
        // fork, but they can still come from WAL/rebuild/consensus state. Preserve the documented
        // `first_block_timestamp + batch_timeout` deadline within a small clock-skew window, but
        // cap malformed future timestamps so they cannot stall sealing or overflow `Instant`
        // arithmetic.
        let max_delay = batch_timeout.saturating_add(MAX_FUTURE_TIMESTAMP_SKEW);
        let delay = delay.min(max_delay);
        let now_instant = Instant::now();
        let Some(instant) = now_instant.checked_add(delay) else {
            return (now_instant, now_unix);
        };
        let unix_deadline = now_unix.saturating_add(delay.as_secs());

        (instant, unix_deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX epoch")
            .as_secs()
    }

    #[test]
    fn past_deadline_seals_immediately() {
        let now = Instant::now();
        let (deadline, unix_deadline) = deadline_from_block_timestamp(1, Duration::from_secs(300));

        assert!(deadline <= now + Duration::from_secs(1));
        assert!(unix_deadline >= now_unix().saturating_sub(1));
    }

    #[test]
    fn current_timestamp_uses_configured_timeout() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let (deadline, unix_deadline) = deadline_from_block_timestamp(now_unix(), timeout);

        assert!(deadline >= now);
        assert!(deadline <= now + timeout + Duration::from_secs(1));
        assert!(unix_deadline <= now_unix().saturating_add(timeout.as_secs()));
    }

    #[test]
    fn small_future_timestamp_preserves_timestamp_based_slack() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let future_timestamp = now_unix().saturating_add(60);
        let (deadline, unix_deadline) = deadline_from_block_timestamp(future_timestamp, timeout);

        assert!(deadline >= now + timeout);
        assert!(deadline <= now + timeout + Duration::from_secs(61));
        assert!(unix_deadline <= now_unix().saturating_add(timeout.as_secs() + 60));
    }

    #[test]
    fn far_future_timestamp_is_capped_to_reasonable_delay() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let future_timestamp = now_unix().saturating_add(10 * 365 * 24 * 60 * 60);
        let (deadline, unix_deadline) = deadline_from_block_timestamp(future_timestamp, timeout);

        let max_delay = timeout + MAX_FUTURE_TIMESTAMP_SKEW;
        assert!(deadline >= now);
        assert!(deadline <= now + max_delay + Duration::from_secs(1));
        assert!(unix_deadline <= now_unix().saturating_add(max_delay.as_secs()));
    }

    #[test]
    fn extreme_future_timestamp_does_not_panic() {
        let now = Instant::now();
        let timeout = Duration::from_secs(300);
        let (deadline, unix_deadline) = deadline_from_block_timestamp(u64::MAX, timeout);

        let max_delay = timeout + MAX_FUTURE_TIMESTAMP_SKEW;
        assert!(deadline >= now);
        assert!(deadline <= now + max_delay + Duration::from_secs(1));
        assert!(unix_deadline <= now_unix().saturating_add(max_delay.as_secs()));
    }
}
