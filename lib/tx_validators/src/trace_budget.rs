//! SYSCOIN: Local capture capacities, not transaction-validity or consensus limits.
//! Reservations are cumulative: freeing a frame does not replenish the budget.

/// SYSCOIN: Maximum retained / allocated payload bytes and frames in one trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceLimits {
    pub max_bytes: usize,
    pub max_frames: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("trace capture capacity exceeded")]
pub struct TraceBudgetError;

#[derive(Debug)]
pub struct TraceBudget {
    limits: TraceLimits,
    bytes: usize,
    frames: usize,
    exceeded: bool,
}

impl TraceBudget {
    pub fn new(limits: TraceLimits) -> Self {
        Self {
            limits,
            bytes: 0,
            frames: 0,
            // SYSCOIN: Zero is fail-closed, never an unbounded opt-out.
            exceeded: limits.max_bytes == 0 || limits.max_frames == 0,
        }
    }

    /// SYSCOIN: Reserve before copying or allocating the corresponding capture data.
    /// Overflow / exhaustion latches an error for the lifetime of the trace.
    pub fn reserve(&mut self, bytes: usize, frames: usize) -> Result<(), TraceBudgetError> {
        self.ensure_within_limit()?;
        match (
            self.bytes.checked_add(bytes),
            self.frames.checked_add(frames),
        ) {
            (Some(bytes), Some(frames))
                if bytes <= self.limits.max_bytes && frames <= self.limits.max_frames =>
            {
                self.bytes = bytes;
                self.frames = frames;
                Ok(())
            }
            _ => {
                self.exceeded = true;
                Err(TraceBudgetError)
            }
        }
    }

    pub fn ensure_within_limit(&self) -> Result<(), TraceBudgetError> {
        if self.exceeded {
            Err(TraceBudgetError)
        } else {
            Ok(())
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.bytes
    }
    pub fn used_frames(&self) -> usize {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    // SYSCOIN: Bounded regression controls for cumulative, latched capture capacity.
    use super::*;

    #[test]
    fn cumulative_capacity_and_failure_are_latched() {
        let mut budget = TraceBudget::new(TraceLimits {
            max_bytes: 8,
            max_frames: 2,
        });
        assert!(budget.reserve(4, 1).is_ok());
        assert!(budget.reserve(4, 1).is_ok());
        assert!(budget.reserve(1, 0).is_err());
        assert!(budget.reserve(0, 0).is_err());
        assert_eq!(budget.used_bytes(), 8);
        assert_eq!(budget.used_frames(), 2);
    }

    #[test]
    fn frame_capacity_zero_and_overflow_fail_closed() {
        let mut budget = TraceBudget::new(TraceLimits {
            max_bytes: usize::MAX,
            max_frames: 1,
        });
        assert!(budget.reserve(usize::MAX, 1).is_ok());
        assert!(budget.reserve(1, 0).is_err());
        let mut budget = TraceBudget::new(TraceLimits {
            max_bytes: 8,
            max_frames: 1,
        });
        assert!(budget.reserve(0, 1).is_ok());
        assert!(budget.reserve(0, 1).is_err());
        assert!(
            TraceBudget::new(TraceLimits {
                max_bytes: 0,
                max_frames: 1
            })
            .reserve(0, 0)
            .is_err()
        );
        assert!(
            TraceBudget::new(TraceLimits {
                max_bytes: 1,
                max_frames: 0
            })
            .reserve(0, 0)
            .is_err()
        );
    }
}
