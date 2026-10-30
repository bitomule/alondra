use super::*;
use crate::test_support::Directory;

fn repo() -> Directory {
    let dir = Directory::new();
    std::fs::create_dir_all(&*dir).unwrap();
    let out = Command::new("git").arg("-C").arg(&*dir).args(["init", "-q"]).output().unwrap();
    assert!(out.status.success());
    std::fs::write(dir.join(".gitignore"), ".env\nsecrets/\n").unwrap();
    dir
}

fn to(file: &str) -> SecretDestination {
    SecretDestination { file: file.into(), variable: "STRIPE_KEY".into() }
}

#[test]
fn the_secret_is_written_to_the_ignored_file_readable_by_the_user_alone() {
    let repo = repo();
    std::fs::write(repo.join(".env"), "OTHER=1\nSTRIPE_KEY=old\n").unwrap();
    let path = to(".env").write(&repo, "sk_live_123\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "OTHER=1\nSTRIPE_KEY=sk_live_123\n");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let nested = to("secrets/app.env").write(&repo, "with space").unwrap();
    assert_eq!(std::fs::read_to_string(nested).unwrap(), "STRIPE_KEY='with space'\n");
}

#[test]
fn a_destination_outside_the_repository_is_refused() {
    let repo = repo();
    for file in ["../.env", "/etc/.env", "a/../../.env", "", "./.env"] {
        assert!(to(file).shape_problem().is_some(), "{file}");
        assert!(to(file).write(&repo, "v").is_err(), "{file}");
    }
    assert!(!repo.parent().unwrap().join(".env").exists());
}

#[test]
fn a_file_git_does_not_ignore_is_refused() {
    let repo = repo();
    let refused = to("config.env").write(&repo, "sk_live_123").unwrap_err();
    assert!(refused.contains("does not ignore"), "{refused}");
    assert!(!repo.join("config.env").exists());
}

#[test]
fn a_link_is_never_followed() {
    let repo = repo();
    let outside = Directory::new();
    std::fs::create_dir_all(&*outside).unwrap();
    std::os::unix::fs::symlink(outside.join("target"), repo.join(".env")).unwrap();
    assert!(to(".env").write(&repo, "sk_live_123").unwrap_err().contains("is a link"));
    std::os::unix::fs::symlink(&*outside, repo.join("secrets")).unwrap();
    assert!(to("secrets/app.env").write(&repo, "sk_live_123").unwrap_err().contains("is a link"));
    assert_eq!(std::fs::read_dir(&*outside).unwrap().count(), 0);
}

#[test]
fn a_variable_must_be_a_name() {
    for variable in ["", "1KEY", "MY-KEY", "A B", "A=B"] {
        assert!(SecretDestination { file: ".env".into(), variable: variable.into() }.shape_problem().is_some(), "{variable}");
    }
}

#[test]
fn a_value_with_quotes_or_lines_reads_back_whole() {
    assert_eq!(quoted("plain_1.2"), "plain_1.2");
    assert_eq!(quoted("it's\nmine $HOME"), "\"it's\\nmine \\$HOME\"");
}
