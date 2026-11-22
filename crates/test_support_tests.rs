mod test_support;
use test_support::Directory;

#[test]
fn removes_written_files_when_the_scope_ends() {
    let path;
    {
        let directory = Directory::new();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("fixture"), b"test data").unwrap();
        path = directory.to_path_buf();
    }
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn removes_written_files_when_a_test_panics() {
    let directory = Directory::new();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("fixture"), b"test data").unwrap();
    let path = directory.to_path_buf();
    let result = std::panic::catch_unwind(move || {
        let _directory = directory;
        panic!("failed test body");
    });
    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn a_socket_fixture_lives_in_a_short_temporary_folder_and_is_refused_in_a_long_one() {
    let short = std::fs::canonicalize(test_support::short_temporary()).unwrap().join(format!("sr-{}", std::process::id()));
    std::fs::create_dir(&short).unwrap();
    let long = short.join("a-folder-too-long-to-hold-the-socket-of-a-host");
    std::fs::create_dir(&long).unwrap();
    let kept = test_support::socket_root(&short);
    let refused = std::panic::catch_unwind(|| test_support::socket_root(&long));
    std::fs::remove_dir_all(&short).unwrap();
    assert_eq!(kept, short);
    assert!(refused.is_err(), "a long folder is never replaced by a shared one");
}

#[cfg(unix)]
fn read_only_tree(directory: &Directory) {
    use std::os::unix::fs::PermissionsExt;
    let locked = directory.join("cache/external/rules");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("BUILD"), b"fixture").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(directory.join("cache"), std::fs::Permissions::from_mode(0o500)).unwrap();
}

#[cfg(unix)]
#[test]
fn removes_read_only_folders_when_the_scope_ends() {
    let path;
    {
        let directory = Directory::new();
        read_only_tree(&directory);
        path = directory.to_path_buf();
    }
    assert!(!path.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn removes_read_only_folders_when_a_test_panics() {
    let directory = Directory::new();
    read_only_tree(&directory);
    let path = directory.to_path_buf();
    let result = std::panic::catch_unwind(move || {
        let _directory = directory;
        panic!("failed test body");
    });
    assert!(result.is_err());
    assert!(!path.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn a_tests_temporary_folder_holds_a_socket_and_has_no_checkout_above_it() {
    let temporary = std::fs::canonicalize(std::env::var_os("TMPDIR").expect("run through Bazel")).unwrap();
    assert_eq!(test_support::socket_root(&temporary), temporary, "tempfile fixtures bind sockets too");
    let repository = temporary.ancestors().find(|folder| folder.join(".git").exists());
    assert_eq!(repository, None, "a git or project lookup from a fixture would find a real checkout");
}
