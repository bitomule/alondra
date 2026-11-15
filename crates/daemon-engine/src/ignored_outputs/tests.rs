use super::*;
use crate::test_support::Directory;
use crate::worktree::testing::{git, task_worktree};

const KB: u64 = 1024;
const SMALL: Limits = Limits { min: 8 * KB, max: 1024 * KB };
const LOW: Space = Space { free: 1_000_000_000, purgeable: 0, total: 500_000_000_000 };
const ROOMY: Space = Space { free: 400_000_000_000, purgeable: 0, total: 500_000_000_000 };

fn worktree(root: &Directory) -> PathBuf {
    let dir = task_worktree(root, "idle");
    std::fs::write(dir.join(".gitignore"), "target/\nbuild/\nout/\n.env\nnotes.local\n").unwrap();
    std::fs::write(dir.join("tracked.txt"), "one").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "tracked"]);
    dir
}

fn fill(dir: &Path, name: &str, kilobytes: usize) {
    std::fs::create_dir_all(dir.join(name)).unwrap();
    std::fs::write(dir.join(name).join("artifact.bin"), vec![7u8; kilobytes * KB as usize]).unwrap();
}

fn low_while_present(dir: &Path, entry: &'static str) -> impl Fn(&Path) -> Option<Space> {
    let present = dir.join(entry);
    move |_| Some(if present.exists() { LOW } else { ROOMY })
}

#[test]
fn a_low_disk_empties_the_ignored_folders_and_touches_nothing_else() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 64);
    std::fs::write(dir.join(".env"), "TOKEN=1").unwrap();
    std::fs::write(dir.join("notes.local"), "mine").unwrap();
    std::fs::write(dir.join("tracked.txt"), "edited").unwrap();
    std::fs::write(dir.join("draft.txt"), "not ignored").unwrap();
    let freed = free(&[dir.clone()], SMALL, low_while_present(&dir, "target/artifact.bin"));
    assert_eq!(freed.len(), 1);
    assert_eq!(freed[0].entries, vec![("target".to_owned(), freed[0].bytes())]);
    assert!(freed[0].bytes() >= 64 * KB);
    assert!(!dir.join("target/artifact.bin").exists());
    assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "TOKEN=1");
    assert_eq!(std::fs::read_to_string(dir.join("notes.local")).unwrap(), "mine");
    assert_eq!(std::fs::read_to_string(dir.join("tracked.txt")).unwrap(), "edited");
    assert_eq!(std::fs::read_to_string(dir.join("draft.txt")).unwrap(), "not ignored");
}

#[test]
fn a_disk_with_room_frees_nothing() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 64);
    assert!(free(&[dir.clone()], SMALL, |_| Some(ROOMY)).is_empty());
    assert!(free(&[dir.clone()], SMALL, |_| None).is_empty());
    assert!(dir.join("target/artifact.bin").exists());
}

#[test]
fn a_folder_holding_a_secret_or_a_repository_of_its_own_stays() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 64);
    std::fs::write(dir.join("target/.env.local"), "KEY=1").unwrap();
    fill(&dir, "build", 64);
    std::fs::write(dir.join("build/signing.p12"), "x").unwrap();
    fill(&dir, "out", 64);
    git(&dir.join("out"), &["init", "-q"]);
    assert!(free(&[dir.clone()], SMALL, |_| Some(LOW)).is_empty());
    for kept in ["target/artifact.bin", "target/.env.local", "build/signing.p12", "out/artifact.bin"] {
        assert!(dir.join(kept).exists(), "{kept}");
    }
}

#[test]
fn folders_under_the_minimum_or_over_the_maximum_stay() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 1);
    fill(&dir, "build", 2048);
    assert!(free(&[dir.clone()], SMALL, |_| Some(LOW)).is_empty());
    assert!(dir.join("target/artifact.bin").exists() && dir.join("build/artifact.bin").exists());
}

#[test]
fn it_stops_once_the_disk_is_past_its_recovery_line_and_takes_the_largest_first() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 256);
    fill(&dir, "build", 64);
    let freed = free(&[dir.clone()], SMALL, low_while_present(&dir, "target/artifact.bin"));
    assert_eq!(freed[0].entries.iter().map(|(entry, _)| entry.as_str()).collect::<Vec<_>>(), ["target"]);
    assert!(dir.join("build/artifact.bin").exists());
}

#[test]
fn a_folder_git_tracks_files_in_keeps_them() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 64);
    std::fs::write(dir.join("target/kept.txt"), "tracked on purpose").unwrap();
    git(&dir, &["add", "-f", "target/kept.txt"]);
    git(&dir, &["commit", "-q", "-m", "force"]);
    free(&[dir.clone()], SMALL, |_| Some(LOW));
    assert!(std::fs::read_to_string(dir.join("target/kept.txt")).is_ok());
}

#[test]
fn what_looks_like_a_secret_is_recognised_by_name() {
    for name in [".env", ".env.production", ".envrc", "AuthKey.p8", "cert.PEM", "id_rsa", "credentials", ".npmrc", "x.mobileprovision"] {
        assert!(protected(name), "{name}");
    }
    for name in ["artifact.bin", "environment.rs", "keyboard.o", "target"] {
        assert!(!protected(name), "{name}");
    }
}

#[test]
fn the_output_base_of_a_running_bazel_server_stays_and_one_without_it_goes() {
    let root = Directory::new();
    let dir = worktree(&root);
    fill(&dir, "target", 64);
    std::fs::create_dir_all(dir.join("target/server")).unwrap();
    std::fs::write(dir.join("target/server/server.pid.txt"), std::process::id().to_string()).unwrap();
    assert!(free(&[dir.clone()], SMALL, |_| Some(LOW)).is_empty());
    assert!(dir.join("target/artifact.bin").exists());
    std::fs::write(dir.join("target/server/server.pid.txt"), "2147483646").unwrap();
    let freed = free(&[dir.clone()], SMALL, low_while_present(&dir, "target/artifact.bin"));
    assert_eq!(freed.len(), 1);
    assert!(!dir.join("target/artifact.bin").exists());
}
