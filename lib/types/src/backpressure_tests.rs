#[cfg(test)]
mod tests {
    use crate::backpressure::BackpressureHandle;
    use crate::transaction_acceptance_state::TransactionAcceptanceState;

    #[tokio::test]
    async fn set_and_clear_single_cause() {
        let handle = BackpressureHandle::new_for_test();
        let mut rx = handle.subscribe();

        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::Accepting
        ));

        handle.set_overloaded("test_component", 1000);
        rx.changed().await.unwrap();
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::NotAccepting(_)
        ));

        handle.clear_overloaded("test_component");
        rx.changed().await.unwrap();
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::Accepting
        ));
    }

    #[tokio::test]
    async fn multiple_causes_clears_only_when_all_gone() {
        let handle = BackpressureHandle::new_for_test();
        let mut rx = handle.subscribe();

        handle.set_overloaded("component_a", 1000);
        rx.changed().await.unwrap();
        handle.set_overloaded("component_b", 1000);

        // Still not accepting after clearing one cause
        handle.clear_overloaded("component_a");
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::NotAccepting(_)
        ));

        // Accepting again after clearing the last cause
        handle.clear_overloaded("component_b");
        rx.changed().await.unwrap();
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::Accepting
        ));
    }

    #[tokio::test]
    async fn duplicate_set_does_not_broadcast_twice() {
        let handle = BackpressureHandle::new_for_test();
        let mut rx = handle.subscribe();

        handle.set_overloaded("component_a", 1000);
        rx.changed().await.unwrap();

        // Second set for the same component should be a no-op (already in the set)
        handle.set_overloaded("component_a", 1000);
        assert!(!rx.has_changed().unwrap());

        handle.clear_overloaded("component_a");
        rx.changed().await.unwrap();
        assert!(matches!(
            *rx.borrow(),
            TransactionAcceptanceState::Accepting
        ));
    }

    #[tokio::test]
    async fn borrow_reflects_current_state() {
        let handle = BackpressureHandle::new_for_test();
        // Keep a receiver alive so that send() doesn't silently drop the value.
        let _rx = handle.subscribe();

        assert!(matches!(
            *handle.borrow(),
            TransactionAcceptanceState::Accepting
        ));

        handle.set_overloaded("c", 500);
        assert!(matches!(
            *handle.borrow(),
            TransactionAcceptanceState::NotAccepting(_)
        ));
    }
}
