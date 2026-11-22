//! Retention compacts what an ended task kept (`engine-kernel`'s `retention`), and the screens
//! `views` renders from it stay the same.
use crate::domain_runtime::DomainRuntime;
use alondra_store_sqlite::retention_fixtures::ended_task;
use alondra_store_sqlite::testing::Fixture;
use alondra_domain_rules::{DomainState, ReadIndex};

#[test]
fn compacting_an_ended_task_leaves_what_the_apps_show_the_same() {
    let fixture = Fixture::new();
    drop(ended_task(&fixture));
    rusqlite::Connection::open(fixture.path()).unwrap().execute("UPDATE domain_outbox SET state='handed'", []).unwrap();
    let runtime = DomainRuntime::open_unwired_at(&fixture.0, &fixture.path()).unwrap();
    let before = runtime.state().unwrap();

    assert_eq!(runtime.retain(alondra_core::now_millis()).unwrap().compacted, 1);

    let after = runtime.state().unwrap();
    let renumbered = serde_json::to_string(&shown(&before)).unwrap().replace(&format!("@{}", before.revision), &format!("@{}", after.revision));
    assert_eq!(serde_json::to_string(&shown(&after)).unwrap(), renumbered, "the apps show the same, buttons aside, which carry the revision");
}

/// What the apps show: the task's detail, its cards, the home and the task list, on every client.
fn shown(state: &DomainState) -> Vec<serde_json::Value> {
    use alondra_views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
    let index = ReadIndex::build(state);
    let live = LiveSnapshot::default();
    let mut keys = vec!["task:task-1".to_owned(), "home".into(), "tasks".into()];
    keys.extend(state.interactions.keys().map(|id| format!("card:{}", id.0)));
    let mut shown = vec![];
    for key in &keys {
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let view = alondra_views::build(key, RenderContext {
                domain: state, index: &index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback, cursor: "", rendered_at: "",
            });
            let Ok(view) = view else { continue };
            let mut view = serde_json::to_value(view).unwrap();
            for field in ["version", "status"] {
                view.as_object_mut().unwrap().remove(field);
            }
            shown.push(serde_json::json!({ "key": key, "view": view }));
        }
    }
    shown
}

#[test]
fn the_hourly_retention_deletes_an_expired_result_and_keeps_a_recent_one() {
    let fixture = Fixture::new();
    let runtime = DomainRuntime::open_unwired_at(&fixture.0, &fixture.path()).unwrap();
    let home = runtime.home().to_path_buf();
    for task in ["t-expired", "t-recent"] {
        std::fs::create_dir_all(home.join("files/results").join(task)).unwrap();
        std::fs::write(home.join("files/results").join(task).join("result.md"), "report").unwrap();
    }
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(15 * 24 * 60 * 60);
    std::fs::File::open(home.join("files/results/t-expired/result.md")).unwrap().set_modified(long_ago).unwrap();

    crate::retention::remove_expired_files(&runtime).unwrap();

    assert!(!home.join("files/results/t-expired").exists());
    assert!(home.join("files/results/t-recent/result.md").exists());
}
