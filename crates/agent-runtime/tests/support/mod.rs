use alondra_domain::Event;
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId};
use alondra_provider_contract::{Capability, Placement, ProviderBinding};
use alondra_store::{AuthorityScope, InputPayload, LifecycleTransaction, OwnerGrant};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[path = "../../../test_support.rs"]
pub mod test_support;
pub use test_support::Directory;
pub fn directory() -> Directory {
    let directory = Directory::for_unix_socket();
    std::fs::create_dir(&*directory).unwrap();
    std::fs::set_permissions(&*directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}
pub fn owner(directory: &Directory, previous: u64) -> OwnerGrant {
    let database = directory.join("domain.db");
    let fresh = !database.exists();
    let mut connection = Connection::open(database).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    if fresh {
        connection
            .execute_batch(include_str!(
                "../../../alondrad/src/domain_store/schema.sql"
            ))
            .unwrap();
        connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'fixture-epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Fixture',0); INSERT INTO agents(id,name) VALUES('node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Task','not_started',0,0,0,0)").unwrap();
        connection
            .execute(
                "INSERT INTO task_worktrees VALUES('task',?1,0)",
                [directory.to_str().unwrap()],
            )
            .unwrap();
    }
    let mut raw = connection.transaction().unwrap();
    LifecycleTransaction::install(&mut raw).unwrap();
    let mut tx =
        LifecycleTransaction::attach(raw, AuthorityScope::new("fixture-epoch".into()).unwrap())
            .unwrap();
    if fresh {
        tx.create(
            &"task".into(),
            InputPayload::new(InputId::new("input").unwrap(), "Fixture prompt").unwrap(),
        )
        .unwrap();
        tx.apply(
            &"task".into(),
            0,
            "admit",
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("instance").unwrap(),
                host: HostId::new("host").unwrap(),
            },
        )
        .unwrap();
        let binding = ProviderBinding::new(
            "binding".into(),
            "fixture".into(),
            "instance".into(),
            "config".into(),
            1,
            1,
            BTreeSet::from([Capability::OfferInput]),
            "credential-ref".into(),
            1,
        )
        .unwrap();
        tx.stage_launch(
            &"task".into(),
            &Placement::LocalDirectory(directory.to_str().unwrap().into()),
            &binding,
        )
        .unwrap();
    }
    let stamp = tx
        .load(&"task".into())
        .unwrap()
        .execution()
        .unwrap()
        .clone();
    tx.reserve_owner(&stamp, previous).unwrap();
    tx.commit().unwrap().owners.pop().unwrap()
}
pub fn fixture() -> PathBuf {
    let value = PathBuf::from(
        std::env::var_os("ALONDRA_OWNED_HOST_FIXTURE").expect("Bazel fixture rootpath"),
    );
    if value.is_absolute() {
        return value;
    }
    Path::new(&std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles"))
        .join(std::env::var_os("TEST_WORKSPACE").unwrap())
        .join(value)
        .canonicalize()
        .unwrap()
}
pub fn environment(directory: &Directory) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".into(), directory.to_str().unwrap().into()),
        ("ALONDRA_FIXTURE_MODE".into(), "host".into()),
        (
            "ALONDRA_FIXTURE_STATE".into(),
            directory.to_str().unwrap().into(),
        ),
    ])
}
