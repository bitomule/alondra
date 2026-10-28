//! The phone's Live Activity and the status header count from one place: whatever the domain went
//! through, the three counts the daemon sends are the ones the header shows.
use super::lifecycle_cache_tests::{run, step, three_tasks};
use super::*;
use crate::views::{self, Client, Lang, LiveSnapshot, RenderContext, Transport};
use proptest::prelude::*;

fn header(state: &DomainState, index: &ReadIndex) -> (ActivityCounts, u32) {
    let live = LiveSnapshot::default();
    let view = views::build("home", RenderContext {
        domain: state, index, live: &live, client: Client::Ios, lang: Lang::En,
        transport: Transport::Loopback, cursor: "cursor", rendered_at: "",
    }).expect("home renders");
    let count = |id: &str| view.status.counts.iter().find(|count| count.id == id).map(|count| count.value).expect(id);
    (ActivityCounts { inbox: count("inbox"), waiting: count("waiting"), working: count("working") }, view.status.badge)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn the_activity_counts_are_the_status_counts(steps in prop::collection::vec(step(), 1..30)) {
        let mut s = three_tasks();
        for (n, step) in steps.iter().enumerate() {
            let _ = run(&mut s, step, n);
            let index = ReadIndex::build(&s);
            let activity = views::activity_counts(&index);
            let (counts, badge) = header(&s, &index);
            prop_assert_eq!(activity, counts, "after {:?} in {:?}", step, steps);
            prop_assert_eq!(activity.inbox, badge, "the Inbox is the icon badge");
            prop_assert_eq!(activity.inbox, views::inbox_badge(&index));
        }
    }
}

#[test]
fn the_generated_domains_reach_every_count() {
    use super::lifecycle_cache_tests::Step;
    let mut s = three_tasks();
    for (n, step) in [Step::Start(0), Step::Ask(0), Step::Start(1)].iter().enumerate() {
        let _ = run(&mut s, step, n);
    }
    let activity = views::activity_counts(&ReadIndex::build(&s));
    assert!(activity.inbox > 0 && activity.waiting > 0 && activity.working > 0, "{activity:?}");
    assert!(activity.any());
    assert!(!ActivityCounts::default().any());
}
