//! What the user did that no view from the Mac shows yet. The TUI draws the daemon's views through
//! it, for every action it sends: an answer takes its card out at once, a view rendered before the
//! daemon confirmed it cannot bring the card back, and an answer the daemon has not confirmed
//! within `CONFIRMATION_MS` is overdue: its card shows again, with a notice, while the answer goes
//! on being delivered a growing wait apart. The same rules as the iPhone and Mac apps' `Optimistic`.
use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use super::Key;

/// How long an answer stays out of sight without the daemon confirming it.
pub const CONFIRMATION_MS: i64 = 30_000;
/// How long one attempt waits for the Mac before it counts as unanswered and is tried again.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
/// The waits between attempts; the last one repeats.
const RETRY_MS: [i64; 5] = [1_000, 2_000, 4_000, 8_000, 15_000];

/// What the notice names when an answer does not arrive in time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub card: String,
    pub label: String,
}

#[derive(Clone, Debug)]
struct Sending {
    target: Option<Key>,
    epoch: String,
    request: Value,
    answer: Answer,
    began: i64,
    failures: usize,
    retry_at: Option<i64>,
    overdue: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Applied {
    version: Option<String>,
    epoch: String,
}

/// An action the daemon answered, and what that means for its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub target: Option<Key>,
    /// The card had already come back as overdue.
    pub was_overdue: bool,
    /// The card is to show again where it was: the daemon did not apply the action.
    pub comes_back: bool,
}

/// An answer that has gone `CONFIRMATION_MS` without the daemon confirming it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overdue {
    pub command: String,
    pub target: Option<Key>,
    pub answer: Answer,
}

#[derive(Clone, Debug, Default)]
pub struct Optimistic {
    sending: HashMap<String, Sending>,
    applied: HashMap<Key, Applied>,
}

impl Optimistic {
    pub fn is_empty(&self) -> bool {
        self.sending.is_empty() && self.applied.is_empty()
    }

    /// Whether the card stays out: answered here, or with an answer on its way that is not overdue.
    pub fn hides(&self, key: &Key) -> bool {
        self.applied.contains_key(key)
            || self.sending.values().any(|s| !s.overdue && s.target.as_ref() == Some(key))
    }

    /// Whether any answer is still on its way for the card, overdue or not.
    pub fn is_sending(&self, key: &Key) -> bool {
        self.sending.values().any(|s| s.target.as_ref() == Some(key))
    }

    /// The command already on its way for the same action, option and input: sending it again
    /// keeps that command, so the daemon never does it twice.
    pub fn same_action(&self, request: &Value) -> Option<String> {
        let same = |earlier: &Value| ["action_id", "option_id", "input"].iter().all(|field| earlier[field] == request[field]);
        self.sending.iter().find(|(_, s)| same(&s.request)).map(|(command, _)| command.clone())
    }

    pub fn request(&self, command: &str) -> Option<&Value> {
        self.sending.get(command).map(|s| &s.request)
    }

    /// The action leaves now. A `target` is the card it takes out of sight until the daemon answers.
    pub fn begin(&mut self, request: Value, target: Option<Key>, answer: Answer, now: i64) {
        let Some(command) = request["command_id"].as_str().map(str::to_owned) else { return };
        let epoch = request["data_epoch"].as_str().unwrap_or_default().to_owned();
        if let Some(key) = &target {
            self.applied.remove(key);
        }
        self.sending.insert(command, Sending { target, epoch, request, answer, began: now, failures: 0, retry_at: None, overdue: false });
    }

    /// The user answered the same again: its card leaves again and it is tried at once, with a new
    /// `CONFIRMATION_MS` to arrive.
    pub fn again(&mut self, command: &str, now: i64) {
        if let Some(s) = self.sending.get_mut(command) {
            s.began = now;
            s.overdue = false;
            s.retry_at = None;
        }
    }

    /// An attempt never reached the daemon or got no answer: it is tried again after a growing wait.
    /// False when the command is no longer on its way.
    pub fn failed(&mut self, command: &str, now: i64) -> bool {
        let Some(s) = self.sending.get_mut(command) else { return false };
        s.retry_at = Some(now + RETRY_MS[s.failures.min(RETRY_MS.len() - 1)]);
        s.failures += 1;
        true
    }

    /// The requests whose wait is over, to send again now.
    pub fn due(&mut self, now: i64) -> Vec<Value> {
        self.sending.values_mut()
            .filter(|s| s.retry_at.is_some_and(|at| at <= now))
            .map(|s| {
                s.retry_at = None;
                s.request.clone()
            })
            .collect()
    }

    /// The answers that have just gone `CONFIRMATION_MS` unconfirmed: their cards show again.
    pub fn expire(&mut self, now: i64) -> Vec<Overdue> {
        self.sending.iter_mut()
            .filter(|(_, s)| !s.overdue && now - s.began >= CONFIRMATION_MS)
            .map(|(command, s)| {
                s.overdue = true;
                Overdue { command: command.clone(), target: s.target.clone(), answer: s.answer.clone() }
            })
            .collect()
    }

    /// The daemon answered `command`. Applied, its card stays out until a view other than `shown`
    /// arrives, even if it had come back as overdue; not applied, it shows again where it was.
    pub fn settle(&mut self, command: &str, applied: bool, shown: Option<String>) -> Option<Settled> {
        let s = self.sending.remove(command)?;
        let others = s.target.as_ref().is_some_and(|key| self.is_sending(key));
        if applied && let Some(key) = &s.target {
            self.applied.insert(key.clone(), Applied { version: shown, epoch: s.epoch.clone() });
        }
        Some(Settled { target: s.target, was_overdue: s.overdue, comes_back: !applied && !s.overdue && !others })
    }

    /// Keeps a card out that the daemon already settled, until a view other than `shown` arrives.
    pub fn hold(&mut self, key: Key, shown: Option<String>, epoch: String) {
        self.applied.insert(key, Applied { version: shown, epoch });
    }

    /// A view arrived: whatever it shows of an applied action is the daemon's word from now on. A
    /// view of another data epoch drops what was sent for the old one.
    pub fn reconcile(&mut self, version: &str, epoch: &str) {
        self.sending.retain(|_, s| s.epoch == epoch);
        self.applied.retain(|_, a| a.version.as_deref() == Some(version) && a.epoch == epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_000_000;

    fn request(command: &str, epoch: &str) -> Value {
        json!({"command_id": command, "data_epoch": epoch, "action_id": format!("ans:{command}"), "option_id": "yes", "input": {}})
    }

    fn answer() -> Answer {
        Answer { card: "Ship the beta?".into(), label: "Yes".into() }
    }

    fn sent(command: &str, card: &str) -> Optimistic {
        let mut optimistic = Optimistic::default();
        optimistic.begin(request(command, "e1"), Some(Key(card.into())), answer(), NOW);
        optimistic
    }

    #[test]
    fn an_answer_takes_its_card_out_the_moment_it_leaves() {
        let optimistic = sent("c1", "card");
        assert!(optimistic.hides(&Key("card".into())));
        assert!(!optimistic.hides(&Key("other".into())));
    }

    #[test]
    fn no_view_brings_back_a_card_whose_answer_is_still_on_its_way() {
        let mut optimistic = sent("c1", "card");
        optimistic.reconcile("v-old", "e1");
        optimistic.reconcile("v-new", "e1");
        assert!(optimistic.hides(&Key("card".into())), "only the daemon's answer settles it");
    }

    #[test]
    fn an_applied_answer_stays_out_until_a_newer_view_than_the_one_shown_arrives() {
        let mut optimistic = sent("c1", "card");
        let settled = optimistic.settle("c1", true, Some("v1".into())).unwrap();
        assert_eq!(settled, Settled { target: Some(Key("card".into())), was_overdue: false, comes_back: false });
        optimistic.reconcile("v1", "e1");
        assert!(optimistic.hides(&Key("card".into())), "the view rendered before the answer cannot bring it back");
        optimistic.reconcile("v2", "e1");
        assert!(!optimistic.hides(&Key("card".into())), "a newer view is the daemon's word");
        assert!(optimistic.is_empty());
    }

    #[test]
    fn an_answer_unconfirmed_after_thirty_seconds_brings_its_card_back_once() {
        let mut optimistic = sent("c1", "card");
        assert!(optimistic.expire(NOW + CONFIRMATION_MS - 1).is_empty());
        let overdue = optimistic.expire(NOW + CONFIRMATION_MS);
        assert_eq!(overdue, [Overdue { command: "c1".into(), target: Some(Key("card".into())), answer: answer() }]);
        assert!(!optimistic.hides(&Key("card".into())), "the card waits for the user again");
        assert!(optimistic.is_sending(&Key("card".into())), "the answer goes on being delivered");
        assert!(optimistic.expire(NOW + CONFIRMATION_MS + 5_000).is_empty(), "the notice is given once");
    }

    #[test]
    fn an_overdue_answer_that_arrives_takes_the_card_away_and_one_refused_leaves_it_where_it_is() {
        let mut optimistic = sent("c1", "card");
        optimistic.expire(NOW + CONFIRMATION_MS);
        let settled = optimistic.settle("c1", true, Some("v1".into())).unwrap();
        assert!(settled.was_overdue && !settled.comes_back);
        assert!(optimistic.hides(&Key("card".into())));

        let mut optimistic = sent("c1", "card");
        optimistic.expire(NOW + CONFIRMATION_MS);
        let settled = optimistic.settle("c1", false, None).unwrap();
        assert!(!settled.comes_back, "it is already back");
    }

    #[test]
    fn a_refused_answer_brings_its_card_back_at_once() {
        let mut optimistic = sent("c1", "card");
        let settled = optimistic.settle("c1", false, Some("v1".into())).unwrap();
        assert!(settled.comes_back);
        assert!(!optimistic.hides(&Key("card".into())));
    }

    #[test]
    fn a_failed_attempt_is_tried_again_a_growing_wait_apart_with_the_same_command() {
        let mut optimistic = sent("c1", "card");
        let mut at = NOW;
        let mut waits = vec![];
        for _ in 0..6 {
            assert!(optimistic.failed("c1", at));
            let mut next = at;
            while optimistic.due(next).is_empty() {
                next += 500;
                assert!(next - at <= 60_000, "never sent again");
            }
            waits.push(next - at);
            at = next;
        }
        assert_eq!(waits, [1_000, 2_000, 4_000, 8_000, 15_000, 15_000]);
        assert!(optimistic.failed("c1", at));
        let again = optimistic.due(at + 15_000);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0]["command_id"], "c1");
        assert!(optimistic.due(at + 15_000).is_empty(), "one attempt out at a time");
        assert!(optimistic.hides(&Key("card".into())), "a retry is silent");
    }

    #[test]
    fn the_same_answer_again_keeps_its_command_and_gets_a_new_thirty_seconds() {
        let mut optimistic = sent("c1", "card");
        optimistic.expire(NOW + CONFIRMATION_MS);
        let mut same = request("c2", "e1");
        same["action_id"] = "ans:c1".into();
        assert_eq!(optimistic.same_action(&same).as_deref(), Some("c1"));
        optimistic.again("c1", NOW + CONFIRMATION_MS + 1);
        assert!(optimistic.hides(&Key("card".into())));
        assert!(optimistic.expire(NOW + 2 * CONFIRMATION_MS).is_empty());
        assert_eq!(optimistic.expire(NOW + 2 * CONFIRMATION_MS + 1).len(), 1);
    }

    #[test]
    fn a_view_of_another_data_epoch_drops_what_was_sent_for_the_old_one() {
        let mut optimistic = sent("c1", "card");
        optimistic.reconcile("v1", "e2");
        assert!(!optimistic.hides(&Key("card".into())));
        assert!(optimistic.settle("c1", true, None).is_none(), "its answer no longer counts");
        assert!(optimistic.is_empty());
    }
}
