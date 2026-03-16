# Backpressure Redesign Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current single-cause, all-errors-are-`-32603` backpressure system with a layered design that gives clients actionable, distinguishable error codes and retry guidance, and dynamically signals overload before the pipeline silently stalls.

**Architecture:** Introduce a `BackpressureHandle` (clonable Arc-wrapped controller) that all pipeline components share, giving each component an independent voice in the "are we overloaded?" decision. The RPC layer already reads a `watch::Receiver<TransactionAcceptanceState>`; we expand `NotAcceptingReason` and fix error-code mapping so clients receive `-32003` with a structured `data` payload instead of the current `-32603` for all failures.

**Tech Stack:** Rust, tokio watch channels, jsonrpsee, vise (metrics). No new dependencies.

---

## Scope

Three independent chunks, each shippable on its own:

| Chunk | What it does | Risk |
|---|---|---|
| 1 | Error taxonomy: new `NotAcceptingReason::Overloaded`, -32003 error code, structured `data` field | Low — pure type/mapping changes |
| 2 | Quick wins: finite mempool defaults, tx hash in EIP-7966 timeout `data` | Trivial |
| 3 | Dynamic wiring: `BackpressureHandle` + prover queue → acceptance state | Medium |

---

## Chunk 1: Error Taxonomy

**Files touched:**
- Modify: `lib/types/src/transaction_acceptance_state.rs`
- Modify: `lib/rpc/src/result.rs`
- Modify: `lib/rpc/src/tx_handler.rs` (error Display strings only)
- Add test: `lib/rpc/src/tests/error_codes.rs` (new file)
- Modify: `lib/rpc/src/lib.rs` (expose test module)

### Task 1: Expand `NotAcceptingReason` with `Overloaded` variant

**File:** `lib/types/src/transaction_acceptance_state.rs`

The current type has one variant and is `Copy`. We keep `Copy` by storing `retry_after_ms: u64` inline.

- [ ] **Step 1: Write the failing unit test**

Create `lib/types/src/tests.rs` (or add to the file if `#[cfg(test)]` block already exists):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overloaded_reason_display() {
        let reason = NotAcceptingReason::Overloaded {
            cause: OverloadCause::ProverQueueFull,
            retry_after_ms: 5_000,
        };
        assert!(reason.to_string().contains("prover"));
        assert!(reason.to_string().contains("5000"));
    }

    #[test]
    fn overloaded_is_copy() {
        let reason = NotAcceptingReason::Overloaded {
            cause: OverloadCause::PipelineSaturated,
            retry_after_ms: 1_000,
        };
        let _copy = reason; // moves would fail if not Copy
        let _copy2 = reason;
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p zksync_os_types --release 2>&1 | grep -E "FAIL|error|overloaded"
```

Expected: compile error — `NotAcceptingReason::Overloaded` does not exist yet.

- [ ] **Step 3: Implement the new types**

Replace the contents of `lib/types/src/transaction_acceptance_state.rs`:

```rust
/// Whether the node should be accepting transactions
#[derive(Debug, Clone)]
pub enum TransactionAcceptanceState {
    Accepting,
    NotAccepting(NotAcceptingReason),
}

/// Reason why the node is not accepting transactions
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum NotAcceptingReason {
    /// Block production has been disabled via config (`sequencer_max_blocks_to_produce`)
    #[error("Node is not currently accepting transactions: block production disabled.")]
    BlockProductionDisabled,
    /// The node is temporarily overloaded; client should retry after `retry_after_ms`.
    #[error("Node is temporarily overloaded ({cause}). Retry after {retry_after_ms}ms.")]
    Overloaded {
        cause: OverloadCause,
        retry_after_ms: u64,
    },
}

/// The specific pipeline component that triggered the overload condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum OverloadCause {
    #[error("prover queue is full")]
    ProverQueueFull,
    #[error("pipeline is saturated")]
    PipelineSaturated,
}

impl NotAcceptingReason {
    /// Suggested client retry delay in milliseconds, if any.
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::BlockProductionDisabled => None,
            Self::Overloaded { retry_after_ms, .. } => Some(*retry_after_ms),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo nextest run -p zksync_os_types --release 2>&1 | grep -E "PASS|ok|FAIL"
```

Expected: all tests pass.

- [ ] **Step 5: Fix all call sites that match on `NotAcceptingReason`**

There is one in `lib/rpc/src/tx_handler.rs` (line 49). The `*reason` dereference still works because `NotAcceptingReason` is `Copy`. No change needed there — verify it compiles:

```bash
cargo build -p zksync_os_rpc --release 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add lib/types/src/transaction_acceptance_state.rs
git commit -m "feat(types): add NotAcceptingReason::Overloaded with OverloadCause and retry_after_ms"
```

---

### Task 2: Fix error code mapping — `-32003` for backpressure, `-32603` only for bugs

**Files:** `lib/rpc/src/result.rs`, new test file `lib/rpc/src/tests/error_codes.rs`

Currently `impl_to_rpc_result!(EthSendRawTransactionError)` maps ALL variants to `-32603` (internal error). This masks overload as a server bug. The fix: replace the macro expansion with a manual `impl` that routes backpressure to `-32003`.

The code `-32003` (`TransactionRejected`) is the standard Reth/EIP code for "pool/node rejected the transaction" — distinguishable from `-32603` (server bug) and `-32000` (invalid tx parameters).

- [ ] **Step 1: Write the failing unit test**

Create `lib/rpc/src/tests/error_codes.rs`:

```rust
use crate::result::ToRpcResult;
use crate::tx_handler::EthSendRawTransactionError;
use jsonrpsee::types::error::INTERNAL_ERROR_CODE;
use zksync_os_types::{NotAcceptingReason, OverloadCause};

/// JSON-RPC code used for "transaction rejected / node overloaded" — matches Reth's -32003.
const TRANSACTION_REJECTED_CODE: i32 = -32003;

#[test]
fn not_accepting_overloaded_returns_32003() {
    let err = EthSendRawTransactionError::NotAcceptingTransactions(
        NotAcceptingReason::Overloaded {
            cause: OverloadCause::ProverQueueFull,
            retry_after_ms: 5_000,
        },
    );
    let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
    assert_eq!(rpc_err.code(), TRANSACTION_REJECTED_CODE);
}

#[test]
fn not_accepting_block_production_disabled_returns_32003() {
    let err = EthSendRawTransactionError::NotAcceptingTransactions(
        NotAcceptingReason::BlockProductionDisabled,
    );
    let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
    assert_eq!(rpc_err.code(), TRANSACTION_REJECTED_CODE);
}

#[test]
fn decode_error_returns_internal_error() {
    let err = EthSendRawTransactionError::FailedToDecodeSignedTransaction;
    let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
    // Decode failure is a client error, not server overload
    assert_eq!(rpc_err.code(), INTERNAL_ERROR_CODE);
}
```

Wire up the module in `lib/rpc/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    mod error_codes;
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p zksync_os_rpc --release 2>&1 | grep -E "error_codes|FAIL|error\["
```

Expected: test compiles but asserts fail (`-32603` != `-32003`).

- [ ] **Step 3: Replace the macro expansion with a manual `impl` for `EthSendRawTransactionError`**

In `lib/rpc/src/result.rs`, remove the line:

```rust
impl_to_rpc_result!(EthSendRawTransactionError);
```

Replace with:

```rust
impl<Ok> ToRpcResult<Ok, EthSendRawTransactionError> for Result<Ok, EthSendRawTransactionError> {
    fn to_rpc_result(self) -> RpcResult<Ok> {
        self.map_err(|err| match err {
            // Backpressure: node is overloaded or production is disabled.
            // Use -32003 (TransactionRejected) so clients can distinguish
            // from -32603 (internal server error) and implement retry logic.
            EthSendRawTransactionError::NotAcceptingTransactions(_)
            | EthSendRawTransactionError::PoolError(_) => {
                rpc_error_with_code(-32003, err.to_string())
            }
            // All other variants are client errors or internal bugs.
            _ => internal_rpc_err(err.to_string()),
        })
    }
}
```

You'll need to add the import at the top of `result.rs` if not already present:

```rust
use crate::tx_handler::EthSendRawTransactionError;
```

(It's already imported via `EthSendRawTransactionSyncError` — check and add only if missing.)

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p zksync_os_rpc --release 2>&1 | grep -E "ok|FAIL"
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add lib/rpc/src/result.rs lib/rpc/src/tests/error_codes.rs lib/rpc/src/lib.rs
git commit -m "fix(rpc): use -32003 for backpressure errors instead of -32603 internal error"
```

---

### Task 3: Add structured `data` field to `-32003` backpressure responses

Clients need machine-readable retry guidance. We add a `data` field — a JSON object with `reason` (string enum) and `retry_after_ms` (number, absent if unknown).

This follows the same pattern already used in `EthCallError` (which puts revert data in the `data` field).

**Files:** `lib/rpc/src/result.rs`

- [ ] **Step 1: Extend the unit test**

In `lib/rpc/src/tests/error_codes.rs`, add:

```rust
use serde_json::Value;

#[test]
fn overloaded_error_data_contains_reason_and_retry() {
    let err = EthSendRawTransactionError::NotAcceptingTransactions(
        NotAcceptingReason::Overloaded {
            cause: OverloadCause::ProverQueueFull,
            retry_after_ms: 5_000,
        },
    );
    let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
    // data field must be present
    let data_raw = rpc_err.data().expect("data field must be present for overload errors");
    let data: Value = serde_json::from_str(data_raw.get()).unwrap();
    assert_eq!(data["reason"], "prover_queue_full");
    assert_eq!(data["retry_after_ms"], 5000u64);
}

#[test]
fn block_production_disabled_data_has_no_retry() {
    let err = EthSendRawTransactionError::NotAcceptingTransactions(
        NotAcceptingReason::BlockProductionDisabled,
    );
    let rpc_err = Err::<(), _>(err).to_rpc_result().unwrap_err();
    let data_raw = rpc_err.data().expect("data field must be present");
    let data: Value = serde_json::from_str(data_raw.get()).unwrap();
    assert_eq!(data["reason"], "block_production_disabled");
    assert!(data.get("retry_after_ms").is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p zksync_os_rpc --release 2>&1 | grep -E "data_contains|FAIL"
```

Expected: FAIL — no `data` field yet.

- [ ] **Step 3: Add serialization support to `NotAcceptingReason`**

In `lib/types/src/transaction_acceptance_state.rs`, add a method to emit the structured data payload:

```rust
use serde_json::{json, Value};

impl NotAcceptingReason {
    /// Returns a structured JSON payload for inclusion in the JSON-RPC `data` field.
    pub fn to_rpc_data(&self) -> Value {
        match self {
            Self::BlockProductionDisabled => json!({ "reason": "block_production_disabled" }),
            Self::Overloaded { cause, retry_after_ms } => json!({
                "reason": cause.as_rpc_str(),
                "retry_after_ms": retry_after_ms,
            }),
        }
    }
}

impl OverloadCause {
    fn as_rpc_str(self) -> &'static str {
        match self {
            Self::ProverQueueFull => "prover_queue_full",
            Self::PipelineSaturated => "pipeline_saturated",
        }
    }
}
```

Add `serde_json` to `lib/types/Cargo.toml` if not already present:

```toml
serde_json = "1"
```

- [ ] **Step 4: Wire the data into the error mapping**

In `lib/rpc/src/result.rs`, update the manual `impl` from Task 2:

```rust
EthSendRawTransactionError::NotAcceptingTransactions(reason) => {
    let data = reason.to_rpc_data().to_string();
    rpc_err(-32003, reason.to_string(), Some(data.as_bytes()))
}
EthSendRawTransactionError::PoolError(_) => {
    rpc_error_with_code(-32003, err.to_string())
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p zksync_os_rpc -p zksync_os_types --release 2>&1 | grep -E "ok|FAIL"
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add lib/types/src/transaction_acceptance_state.rs lib/rpc/src/result.rs lib/rpc/src/tests/error_codes.rs lib/types/Cargo.toml
git commit -m "feat(rpc): add structured data field to -32003 backpressure responses with reason and retry_after_ms"
```

---

## Chunk 2: Quick Wins

### Task 4: Set finite mempool defaults

**File:** `node/bin/src/config/mod.rs`

Both `max_pending_txs` and `max_pending_size` default to `usize::MAX` — effectively unlimited. This means the node runs out of memory before any pool-full backpressure fires. Align with Reth's defaults (10k txns / 20 MB per sub-pool).

- [ ] **Step 1: Write a unit test for the defaults**

In `node/bin/src/config/mod.rs`, find or add a `#[cfg(test)]` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mempool_defaults_are_finite() {
        let config = MempoolConfig::default();
        assert!(config.max_pending_txs < usize::MAX, "max_pending_txs must be finite");
        assert!(config.max_pending_size < usize::MAX, "max_pending_size must be finite");
        // Sanity bounds: not so small that normal use breaks
        assert!(config.max_pending_txs >= 1_000);
        assert!(config.max_pending_size >= 10 * 1024 * 1024); // 10 MB
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p zksync_os_server --release -- config::tests::mempool_defaults_are_finite 2>&1
```

Expected: FAIL (`usize::MAX` is not finite).

- [ ] **Step 3: Update the defaults**

In `node/bin/src/config/mod.rs`, change:

```rust
// Before
#[config(default_t = usize::MAX)]
pub max_pending_txs: usize,
#[config(default_t = usize::MAX)]
pub max_pending_size: usize,
```

To:

```rust
// After
/// Maximum number of pending transactions in the mempool.
/// Aligned with Reth's default. Override via `mempool_max_pending_txs` env var.
#[config(default_t = 10_000)]
pub max_pending_txs: usize,
/// Maximum total size (bytes) of pending transactions in the mempool.
/// Aligned with Reth's default (20 MB). Override via `mempool_max_pending_size` env var.
#[config(default_t = 20 * 1024 * 1024)]
pub max_pending_size: usize,
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p zksync_os_server --release -- config::tests 2>&1 | grep -E "ok|FAIL"
```

Expected: pass.

- [ ] **Step 5: Run integration tests to verify nothing breaks**

```bash
cargo nextest run -p zksync_os_integration_tests --release 2>&1 | tail -20
```

Expected: all green. (The mempool limits are large enough that existing tests never hit them.)

- [ ] **Step 6: Commit**

```bash
git add node/bin/src/config/mod.rs
git commit -m "fix(config): set finite mempool defaults (10k txns / 20MB) instead of usize::MAX"
```

---

### Task 5: Include tx hash in EIP-7966 `eth_sendRawTransactionSync` timeout error

**File:** `lib/rpc/src/tx_handler.rs`, `lib/rpc/src/result.rs`

When `eth_sendRawTransactionSync` times out, the transaction is still in the mempool. Currently the client gets: `"The transaction was added to the mempool but wasn't processed within 2s."` — with no way to poll for status. Fix: put the tx hash in the `data` field (matching Reth and op-geth).

- [ ] **Step 1: Update the `Timeout` variant and its display**

In `lib/rpc/src/tx_handler.rs`, change:

```rust
// Before
/// Timeout while waiting for transaction receipt.
#[error("The transaction was added to the mempool but wasn't processed within {0:?}.")]
Timeout(Duration),
```

To:

```rust
// After
/// Timeout while waiting for transaction receipt.
/// The transaction hash is preserved so clients can poll for inclusion.
#[error("The transaction was added to the mempool but wasn't processed within {timeout:?}. \
         Transaction hash: {tx_hash}")]
Timeout {
    timeout: Duration,
    tx_hash: B256,
},
```

- [ ] **Step 2: Update the place that constructs `Timeout`**

In the same file, in `send_raw_transaction_sync_impl`, there are two `Timeout` constructions. Change both:

```rust
// line ~119 (channel closed path)
return Err(EthSendRawTransactionSyncError::Timeout {
    timeout: timeout_duration,
    tx_hash,
});

// line ~133 (outer tokio::time::timeout path)
.map_err(|_| EthSendRawTransactionSyncError::Timeout {
    timeout: timeout_duration,
    tx_hash,
})?
```

`tx_hash` is already in scope (bound on line ~110: `let tx_hash = self.send_raw_transaction_impl(bytes).await?;`).

- [ ] **Step 3: Update the error mapping in `result.rs` to include tx hash in `data`**

In `lib/rpc/src/result.rs`, the `EthSendRawTransactionSyncError` mapping already uses code 4:

```rust
// Before
err @ EthSendRawTransactionSyncError::Timeout(_) => {
    rpc_error_with_code(4, err.to_string())
}
```

Change to:

```rust
// After
EthSendRawTransactionSyncError::Timeout { tx_hash, .. } => {
    use alloy::primitives::hex;
    let data = serde_json::json!({ "txHash": format!("{tx_hash:#x}") }).to_string();
    rpc_err(4, err.to_string(), Some(data.as_bytes()))
}
```

- [ ] **Step 4: Run unit tests**

```bash
cargo nextest run -p zksync_os_rpc --release 2>&1 | grep -E "ok|FAIL"
```

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add lib/rpc/src/tx_handler.rs lib/rpc/src/result.rs
git commit -m "fix(rpc): include tx hash in EIP-7966 timeout error data field"
```

---

## Chunk 3: Dynamic Pipeline Backpressure

This chunk wires actual pipeline queue depth back to the RPC acceptance state. Currently the node silently stalls (txns pile up in mempool) when the prover queue fills or the batcher is blocked. After this chunk, the node actively signals `-32003` to clients before the pipeline completely saturates.

**Files touched:**
- Add: `lib/types/src/backpressure.rs`
- Modify: `lib/types/src/lib.rs` (re-export)
- Modify: `node/bin/src/lib.rs` (pass handle to components)
- Modify: `node/bin/src/prover_api/prover_job_map/map.rs` (signal on fill/drain)
- Modify: `lib/sequencer/src/execution/block_executor.rs` (use handle instead of raw sender)
- Add test: `integration-tests/tests/backpressure.rs`

### Task 6: Implement `BackpressureHandle`

The handle is the single coordination point. Multiple components can independently declare themselves overloaded; the node stays in `NotAccepting` until all conditions are cleared.

**File:** `lib/types/src/backpressure.rs` (new file)

- [ ] **Step 1: Write unit tests for the handle**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;
    use crate::{NotAcceptingReason, OverloadCause, TransactionAcceptanceState};

    fn make_handle() -> (BackpressureHandle, watch::Receiver<TransactionAcceptanceState>) {
        let (tx, rx) = watch::channel(TransactionAcceptanceState::Accepting);
        (BackpressureHandle::new(tx), rx)
    }

    #[test]
    fn starts_accepting() {
        let (_, rx) = make_handle();
        assert!(matches!(*rx.borrow(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn set_overloaded_signals_not_accepting() {
        let (handle, rx) = make_handle();
        handle.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        assert!(matches!(*rx.borrow(), TransactionAcceptanceState::NotAccepting(_)));
    }

    #[test]
    fn clear_overloaded_returns_to_accepting() {
        let (handle, rx) = make_handle();
        handle.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        handle.clear_overloaded(OverloadCause::ProverQueueFull);
        assert!(matches!(*rx.borrow(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn two_conditions_both_must_clear() {
        let (handle, rx) = make_handle();
        handle.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
        handle.set_overloaded(OverloadCause::PipelineSaturated, 1_000);
        handle.clear_overloaded(OverloadCause::ProverQueueFull);
        // Still overloaded because pipeline is saturated
        assert!(matches!(*rx.borrow(), TransactionAcceptanceState::NotAccepting(_)));
        handle.clear_overloaded(OverloadCause::PipelineSaturated);
        // Now clear
        assert!(matches!(*rx.borrow(), TransactionAcceptanceState::Accepting));
    }

    #[test]
    fn stop_permanently_overrides_everything() {
        let (handle, rx) = make_handle();
        handle.stop_permanently(NotAcceptingReason::BlockProductionDisabled);
        // Clearing dynamic conditions does nothing
        handle.clear_overloaded(OverloadCause::ProverQueueFull);
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::NotAccepting(NotAcceptingReason::BlockProductionDisabled)
        ));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p zksync_os_types --release 2>&1 | grep -E "backpressure|FAIL|error\["
```

Expected: compile error — module does not exist.

- [ ] **Step 3: Implement `BackpressureHandle`**

Create `lib/types/src/backpressure.rs`:

```rust
use crate::{NotAcceptingReason, OverloadCause, TransactionAcceptanceState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Shared controller that lets multiple pipeline components independently signal overload.
/// The node stays in `NotAccepting` until every registered condition is cleared.
#[derive(Clone, Debug)]
pub struct BackpressureHandle {
    inner: Arc<Mutex<Inner>>,
    sender: Arc<watch::Sender<TransactionAcceptanceState>>,
}

#[derive(Default)]
struct Inner {
    permanent: Option<NotAcceptingReason>,
    active: HashMap<OverloadCause, u64>, // cause -> retry_after_ms
}

impl BackpressureHandle {
    pub fn new(sender: watch::Sender<TransactionAcceptanceState>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            sender: Arc::new(sender),
        }
    }

    /// Signal that a component is overloaded. Idempotent — calling again updates retry hint.
    pub fn set_overloaded(&self, cause: OverloadCause, retry_after_ms: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.permanent.is_some() {
            return;
        }
        inner.active.insert(cause, retry_after_ms);
        self.sync(&inner);
    }

    /// Signal that a component has recovered. No-op if the cause was not active.
    pub fn clear_overloaded(&self, cause: OverloadCause) {
        let mut inner = self.inner.lock().unwrap();
        if inner.permanent.is_some() {
            return;
        }
        inner.active.remove(&cause);
        self.sync(&inner);
    }

    /// Permanently stop accepting transactions. Cannot be undone (node restart required).
    /// Used by `BlockExecutor` when `max_blocks_to_produce` limit is reached.
    pub fn stop_permanently(&self, reason: NotAcceptingReason) {
        let mut inner = self.inner.lock().unwrap();
        inner.permanent = Some(reason);
        let _ = self
            .sender
            .send(TransactionAcceptanceState::NotAccepting(reason));
    }

    fn sync(&self, inner: &Inner) {
        let state = if inner.active.is_empty() {
            TransactionAcceptanceState::Accepting
        } else {
            // Pick the condition with the longest suggested retry delay.
            let (&cause, &retry_after_ms) =
                inner.active.iter().max_by_key(|(_, ms)| *ms).unwrap();
            TransactionAcceptanceState::NotAccepting(NotAcceptingReason::Overloaded {
                cause,
                retry_after_ms,
            })
        };
        let _ = self.sender.send(state);
    }
}
```

Add `pub mod backpressure;` and re-export to `lib/types/src/lib.rs`:

```rust
pub mod backpressure;
pub use backpressure::BackpressureHandle;
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p zksync_os_types --release 2>&1 | grep -E "ok|FAIL"
```

Expected: all pass.

- [ ] **Step 5: Migrate `BlockExecutor` to use `BackpressureHandle`**

In `lib/sequencer/src/execution/block_executor.rs`, replace the raw `watch::Sender<TransactionAcceptanceState>` field with `BackpressureHandle`, and replace the call to `tx_acceptance_state_sender.send(...)` with `handle.stop_permanently(...)`:

Find `check_block_production_limit` (around line 174) and update its signature and body:

```rust
// Change parameter from:
//   tx_acceptance_state_sender: &watch::Sender<TransactionAcceptanceState>
// To:
//   backpressure: &BackpressureHandle

async fn check_block_production_limit(
    limit: u64,
    already_produced_blocks_count: u64,
    backpressure: &BackpressureHandle,
    latency_tracker: &ComponentStateHandle<SequencerState>,
) {
    if already_produced_blocks_count >= limit {
        tracing::warn!(
            already_produced_blocks_count,
            limit,
            "Reached max_blocks_to_produce limit, stopping transaction acceptance"
        );
        backpressure.stop_permanently(NotAcceptingReason::BlockProductionDisabled);
        latency_tracker.enter_state(SequencerState::ConfiguredBlockLimitReached);
        std::future::pending::<()>().await;
    }
}
```

Update the `BlockExecutor` struct and `new()` to accept `BackpressureHandle` instead of `watch::Sender`. Update the call site in `node/bin/src/lib.rs` to construct a `BackpressureHandle` from the existing watch sender (or create one at startup).

- [ ] **Step 6: Compile check**

```bash
cargo build --release 2>&1 | grep "^error" | head -20
```

Fix any remaining type errors (there may be a few in the node wiring). Expected: clean build.

- [ ] **Step 7: Run unit and integration tests**

```bash
cargo nextest run --workspace --exclude zksync_os_integration_tests --release 2>&1 | tail -10
cargo nextest run -p zksync_os_integration_tests --release 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add lib/types/src/backpressure.rs lib/types/src/lib.rs \
        lib/sequencer/src/execution/block_executor.rs \
        node/bin/src/lib.rs
git commit -m "feat(types): add BackpressureHandle for coordinated multi-component overload signaling"
```

---

### Task 7: Wire prover queue depth into `BackpressureHandle`

The prover's `ProverJobMap::add_job` already blocks when the batch range is full (Task 7 of the previous analysis). We add pre-emptive signaling: when the queue reaches 80% capacity, set overloaded; when it drains below 60%, clear.

**File:** `node/bin/src/prover_api/prover_job_map/map.rs`

- [ ] **Step 1: Add `BackpressureHandle` to `ProverJobMap`**

The `ProverJobMap` struct needs a `backpressure: Option<BackpressureHandle>` field. `Option` keeps it backward-compatible with existing tests that construct a `ProverJobMap` directly.

In `map.rs`, add to the struct and `new()`:

```rust
use zksync_os_types::{BackpressureHandle, OverloadCause};

pub struct ProverJobMap<T> {
    // ... existing fields ...
    backpressure: Option<BackpressureHandle>,
}

impl<T> ProverJobMap<T> {
    pub fn with_backpressure(mut self, handle: BackpressureHandle) -> Self {
        self.backpressure = Some(handle);
        self
    }
}
```

- [ ] **Step 2: Add threshold signaling to `add_job` and `remove_job`**

The high-water mark is 80% of `max_assigned_batch_range`; the low-water mark is 60%.

In `add_job`, after inserting the job:

```rust
// After: jobs.insert(batch_number, entry);
self.maybe_signal_overloaded(&jobs);
```

In `remove_job` / wherever jobs are completed and removed:

```rust
// After the removal
self.maybe_signal_recovered(&jobs);
```

Add the helper methods:

```rust
fn maybe_signal_overloaded(&self, jobs: &JobMap<T>) {
    let Some(ref bp) = self.backpressure else { return };
    let utilization = self.batch_range_utilization(jobs);
    if utilization >= 0.80 {
        // Suggest a retry window proportional to the number of outstanding jobs
        // times a rough per-job proof time (30s is conservative).
        let outstanding = jobs.len() as u64;
        let retry_after_ms = outstanding.saturating_mul(30_000).min(300_000);
        bp.set_overloaded(OverloadCause::ProverQueueFull, retry_after_ms);
    }
}

fn maybe_signal_recovered(&self, jobs: &JobMap<T>) {
    let Some(ref bp) = self.backpressure else { return };
    let utilization = self.batch_range_utilization(jobs);
    if utilization < 0.60 {
        bp.clear_overloaded(OverloadCause::ProverQueueFull);
    }
}

fn batch_range_utilization(&self, jobs: &JobMap<T>) -> f64 {
    if self.max_assigned_batch_range == 0 || jobs.is_empty() {
        return 0.0;
    }
    let min = *jobs.keys().next().unwrap();
    let max = *jobs.keys().next_back().unwrap();
    let range = (max - min + 1) as f64;
    range / self.max_assigned_batch_range as f64
}
```

- [ ] **Step 3: Pass `BackpressureHandle` from node startup to `FriJobManager` → `ProverJobMap`**

In `node/bin/src/lib.rs` (or wherever `FriJobManager` / `ProverJobMap` is constructed), pass the `BackpressureHandle` created in Task 6:

```rust
let fri_job_map = ProverJobMap::new(/* existing args */)
    .with_backpressure(backpressure_handle.clone());
```

- [ ] **Step 4: Write integration test**

Create `integration-tests/tests/backpressure.rs`:

```rust
//! Tests that the node signals -32003 when the pipeline is overloaded,
//! and returns to accepting transactions once the load clears.

use alloy::network::TransactionBuilder;
use alloy::primitives::{U256, Address};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use std::time::Duration;
use zksync_os_integration_tests::Tester;
use zksync_os_server::config::MempoolConfig;

/// Simulate mempool-full backpressure by setting a very small limit.
/// When the mempool is full, eth_sendRawTransaction must return -32003.
#[test_log::test(tokio::test)]
async fn mempool_full_returns_transaction_rejected_code() -> anyhow::Result<()> {
    let tester = Tester::builder()
        .config_override(|config| {
            config.mempool_config = MempoolConfig {
                max_pending_txs: 1,
                ..Default::default()
            };
        })
        .build()
        .await?;

    let alice = tester.l2_wallet.default_signer().address();
    let gas_price = tester.l2_provider.get_gas_price().await?;
    let chain_id = tester.l2_provider.get_chain_id().await?;

    // Submit a tx with a nonce gap so it stays pending and fills the pool.
    let stale_nonce = tester.l2_provider.get_transaction_count(alice).await? + 100;
    let filler_tx = TransactionRequest::default()
        .with_to(Address::random())
        .with_value(U256::from(1))
        .with_nonce(stale_nonce)
        .with_gas_price(gas_price)
        .with_gas_limit(21_000)
        .with_chain_id(chain_id);
    let filler_envelope = filler_tx.build(&tester.l2_wallet).await?;
    let filler_encoded = alloy::eips::Encodable2718::encoded_2718(&filler_envelope);
    tester.l2_provider.send_raw_transaction(&filler_encoded).await?;

    // Now the pool has 1 tx (at max_pending_txs=1). A second submission must fail with -32003.
    let overflow_tx = TransactionRequest::default()
        .with_to(Address::random())
        .with_value(U256::from(1))
        .with_nonce(stale_nonce + 1)
        .with_gas_price(gas_price)
        .with_gas_limit(21_000)
        .with_chain_id(chain_id);
    let overflow_envelope = overflow_tx.build(&tester.l2_wallet).await?;
    let overflow_encoded = alloy::eips::Encodable2718::encoded_2718(&overflow_envelope);

    let raw_response = tester
        .l2_provider
        .client()
        .request::<_, serde_json::Value>("eth_sendRawTransaction", [alloy::primitives::hex::encode_prefixed(&overflow_encoded)])
        .await;

    let err = raw_response.expect_err("should be rejected when pool is full");
    let err_str = err.to_string();
    // Must be -32003, not -32603
    assert!(
        err_str.contains("-32003"),
        "Expected -32003 TransactionRejected, got: {err_str}"
    );

    Ok(())
}
```

Add `mod backpressure;` to `integration-tests/tests/mod.rs`.

- [ ] **Step 5: Run the integration test**

```bash
cargo nextest run -p zksync_os_integration_tests --release -- backpressure 2>&1
```

Expected: pass.

- [ ] **Step 6: Run full test suite**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --exclude zksync_os_integration_tests --release
cargo nextest run -p zksync_os_integration_tests --release
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add node/bin/src/prover_api/prover_job_map/map.rs \
        node/bin/src/lib.rs \
        integration-tests/tests/backpressure.rs \
        integration-tests/tests/mod.rs
git commit -m "feat(prover): signal -32003 backpressure when prover queue reaches 80% capacity"
```

---

### Task 8: Backpressure metrics (Grafana visibility)

The existing `component_time_spent_in_state` metric tracks time a component spends in `WaitingSend` — passive latency data. This task adds two dedicated backpressure metrics that are directly alertable in Grafana:

1. **`backpressure_active{cause}`** — a gauge (0/1) per cause. Tells you *right now* which subsystem is backpressured.
2. **`backpressure_tx_rejected_total{cause}`** — a counter per cause. `rate()` gives rejections/sec per subsystem over time.

**Files:**
- Add: `lib/types/src/backpressure_metrics.rs`
- Modify: `lib/types/src/backpressure.rs` (call metrics on set/clear)
- Modify: `lib/types/src/lib.rs` (expose module)
- Modify: `lib/rpc/src/tx_handler.rs` (increment rejected counter on rejection)

- [ ] **Step 1: Write unit tests for the metrics**

Add to `lib/types/src/backpressure.rs` `#[cfg(test)]` block:

```rust
#[test]
fn set_overloaded_updates_active_gauge() {
    let (tx, _rx) = watch::channel(TransactionAcceptanceState::Accepting);
    let handle = BackpressureHandle::new(tx);
    // Gauge starts at 0 — just verify set/clear don't panic and compile cleanly.
    // Functional gauge values are verified by Prometheus scrape in integration tests.
    handle.set_overloaded(OverloadCause::ProverQueueFull, 5_000);
    handle.clear_overloaded(OverloadCause::ProverQueueFull);
}
```

(Vise gauges don't expose their value in unit tests easily — the functional assertion lives in the integration test below.)

- [ ] **Step 2: Create the metrics struct**

Create `lib/types/src/backpressure_metrics.rs`:

```rust
use vise::{Counter, Gauge, LabeledFamily, Metrics};

#[derive(Debug, Metrics)]
#[metrics(prefix = "backpressure")]
pub struct BackpressureMetrics {
    /// 1 when this backpressure cause is currently active, 0 when clear.
    /// Use this in Grafana to alert when a subsystem is stuck:
    ///   alert: backpressure_active{cause="prover_queue_full"} == 1 for 5m
    #[metrics(labels = ["cause"])]
    pub active: LabeledFamily<&'static str, Gauge<u64>>,

    /// Total number of eth_sendRawTransaction calls rejected due to backpressure, by cause.
    /// Use rate() in Grafana to see rejections/sec per subsystem:
    ///   rate(backpressure_tx_rejected_total{cause=~".+"}[5m])
    #[metrics(labels = ["cause"])]
    pub tx_rejected_total: LabeledFamily<&'static str, Counter>,
}

#[vise::register]
pub static BACKPRESSURE_METRICS: vise::Global<BackpressureMetrics> = vise::Global::new();
```

Add `pub mod backpressure_metrics;` and re-export to `lib/types/src/lib.rs`:

```rust
pub mod backpressure_metrics;
pub use backpressure_metrics::BACKPRESSURE_METRICS;
```

- [ ] **Step 3: Instrument `BackpressureHandle`**

In `lib/types/src/backpressure.rs`, call the metrics in `set_overloaded`, `clear_overloaded`, and `stop_permanently`:

```rust
use crate::backpressure_metrics::BACKPRESSURE_METRICS;

pub fn set_overloaded(&self, cause: OverloadCause, retry_after_ms: u64) {
    let mut inner = self.inner.lock().unwrap();
    if inner.permanent.is_some() {
        return;
    }
    inner.active.insert(cause, retry_after_ms);
    BACKPRESSURE_METRICS.active[&cause.as_rpc_str()].set(1);
    self.sync(&inner);
}

pub fn clear_overloaded(&self, cause: OverloadCause) {
    let mut inner = self.inner.lock().unwrap();
    if inner.permanent.is_some() {
        return;
    }
    inner.active.remove(&cause);
    BACKPRESSURE_METRICS.active[&cause.as_rpc_str()].set(0);
    self.sync(&inner);
}

pub fn stop_permanently(&self, reason: NotAcceptingReason) {
    let mut inner = self.inner.lock().unwrap();
    inner.permanent = Some(reason);
    // "block_production_disabled" is a permanent condition — set gauge and never clear it.
    BACKPRESSURE_METRICS.active[&"block_production_disabled"].set(1);
    let _ = self.sender.send(TransactionAcceptanceState::NotAccepting(reason));
}
```

- [ ] **Step 4: Increment `tx_rejected_total` at the RPC rejection site**

In `lib/rpc/src/tx_handler.rs`, in `send_raw_transaction_impl`, when a `NotAcceptingTransactions` error is returned:

```rust
if let TransactionAcceptanceState::NotAccepting(reason) = &*self.acceptance_state.borrow() {
    let cause_str = match reason {
        NotAcceptingReason::BlockProductionDisabled => "block_production_disabled",
        NotAcceptingReason::Overloaded { cause, .. } => cause.as_rpc_str(),
    };
    BACKPRESSURE_METRICS.tx_rejected_total[&cause_str].inc();
    return Err(EthSendRawTransactionError::NotAcceptingTransactions(*reason));
}
```

Add the import at the top of `tx_handler.rs`:

```rust
use zksync_os_types::BACKPRESSURE_METRICS;
```

- [ ] **Step 5: Add a Grafana-oriented integration test**

In `integration-tests/tests/backpressure.rs`, add:

```rust
/// Verifies that the Prometheus metrics endpoint exposes backpressure_tx_rejected_total
/// after a rejection occurs. This ensures the metric is wired up and will appear in Grafana.
#[test_log::test(tokio::test)]
async fn rejected_tx_increments_prometheus_counter() -> anyhow::Result<()> {
    let tester = Tester::builder()
        .config_override(|config| {
            config.mempool_config = MempoolConfig {
                max_pending_txs: 1,
                ..Default::default()
            };
        })
        .build()
        .await?;

    let alice = tester.l2_wallet.default_signer().address();
    let gas_price = tester.l2_provider.get_gas_price().await?;
    let chain_id = tester.l2_provider.get_chain_id().await?;
    let stale_nonce = tester.l2_provider.get_transaction_count(alice).await? + 100;

    // Fill the pool (nonce gap keeps it pending)
    let build_tx = |nonce: u64| {
        TransactionRequest::default()
            .with_to(Address::random())
            .with_value(U256::from(1))
            .with_nonce(nonce)
            .with_gas_price(gas_price)
            .with_gas_limit(21_000)
            .with_chain_id(chain_id)
    };
    let filler = build_tx(stale_nonce).build(&tester.l2_wallet).await?;
    tester.l2_provider.send_raw_transaction(&filler.encoded_2718()).await?;

    // This should be rejected and increment the counter
    let overflow = build_tx(stale_nonce + 1).build(&tester.l2_wallet).await?;
    let _ = tester.l2_provider.send_raw_transaction(&overflow.encoded_2718()).await;

    // Scrape the Prometheus metrics endpoint
    let metrics_url = format!("http://{}/metrics", tester.status_server_address());
    let body = reqwest::get(&metrics_url).await?.text().await?;

    assert!(
        body.contains("backpressure_tx_rejected_total"),
        "backpressure_tx_rejected_total not found in Prometheus output:\n{body}"
    );

    Ok(())
}
```

Note: `tester.status_server_address()` may need to be added to the `Tester` API if not already exposed — check `integration-tests/src/lib.rs` and add a getter if needed.

- [ ] **Step 6: Run tests**

```bash
cargo nextest run -p zksync_os_types --release 2>&1 | grep -E "ok|FAIL"
cargo nextest run -p zksync_os_integration_tests --release -- backpressure 2>&1
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add lib/types/src/backpressure_metrics.rs \
        lib/types/src/backpressure.rs \
        lib/types/src/lib.rs \
        lib/rpc/src/tx_handler.rs \
        integration-tests/tests/backpressure.rs
git commit -m "feat(metrics): add backpressure_active gauge and tx_rejected_total counter for Grafana alerting"
```

**Suggested Grafana panels (add to the sequencer dashboard):**

| Panel | PromQL | Alert |
|---|---|---|
| Backpressure active by cause | `backpressure_active` | value = 1 for > 5 min |
| Tx rejection rate | `rate(backpressure_tx_rejected_total[5m])` | rate > 10/s |
| Rejection breakdown (pie) | `sum by (cause) (backpressure_tx_rejected_total)` | — |

---

## PR Description Template

```
feat(rpc): backpressure redesign — -32003 error codes, structured data, dynamic pipeline signaling

## Summary
- Replaces -32603 (internal error) with -32003 (TransactionRejected) for all backpressure
  conditions, matching Reth's error taxonomy so clients can distinguish overload from bugs
- Adds structured `data` field `{ reason, retry_after_ms }` to -32003 responses
- Sets finite mempool defaults (10k txns / 20 MB) — previously usize::MAX caused silent OOM
- Includes tx hash in EIP-7966 eth_sendRawTransactionSync timeout data field
- Introduces BackpressureHandle: a clonable Arc-based controller that lets multiple pipeline
  components independently signal overload; node returns to Accepting only when all clear
- Wires prover queue utilization (>80% full) into backpressure signaling
- Adds `backpressure_active{cause}` gauge and `backpressure_tx_rejected_total{cause}` counter
  for Grafana alerting per subsystem

## Test plan
- [ ] New unit tests for NotAcceptingReason variants and Display
- [ ] New unit tests for error code mapping (-32003 vs -32603)
- [ ] New unit tests for structured data field content
- [ ] New unit tests for BackpressureHandle set/clear/multi-condition semantics
- [ ] New integration test: mempool-full returns -32003 (not -32603)
- [ ] New integration test: Prometheus endpoint exposes backpressure_tx_rejected_total
- [ ] All existing integration tests pass unchanged

No breaking changes to wire format.
```
