use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

const LONG: Duration = Duration::from_secs(60);

fn id() -> InteractionId {
    "delivery".into()
}

fn answering_within(budget: Duration) -> DeliveryReviews {
    let reviews = DeliveryReviews::default();
    reviews.limit(Limits { answer_within: budget, ..Limits::default() });
    reviews
}

#[tokio::test]
async fn a_slow_delivery_answers_still_checking_and_the_same_call_picks_up_its_outcome() {
    let reviews = answering_within(Duration::ZERO);
    let started = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let work = |started: Arc<AtomicUsize>, gate: Arc<Notify>| {
        move || async move {
            started.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
            Ok(json!({"stored": true}))
        }
    };

    let first = reviews.deliver(&id(), "same".into(), work(started.clone(), gate.clone())).await;
    assert_eq!(first.err().expect("still checking").code, "delivery_review_pending");

    let again = reviews.deliver(&id(), "same".into(), work(started.clone(), gate.clone())).await;
    assert_eq!(again.err().expect("still checking").code, "delivery_review_pending");

    gate.notify_one();
    reviews.limit(Limits { answer_within: LONG, ..Limits::default() });
    let finished = reviews.deliver(&id(), "same".into(), work(started.clone(), gate.clone())).await;
    assert_eq!(finished.expect("the retry gets the outcome"), json!({"stored": true}));
    assert_eq!(started.load(Ordering::SeqCst), 1, "the delivery ran once for three calls");
}

#[tokio::test]
async fn a_finished_delivery_is_handed_over_once() {
    let reviews = answering_within(LONG);
    let started = Arc::new(AtomicUsize::new(0));
    for expected in 1..=2 {
        let counter = started.clone();
        let outcome = reviews
            .deliver(&id(), "same".into(), move || async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(DomainError::new("private_names_in_public_content", "fix it"))
            })
            .await;
        assert_eq!(outcome.err().expect("refused").code, "private_names_in_public_content");
        assert_eq!(started.load(Ordering::SeqCst), expected, "a refusal is checked again after the node fixes it");
    }
}

#[tokio::test]
async fn a_delivery_whose_call_is_cut_still_ends_and_its_outcome_waits_for_the_call_sent_again() {
    let reviews = Arc::new(answering_within(LONG));
    let gate = Arc::new(Notify::new());
    let ended = Arc::new(Notify::new());
    let (opened, finished) = (gate.clone(), ended.clone());
    let cut = tokio::spawn({
        let reviews = reviews.clone();
        async move {
            reviews.deliver(&id(), "same".into(), move || async move {
                opened.notified().await;
                finished.notify_one();
                Ok(json!({"stored": true}))
            }).await
        }
    });
    tokio::task::yield_now().await;
    cut.abort();
    assert!(cut.await.unwrap_err().is_cancelled(), "the node stopped waiting");

    gate.notify_one();
    ended.notified().await;
    let again = reviews.deliver(&id(), "same".into(), || async { panic!("the delivery already ended") }).await;
    assert_eq!(again.expect("the outcome of the cut call"), json!({"stored": true}));
}

#[tokio::test]
async fn another_body_or_caller_with_the_same_interaction_never_picks_up_an_outcome_that_is_not_its_own() {
    let reviews = answering_within(Duration::ZERO);
    let gate = Arc::new(Notify::new());
    let opened = gate.clone();
    let first = reviews.deliver(&id(), "first".into(), move || async move {
        opened.notified().await;
        Ok(json!({"version": 1}))
    }).await;
    assert_eq!(first.err().expect("still checking").code, "delivery_review_pending");

    let other = reviews.deliver(&id(), "second".into(), || async { panic!("waits for the first check to end") }).await;
    assert!(other.err().expect("still checking").message.contains("earlier version"));

    gate.notify_one();
    reviews.limit(Limits { answer_within: LONG, ..Limits::default() });
    tokio::time::timeout(LONG, async {
        while guard(&reviews.running).get(&id()).is_some_and(|entry| entry.finished.borrow().is_none()) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let second = reviews.deliver(&id(), "second".into(), || async { Ok(json!({"version": 2})) }).await;
    assert_eq!(second.expect("its own outcome"), json!({"version": 2}));
}
