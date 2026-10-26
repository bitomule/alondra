//! This user's own private values, read from their Mac, for the final check: their name and home,
//! the Mac's names, their GitHub login, and from `domain.db` every title, repository, branch,
//! worktree, account and node name.
use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use super::canary::Private;
use super::collect::{Host, open_domain};

pub fn of_this_mac(host: &dyn Host, vocabulary: &HashSet<String>) -> Private {
    let mut private = Private::default();
    let home = host.home();
    if let Some(user_home) = home.parent().filter(|_| home.ends_with(".alondra")) {
        add_path(&mut private, "your home folder", user_home, vocabulary);
    }
    add_path(&mut private, "your home folder", home, vocabulary);
    if let Ok(user) = std::env::var("USER") {
        private.add("your user name", &user, vocabulary);
    }
    if let Ok(user_home) = std::env::var("HOME") {
        add_path(&mut private, "your home folder", Path::new(&user_home), vocabulary);
    }
    for key in ["ComputerName", "LocalHostName", "HostName"] {
        if let Some(name) = host.run("scutil", &["--get", key]) {
            private.add("the name of this Mac", name.trim(), vocabulary);
        }
    }
    if let Some(name) = host.run("hostname", &[]) {
        let name = name.trim();
        private.add("the name of this Mac", name, vocabulary);
        private.add("the name of this Mac", name.trim_end_matches(".local"), vocabulary);
    }
    if let Some(login) = host.run("gh", &["config", "get", "-h", "github.com", "user"]) {
        private.add("your GitHub user", login.trim(), vocabulary);
    }
    if let Ok(devices) = std::fs::read_to_string(home.join("devices.json"))
        && let Ok(devices) = serde_json::from_str::<Value>(&devices)
    {
        strings_under(&devices, &["name", "label"], &mut |name| private.add("the name of a device", name, vocabulary));
    }
    if let Ok(db) = open_domain(home) {
        let mut add_all = |what: &'static str, sql: &str| {
            let Ok(mut query) = db.prepare(sql) else { return };
            let Ok(rows) = query.query_map([], |r| r.get::<_, Option<String>>(0)) else { return };
            for value in rows.flatten().flatten() {
                private.add(what, &value, vocabulary);
                if let Some((owner, name)) = value.split_once('/')
                    && what == "the name of a repository"
                {
                    private.add(what, owner, vocabulary);
                    private.add(what, name, vocabulary);
                }
            }
        };
        add_all("the title of a task", "SELECT title FROM tasks");
        add_all("the title of a request", "SELECT title FROM requests");
        add_all("the name of a repository", "SELECT repository FROM pull_requests");
        add_all("the name of a branch", "SELECT head FROM pull_requests");
        add_all("the name of a branch", "SELECT branch FROM task_worktree_records");
        add_all("a folder of a task", "SELECT path FROM task_worktree_records");
        add_all("a folder of a task", "SELECT dir FROM task_worktrees");
        add_all("the name of a node", "SELECT name FROM agents");
        add_all("the name of an account", "SELECT account FROM task_accounts");
        if let Ok(accounts) = db.query_row("SELECT accounts FROM accounts", [], |r| r.get::<_, String>(0))
            && let Ok(accounts) = serde_json::from_str::<Value>(&accounts)
        {
            strings_under(&accounts, &["name", "label", "email", "id"], &mut |value| private.add("the name of an account", value, vocabulary));
        }
        for path in folders(&db) {
            add_path(&mut private, "a folder of a project", Path::new(&path), vocabulary);
        }
    }
    private
}

/// A path, and each of its folder names below the home folder (`ana.lopez`, `acme-billing`).
fn add_path(private: &mut Private, what: &'static str, path: &Path, vocabulary: &HashSet<String>) {
    private.add(what, &path.to_string_lossy(), vocabulary);
    for part in path.components().skip(3) {
        private.add(what, &part.as_os_str().to_string_lossy(), vocabulary);
    }
    if let Some(user) = path.components().nth(2) {
        private.add("your user name", &user.as_os_str().to_string_lossy(), vocabulary);
    }
}

fn folders(db: &rusqlite::Connection) -> Vec<String> {
    let mut out = Vec::new();
    for sql in ["SELECT path FROM task_worktree_records", "SELECT dir FROM task_worktrees"] {
        let Ok(mut query) = db.prepare(sql) else { continue };
        let Ok(rows) = query.query_map([], |r| r.get::<_, String>(0)) else { continue };
        out.extend(rows.flatten());
    }
    out
}

fn strings_under(value: &Value, keys: &[&str], add: &mut dyn FnMut(&str)) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| strings_under(item, keys, add)),
        Value::Object(fields) => {
            for (key, value) in fields {
                if keys.contains(&key.as_str())
                    && let Some(text) = value.as_str()
                {
                    add(text);
                }
                strings_under(value, keys, add);
            }
        }
        _ => {}
    }
}
