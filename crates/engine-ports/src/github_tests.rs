use super::*;
use std::sync::Mutex;

/// An observer like the real one: its `close` closes whatever answers at the address, so only
/// `close_checked` stands between a stale key and another pull request.
struct Address {
    answers: Result<ReadIdentity, GhFailure>,
    closed: Mutex<Vec<(String, u64)>>,
}

impl PrObserver for Address {
    fn observe<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> Observed<'a> {
        Box::pin(async { Err(GhFailure::Unreadable("only the identity is read".into())) })
    }
    fn close<'a>(&'a self, _: &'a str, repository: &'a str, number: u64, _: &'a ProviderPrKey, _: &'a str) -> Closed<'a> {
        self.closed.lock().unwrap().push((repository.to_owned(), number));
        Box::pin(async { Ok(()) })
    }
    fn read_identity<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> Located<'a> {
        let answers = self.answers.clone();
        Box::pin(async move { answers })
    }
}

fn answering(key: &str) -> Address {
    Address {
        answers: Ok(ReadIdentity { key: ProviderPrKey(key.into()), locator: ProviderLocator(format!("PR_{key}")), repository: "ana/lumo".into(), head_ref: "csv-export".into() }),
        closed: Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn a_close_checked_closes_only_the_pull_request_that_answers_with_the_key() {
    let right = answering("1");
    close_checked(&right, "github.com", "ana/lumo", 42, &ProviderPrKey("1".into()), "bye").await.unwrap();
    assert_eq!(*right.closed.lock().unwrap(), [("ana/lumo".to_owned(), 42)]);

    let other = answering("2");
    let failure = close_checked(&other, "github.com", "ana/lumo", 42, &ProviderPrKey("1".into()), "bye").await.unwrap_err();
    assert!(matches!(failure, GhFailure::IdentityMismatch(_)), "{failure:?}");
    assert!(other.closed.lock().unwrap().is_empty(), "the pull request that took the address is never closed");
}

#[tokio::test]
async fn a_close_checked_that_cannot_read_the_address_closes_nothing_and_says_why() {
    let gone = Address { answers: Err(GhFailure::NotFound("Could not resolve to a Repository".into())), closed: Mutex::new(Vec::new()) };
    let failure = close_checked(&gone, "github.com", "ana/lumo", 42, &ProviderPrKey("1".into()), "bye").await.unwrap_err();
    assert!(failure.not_found(), "{failure:?}");
    assert!(gone.closed.lock().unwrap().is_empty());
}
