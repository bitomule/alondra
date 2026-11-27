//! A description's images as the Mac's real `gh` answers for them: GitHub's HTTP status, never its
//! words, decides whether a failed read is worth asking again.
use alondra_engine_ports::pr_images::{Target, bring};
use alondra_view_model::pr_images::{ImageProblem, Shown};
use crate::test_support::Directory;
use std::collections::BTreeMap;
use std::path::Path;

fn target() -> Target<'static> {
    Target { host: "github.com", repository: "o/r", number: 87, head: "8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9" }
}

fn problem(shown: &Shown) -> ImageProblem {
    let Shown::Unavailable { problem, .. } = shown else { panic!("brought: {shown:?}") };
    problem.clone()
}

/// A `gh` that answers every call with `stdout` on its standard output, `stderr` on its error
/// output and exits with `code`.
fn answering_gh(dir: &Path, stdout: &str, stderr: &str, code: i32) -> alondra_github_gh::GhCli {
    use std::os::unix::fs::PermissionsExt;
    let program = dir.join("gh");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '{stdout}'\nprintf '%s\\n' '{stderr}' >&2\nexit {code}\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    alondra_github_gh::GhCli::at(program).without_limits()
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
