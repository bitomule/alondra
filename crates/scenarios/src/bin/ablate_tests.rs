use super::*;
use std::os::unix::fs::PermissionsExt;

const OLD: &[u8] = b"old line\r\nno newline at the end \xff";
const FIXED: &[u8] = b"fixed line\n";

fn base(file: &str) -> Option<Vec<u8>> {
    (file == "kept.txt").then(|| OLD.to_vec())
}

fn workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("kept.txt"), FIXED).unwrap();
    std::fs::create_dir(workspace.path().join("src")).unwrap();
    std::fs::write(workspace.path().join("src/added.rs"), b"fn added() {}").unwrap();
    workspace
}

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_file_goes_back_to_the_base_byte_for_byte_and_comes_back_as_the_fix_left_it() {
    let workspace = workspace();
    let path = workspace.path().join("kept.txt");
    let reverted = Reverted::apply(workspace.path(), &files(&["kept.txt"]), base).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), OLD);
    drop(reverted);
    assert_eq!(std::fs::read(&path).unwrap(), FIXED);
}

#[test]
fn a_file_the_fix_added_is_taken_away_and_comes_back() {
    let workspace = workspace();
    let path = workspace.path().join("src/added.rs");
    let reverted = Reverted::apply(workspace.path(), &files(&["src/added.rs", "kept.txt"]), base).unwrap();
    assert!(!path.exists());
    drop(reverted);
    assert_eq!(std::fs::read(&path).unwrap(), b"fn added() {}");
}

#[test]
fn a_file_the_fix_deleted_comes_back_from_the_base_and_goes_again() {
    let workspace = workspace();
    let path = workspace.path().join("kept.txt");
    std::fs::remove_file(&path).unwrap();
    let reverted = Reverted::apply(workspace.path(), &files(&["kept.txt"]), base).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), OLD);
    drop(reverted);
    assert!(!path.exists());
}

#[test]
fn the_files_come_back_when_the_run_panics() {
    let workspace = workspace();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _reverted = Reverted::apply(workspace.path(), &files(&["src/added.rs", "kept.txt"]), base).unwrap();
        panic!("the run failed");
    }));
    assert!(outcome.is_err());
    assert_eq!(std::fs::read(workspace.path().join("kept.txt")).unwrap(), FIXED);
    assert_eq!(std::fs::read(workspace.path().join("src/added.rs")).unwrap(), b"fn added() {}");
}

#[test]
fn a_file_neither_side_has_refuses_the_run_before_touching_any_file() {
    let workspace = workspace();
    let outcome = Reverted::apply(workspace.path(), &files(&["kept.txt", "typo.txt"]), base);
    assert!(outcome.err().is_some_and(|message| message.contains("typo.txt")));
    assert_eq!(std::fs::read(workspace.path().join("kept.txt")).unwrap(), FIXED);
}

#[test]
fn a_path_outside_the_repository_is_refused() {
    let workspace = workspace();
    for outside in ["../kept.txt", "/etc/hosts", "src/../kept.txt"] {
        assert!(Reverted::apply(workspace.path(), &files(&[outside]), |_| Some(OLD.to_vec())).is_err(), "{outside}");
    }
}

#[test]
fn the_permissions_of_a_file_come_back_too() {
    let workspace = workspace();
    let path = workspace.path().join("kept.txt");
    std::fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
    let reverted = Reverted::apply(workspace.path(), &files(&["kept.txt"]), base).unwrap();
    std::fs::remove_file(&path).ok();
    drop(reverted);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
}
