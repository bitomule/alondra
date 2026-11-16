//! One test: an isolation alive changes the world of every thread of the process, so its steps
//! run in order.

use alondra_core::outside::{AsyncCommand, Command, Isolation, Mac, account_name, computer_name, connect, tcp_connect_timeout};
use std::io::ErrorKind;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn taken(leaks: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    std::mem::take(&mut *leaks.lock().unwrap())
}

#[test]
fn the_world_is_real_until_an_isolation_is_alive_and_then_nothing_gets_out() {
    assert!(Command::new("/usr/bin/true").status().unwrap().success(), "no isolation: the program runs");
    assert!(connect("example.com").is_ok(), "no isolation: the network is open");

    let leaks = Arc::new(Mutex::new(Vec::new()));
    let isolation = Isolation::new(leaks.clone(), Mac { account: "Ana Example".into(), computer: "Ana's Mac".into() });
    {
        let _entered = isolation.enter();
        let refused = Command::new("gh").args(["pr", "view", "7"]).output().unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::PermissionDenied);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let refused = runtime.block_on(AsyncCommand::new("git").arg("fetch").kill_on_drop(true).output()).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::PermissionDenied);
        assert_eq!(connect("api.github.com").unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert!(tcp_connect_timeout(&"203.0.113.7:443".parse().unwrap(), Duration::from_millis(1)).is_err());
        for loopback in ["127.0.0.1", "localhost", "[::1]"] {
            assert!(connect(loopback).is_ok(), "{loopback} is inside the world");
        }
        assert_eq!(account_name().as_deref(), Some("Ana Example"), "the world names its own account");
        assert_eq!(computer_name().as_deref(), Some("Ana's Mac"), "and its own Mac");
    }
    assert_eq!(taken(&leaks), ["process gh pr view 7", "process git fetch", "network api.github.com", "network 203.0.113.7"]);

    let elsewhere = std::thread::spawn(|| Command::new("claude").arg("agents").spawn().map(|_| ()).unwrap_err().kind()).join().unwrap();
    assert_eq!(elsewhere, ErrorKind::PermissionDenied, "a thread that entered no isolation is still inside one");
    assert_eq!(std::thread::spawn(account_name).join().unwrap().as_deref(), Some("Ana Example"));
    assert_eq!(taken(&leaks), ["process claude agents"]);

    drop(isolation);
    assert!(Command::new("/usr/bin/true").status().unwrap().success(), "the isolation is gone: the world is real again");
    assert!(taken(&leaks).is_empty());
}
