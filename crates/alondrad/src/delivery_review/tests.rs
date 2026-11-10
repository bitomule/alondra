use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

const LONG: Duration = Duration::from_secs(60);

fn id() -> InteractionId {
    "delivery".into()
}

#[tokio::test]
async fn a_slow_review_answers_still_reading_and_the_same_call_picks_up_its_result() {
    let reviews = DeliveryReviews::default();
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let work = |started: Arc<AtomicUsize>, gate: Arc<Notify>| {
        move || async move {
            started.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
            Ok(None)
        }
    };

    let first = reviews
        .review(&id(), Duration::ZERO, work(started.clone(), gate.clone()))
        .await;
    assert_eq!(first.err().expect("still reading").code, "delivery_review_pending");

    let again = reviews
        .review(&id(), Duration::ZERO, work(started.clone(), gate.clone()))
        .await;
    assert_eq!(again.err().expect("still reading").code, "delivery_review_pending");

    gate.notify_one();
    let finished = reviews
        .review(&id(), LONG, work(started.clone(), gate.clone()))
        .await;
    assert!(matches!(finished, Ok(None)), "the retry gets the finished review");
    assert_eq!(started.load(Ordering::SeqCst), 1, "the read ran once for three calls");
}

#[tokio::test]
async fn a_finished_review_is_handed_over_once() {
    let reviews = DeliveryReviews::default();
    let started = Arc::new(AtomicUsize::new(0));
    for expected in 1..=2 {
        let counter = started.clone();
        let outcome = reviews
            .review(&id(), LONG, move || async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(DomainError::new("private_names_in_public_content", "fix it"))
            })
            .await;
        assert_eq!(outcome.err().expect("refused").code, "private_names_in_public_content");
        assert_eq!(started.load(Ordering::SeqCst), expected, "a refusal is read again after the node fixes it");
    }
}
