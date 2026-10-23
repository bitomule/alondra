use super::*;
use crate::test_support::Directory;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nshot";

struct Fixture {
    _dir: Directory,
    cwd: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let dir = Directory::new();
    let (cwd, home) = (dir.join("worktree"), dir.join("home"));
    std::fs::create_dir_all(cwd.join("docs")).unwrap();
    std::fs::create_dir_all(home.join("files")).unwrap();
    std::fs::write(cwd.join("docs/shot.png"), PNG).unwrap();
    Fixture { _dir: dir, cwd, home }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn route() -> String {
    format!("/files?path=files/images/{}.png", digest(PNG))
}

#[test]
fn a_relative_image_is_copied_by_its_sha256_and_the_document_points_at_the_copy() {
    let f = fixture();
    let (carried, skipped) = carry("Look: ![main screen](docs/shot.png \"Main\")\n", &f.cwd, &f.home);
    assert_eq!(carried, format!("Look: ![main screen]({} \"Main\")\n", route()));
    assert!(skipped.is_empty());
    let copy = f.home.join(stored_image(&route()).unwrap());
    assert_eq!(std::fs::read(copy).unwrap(), PNG);
}

#[test]
fn html_and_reference_images_are_carried_and_everything_else_stays() {
    let f = fixture();
    let doc = "<p align=center><img alt='logo' src=docs/shot.png width=80/></p>\n\n\
               Inline <img src=\"docs/shot.png\"> too, and ![ref][s].\n\n\
               [s]: docs/shot.png\n\n\
               ```\n![code](docs/shot.png)\n```\n\n\
               [a link](docs/shot.png) ![web](https://x.dev/a.png) ![abs](/etc/a.png)\n";
    let (carried, skipped) = carry(doc, &f.cwd, &f.home);
    let r = route();
    assert_eq!(
        carried,
        format!(
            "<p align=center><img alt='logo' src={r} width=80/></p>\n\n\
             Inline <img src=\"{r}\"> too, and ![ref][s].\n\n\
             [s]: {r}\n\n\
             ```\n![code](docs/shot.png)\n```\n\n\
             [a link](docs/shot.png) ![web](https://x.dev/a.png) ![abs](/etc/a.png)\n"
        )
    );
    assert!(skipped.is_empty());
}

#[test]
fn an_image_outside_the_worktree_missing_too_large_or_not_an_image_is_left_as_it_was() {
    let f = fixture();
    std::fs::write(f.cwd.parent().unwrap().join("secret.png"), PNG).unwrap();
    std::os::unix::fs::symlink(f.cwd.parent().unwrap().join("secret.png"), f.cwd.join("docs/link.png")).unwrap();
    std::fs::write(f.cwd.join("docs/huge.png"), vec![0u8; IMAGE_MAX_BYTES as usize + 1]).unwrap();
    std::fs::write(f.cwd.join(".env"), "TOKEN=x").unwrap();
    let doc = "![a](../secret.png) ![b](docs/link.png) ![c](docs/gone.png) ![d](docs/huge.png) ![e](.env)";
    let (carried, skipped) = carry(doc, &f.cwd, &f.home);
    assert_eq!(carried, doc);
    assert_eq!(
        skipped,
        vec![
            ("../secret.png".to_owned(), Skipped::Outside),
            ("docs/link.png".to_owned(), Skipped::Outside),
            ("docs/gone.png".to_owned(), Skipped::Missing),
            ("docs/huge.png".to_owned(), Skipped::TooLarge),
            (".env".to_owned(), Skipped::NotAnImage),
        ]
    );
    assert!(!f.home.join("files/images").exists(), "nothing is copied");
    let notice = notice(&skipped).unwrap();
    assert!(notice.contains("../secret.png (it is outside your working folder)"), "{notice}");
}

#[test]
fn a_document_with_relative_images_becomes_a_new_stored_file_with_its_own_sha256() {
    let f = fixture();
    let original = "# Plan\n\n![shot](docs/shot.png)\n";
    let stored = f.home.join("files/plan.md");
    std::fs::write(&stored, original).unwrap();
    let (path, sha, skipped) = carry_document("files/plan.md", &digest(original.as_bytes()), &f.cwd, &f.home).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, format!("# Plan\n\n![shot]({})\n", route()));
    assert_eq!(sha, digest(text.as_bytes()));
    assert!(path.starts_with(f.home.join("files")) && skipped.is_empty());
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), original, "the author's file is untouched");
    assert!(carry_document("files/plan.md", &"0".repeat(64), &f.cwd, &f.home).is_none(), "a wrong SHA256 is the daemon's to refuse");
}

#[test]
fn only_a_route_alondra_writes_names_a_stored_image() {
    assert_eq!(stored_image(&route()), Some(&route()["/files?path=".len()..]));
    for url in ["/files?path=files/images/../x.png", "/files?path=/etc/hosts", "docs/shot.png", "/files?path=files/images/abc.png"] {
        assert_eq!(stored_image(url), None, "{url}");
    }
}
