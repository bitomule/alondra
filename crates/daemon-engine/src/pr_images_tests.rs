use super::*;
use crate::test_support::Directory;
use std::collections::BTreeSet;
use std::sync::Mutex;

pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nshot";
const JPG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, b'j', b'p', b'g'];
const HEAD: &str = "8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9";
const UUID: &str = "3bae41ae-ce20-495d-bb1b-ad4229a6e245";

/// GitHub as a test sets it: files by repository, reference and path, readable repositories, the
/// rendered description and what each signed link gives.
#[derive(Default)]
pub struct FakeGitHub(Mutex<Fake>);

#[derive(Default)]
pub(crate) struct Fake {
    files: BTreeMap<(String, String, String), Vec<u8>>,
    sizes: BTreeMap<String, u64>,
    repositories: BTreeSet<String>,
    html: String,
    signed: BTreeMap<String, Vec<u8>>,
    every_call: Option<Failure>,
    calls: Vec<String>,
}

impl FakeGitHub {
    pub fn with(files: &[(&str, &str, &str, &[u8])]) -> Self {
        let fake = Self::default();
        {
            let mut state = fake.0.lock().unwrap();
            for (repository, reference, path, bytes) in files {
                state.files.insert(((*repository).into(), (*reference).into(), (*path).into()), bytes.to_vec());
                state.repositories.insert((*repository).into());
            }
            state.repositories.insert("o/r".into());
        }
        fake
    }

    fn set(&self, change: impl FnOnce(&mut Fake)) {
        change(&mut self.0.lock().unwrap());
    }

    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    fn answer<T>(&self, call: String, answer: impl FnOnce(&Fake) -> Result<T, Failure>) -> Result<T, Failure> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(call);
        if let Some(failure) = state.every_call.clone() {
            return Err(failure);
        }
        answer(&state)
    }
}

impl GitHubImages for FakeGitHub {
    fn size<'a>(&'a self, _: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, u64> {
        let answer = self.answer(format!("size {repository} {reference} {path}"), |state| {
            let file = state.files.get(&(repository.into(), reference.into(), path.into())).ok_or(Failure::NotFound)?;
            Ok(state.sizes.get(path).copied().unwrap_or(file.len() as u64))
        });
        Box::pin(async move { answer })
    }

    fn file<'a>(&'a self, _: &'a str, repository: &'a str, path: &'a str, reference: &'a str) -> Fetch<'a, Vec<u8>> {
        let answer = self.answer(format!("file {repository} {reference} {path}"), |state| {
            state.files.get(&(repository.into(), reference.into(), path.into())).cloned().ok_or(Failure::NotFound)
        });
        Box::pin(async move { answer })
    }

    fn repository<'a>(&'a self, _: &'a str, repository: &'a str) -> Fetch<'a, ()> {
        let answer = self.answer(format!("repository {repository}"), |state| {
            state.repositories.contains(repository).then_some(()).ok_or(Failure::NotFound)
        });
        Box::pin(async move { answer })
    }

    fn body_html<'a>(&'a self, _: &'a str, repository: &'a str, number: u64) -> Fetch<'a, String> {
        let answer = self.answer(format!("body_html {repository} {number}"), |state| Ok(state.html.clone()));
        Box::pin(async move { answer })
    }

    fn download<'a>(&'a self, url: &'a str) -> Fetch<'a, Vec<u8>> {
        let answer = self.answer(format!("download {url}"), |state| state.signed.get(url).cloned().ok_or(Failure::NotFound));
        Box::pin(async move { answer })
    }
}

fn target() -> Target<'static> {
    Target { host: "github.com", repository: "o/r", number: 87, head: HEAD }
}

fn signed(uuid: &str) -> String {
    format!("https://{SIGNED_HOST}/42989/589621123-{uuid}.png?jwt=eyJ0eXAi&X-Amz-Expires=300")
}

async fn brought(github: &FakeGitHub, home: &Path, body: &str) -> Brought {
    bring(github, home, &target(), body, &BTreeMap::new()).await
}

fn stored(home: &Path, shown: &Shown) -> Vec<u8> {
    let Shown::Stored { route } = shown else { panic!("not brought: {shown:?}") };
    std::fs::read(home.join(markdown::stored_image(route).expect("a stored copy"))).unwrap()
}

fn problem(shown: &Shown) -> ImageProblem {
    let Shown::Unavailable { problem, .. } = shown else { panic!("brought: {shown:?}") };
    problem.clone()
}

#[tokio::test]
async fn every_way_a_description_cites_a_file_of_its_repository_is_brought_from_github() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[
        ("o/r", HEAD, "docs/pr/a.jpg", JPG),
        ("o/r", HEAD, "docs/b.png", PNG),
        ("o/r", "main", "shots/c.png", PNG),
        ("other/repo", "feature/duo", "d.png", PNG),
    ]);
    let body = format!(
        "![a](https://github.com/o/r/blob/{HEAD}/docs/pr/a.jpg?raw=true)\n\
         ![b](docs/b.png)\n\
         <img width=\"300\" src=\"https://github.com/o/r/raw/main/shots/c.png\">\n\
         ![d](https://raw.githubusercontent.com/other/repo/feature/duo/d.png)\n"
    );
    let brought = brought(&github, &dir, &body).await;
    assert_eq!(brought.images.len(), 4);
    assert!(!brought.retry);
    assert_eq!(stored(&dir, &brought.images[0].shown), JPG);
    for image in &brought.images[1..] {
        assert_eq!(stored(&dir, &image.shown), PNG);
    }
    assert!(brought.images[2].html);
}

#[tokio::test]
async fn an_uploaded_image_is_brought_through_the_signed_link_of_the_rendered_description() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[]);
    github.set(|state| {
        state.html = format!("<p><a href=\"{0}\"><img alt=\"Image\" src=\"{0}\"></a></p>", signed(UUID).replace('&', "&amp;"));
        state.signed.insert(signed(UUID), PNG.to_vec());
    });
    let body = format!("![Image](https://github.com/user-attachments/assets/{UUID})\n<img src=\"https://github.com/user-attachments/assets/{UUID}\">\n");
    let brought = brought(&github, &dir, &body).await;
    assert_eq!(brought.images.len(), 2);
    assert_eq!(stored(&dir, &brought.images[0].shown), PNG);
    assert_eq!(brought.images[0].shown, brought.images[1].shown);
    let calls = github.calls();
    assert_eq!(calls.iter().filter(|call| call.starts_with("body_html")).count(), 1, "{calls:?}");
    assert_eq!(calls.iter().filter(|call| call.starts_with("download")).count(), 1, "{calls:?}");
}

#[tokio::test]
async fn an_upload_github_no_longer_renders_says_so() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[]);
    let brought = brought(&github, &dir, &format!("![](https://github.com/user-attachments/assets/{UUID})")).await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::Gone);
}

#[tokio::test]
async fn an_image_missing_from_the_branch_says_at_which_commit_and_links_to_github() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "docs/a.png", PNG)]);
    let body = format!("![](https://github.com/o/r/blob/{HEAD}/docs/gone.jpg?raw=true) ![](docs/gone.png)");
    let brought = brought(&github, &dir, &body).await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::NotInBranch { reference: HEAD.into() });
    let Shown::Unavailable { link, problem } = &brought.images[1].shown else { panic!() };
    assert_eq!(problem, &ImageProblem::NotInBranch { reference: HEAD.into() });
    assert_eq!(link, &format!("https://github.com/o/r/blob/{HEAD}/docs/gone.png"));
}

#[tokio::test]
async fn a_repository_github_does_not_show_and_a_signed_out_gh_say_so() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[]);
    let body = format!("![](https://github.com/secret/repo/blob/{HEAD}/a.png)");
    let brought = brought(&github, &dir, &body).await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::NoAccess { repository: "secret/repo".into() });
    github.set(|state| state.every_call = Some(Failure::SignedOut));
    let brought = super::bring(&github, &dir, &target(), &body, &BTreeMap::new()).await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::SignedOut);
    assert!(!brought.retry);
}

#[tokio::test]
async fn an_image_larger_than_the_limit_is_never_downloaded() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "huge.png", PNG)]);
    github.set(|state| {
        state.sizes.insert("huge.png".into(), 23_000_000);
    });
    let brought = brought(&github, &dir, "![](huge.png)").await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::TooLarge { bytes: 23_000_000 });
    assert!(!github.calls().iter().any(|call| call.starts_with("file")), "{:?}", github.calls());
    assert!(!dir.join(markdown::FOLDER).exists());
}

#[tokio::test]
async fn bytes_that_are_not_an_image_are_not_kept() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "page.png", b"<!DOCTYPE html><html>Sign in</html>")]);
    let brought = brought(&github, &dir, "![](page.png)").await;
    assert_eq!(problem(&brought.images[0].shown), ImageProblem::NotAnImage);
    assert!(!dir.join(markdown::FOLDER).exists());
}

#[tokio::test]
async fn a_passing_failure_is_asked_again_and_what_was_brought_is_not() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "a.png", PNG)]);
    github.set(|state| state.every_call = Some(Failure::Transient));
    let first = brought(&github, &dir, "![](a.png)").await;
    assert_eq!(problem(&first.images[0].shown), ImageProblem::Retrying);
    assert!(first.retry);
    assert!(lasting(&target(), &first.images, true).is_empty());
    github.set(|state| state.every_call = None);
    let second = brought(&github, &dir, "![](a.png)").await;
    assert_eq!(stored(&dir, &second.images[0].shown), PNG);
    let calls = github.calls().len();
    let third = bring(&github, &dir, &target(), "![](a.png)", &lasting(&target(), &second.images, true)).await;
    assert_eq!(third, Brought { retry: false, ..second });
    assert_eq!(github.calls().len(), calls);
}

#[tokio::test]
async fn a_new_head_asks_again_for_what_the_branch_can_change_and_keeps_commits_and_uploads() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "a.png", PNG), ("o/r", HEAD, "b.png", PNG)]);
    github.set(|state| {
        state.html = format!("<img src=\"{}\">", signed(UUID));
        state.signed.insert(signed(UUID), PNG.to_vec());
    });
    let body = format!("![](a.png) ![](https://github.com/o/r/blob/{HEAD}/b.png) ![](https://github.com/user-attachments/assets/{UUID})");
    let first = brought(&github, &dir, &body).await;
    let kept = lasting(&target(), &first.images, false);
    assert_eq!(kept.keys().cloned().collect::<Vec<_>>(), [format!("https://github.com/o/r/blob/{HEAD}/b.png"), format!("https://github.com/user-attachments/assets/{UUID}")]);
}

#[tokio::test]
async fn images_elsewhere_are_left_for_the_clients_and_the_same_image_is_brought_once() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", HEAD, "a.png", PNG)]);
    let body = "![](https://alondra.dev/logo.png) ![](http://github.com/o/r/blob/main/a.png) ![](/docs/a.png) ![](a.png) ![again](a.png)";
    let brought = brought(&github, &dir, body).await;
    assert_eq!(brought.images.iter().map(|image| image.url.as_str()).collect::<Vec<_>>(), ["a.png", "a.png"]);
    assert_eq!(github.calls().iter().filter(|call| call.starts_with("file")).count(), 1);
}

#[tokio::test]
async fn a_branch_with_a_slash_in_a_link_is_read_by_trying_where_the_branch_ends() {
    let dir = Directory::new();
    let github = FakeGitHub::with(&[("o/r", "feature/duo", "docs/a.png", PNG)]);
    let brought = brought(&github, &dir, "![](https://github.com/o/r/blob/feature/duo/docs/a.png?raw=true)").await;
    assert_eq!(stored(&dir, &brought.images[0].shown), PNG);
}

#[tokio::test]
async fn more_images_than_the_limit_say_so_and_are_not_asked_for() {
    let dir = Directory::new();
    let files: Vec<(String, Vec<u8>)> = (0..=MOST).map(|n| (format!("{n}.png"), PNG.to_vec())).collect();
    let github = FakeGitHub::with(&files.iter().map(|(path, bytes)| ("o/r", HEAD, path.as_str(), bytes.as_slice())).collect::<Vec<_>>());
    let body: String = files.iter().map(|(path, _)| format!("![]({path})\n\n")).collect();
    let brought = brought(&github, &dir, &body).await;
    assert_eq!(brought.images.iter().filter(|image| matches!(image.shown, Shown::Stored { .. })).count(), MOST);
    assert_eq!(problem(&brought.images[MOST].shown), ImageProblem::TooMany);
}

#[test]
fn signed_links_are_read_from_the_rendered_description_by_the_uploads_uuid() {
    let html = format!("<a href=\"{0}\"><img src=\"{0}\" alt=\"Image\"></a><img src=\"https://camo.githubusercontent.com/x\">", signed(UUID).replace('&', "&amp;"));
    assert_eq!(signed_links(&html), BTreeMap::from([(UUID.to_owned(), signed(UUID))]));
}

/// A `gh` that answers every call with `stdout` on its standard output, `stderr` on its error
/// output and exits with `code`.
fn answering_gh(dir: &Path, stdout: &str, stderr: &str, code: i32) -> Gh {
    use std::os::unix::fs::PermissionsExt;
    let program = dir.join("gh");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '{stdout}'\nprintf '%s\\n' '{stderr}' >&2\nexit {code}\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    Gh::at(program)
}

#[tokio::test]
async fn githubs_http_status_decides_a_failed_read_and_its_words_never_do() {
    let cases = [
        ("HTTP/2.0 502 Bad Gateway\\r\\nContent-Type: application/json\\r\\n\\r\\n{\"message\":\"upstream not found\"}",
            "gh: Server Error: upstream not found (HTTP 502)", 1, ImageProblem::Retrying),
        ("HTTP/2.0 401 Unauthorized\\r\\n\\r\\n{\"message\":\"Requires authentication\"}", "gh: Requires authentication (HTTP 401)", 1,
            ImageProblem::SignedOut),
        ("", "To get started with GitHub CLI, please run:  gh auth login", 4, ImageProblem::SignedOut),
        ("", "error connecting to api.github.com: not found", 1, ImageProblem::Retrying),
    ];
    for (stdout, stderr, code, expected) in cases {
        let dir = Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let gh = answering_gh(&dir, stdout, stderr, code);
        let brought = bring(&gh, &dir, &target(), "![](a.png)", &BTreeMap::new()).await;
        assert_eq!(problem(&brought.images[0].shown), expected, "{stderr}");
    }
}
