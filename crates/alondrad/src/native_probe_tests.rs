use super::*;
use std::cell::RefCell;

#[tokio::test]
async fn stale_probe_storms_back_off_and_success_starts_a_fresh_retry_sequence() {
    let waits = RefCell::new(Vec::new());
    let mut attempts = 0;
    retry_with(
        || {
            attempts += 1;
            std::future::ready(if attempts <= 10 { Outcome::Stale } else { Outcome::Finished })
        },
        |delay| {
            waits.borrow_mut().push(delay.as_secs());
            std::future::ready(())
        },
    ).await;
    assert_eq!(attempts, 11);
    assert_eq!(*waits.borrow(), [0, 1, 2, 4, 8, 16, 30, 30, 30, 30]);
    let mut attempts = 0;
    retry_with(
        || {
            attempts += 1;
            std::future::ready(if attempts == 1 { Outcome::Stale } else { Outcome::Finished })
        },
        |delay| {
            waits.borrow_mut().push(delay.as_secs());
            std::future::ready(())
        },
    ).await;
    assert_eq!(waits.borrow().last(), Some(&0), "success resets the backoff");
}

#[tokio::test]
async fn ordinary_errors_do_not_trigger_rapid_retries() {
    use crate::domain_store::StoreError;
    use alondra_core::domain::DomainError;
    for error in [StoreError::Domain(DomainError::new("denied", "stale_probe")), StoreError::Storage(anyhow::anyhow!("stale_probe"))] {
        let mut attempts = 0;
        retry_with(
            || {
                attempts += 1;
                std::future::ready(Outcome::of(&error))
            },
            |_| -> std::future::Ready<()> { panic!("only the typed stale_probe error can retry") },
        ).await;
        assert_eq!(attempts, 1);
    }
}
