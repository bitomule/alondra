//! GitHub as a test sets it, for the images of a description.
use alondra_daemon_support::locks::Poisonable as _;
use super::*;
use std::collections::BTreeSet;
use std::sync::Mutex;

pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nshot";

/// GitHub as a test sets it: files by repository, reference and path, readable repositories, the
/// rendered description and what each signed link gives.
#[derive(Default)]
pub struct FakeGitHub(Mutex<Fake>);

#[derive(Default)]
pub struct Fake {
    pub files: BTreeMap<(String, String, String), Vec<u8>>,
    pub sizes: BTreeMap<String, u64>,
    pub repositories: BTreeSet<String>,
    pub html: String,
    pub signed: BTreeMap<String, Vec<u8>>,
    pub every_call: Option<Failure>,
    pub calls: Vec<String>,
}

impl FakeGitHub {
    pub fn with(files: &[(&str, &str, &str, &[u8])]) -> Self {
        let fake = Self::default();
        {
            let mut state = fake.0.lock_or_recover("a test's fixture");
            for (repository, reference, path, bytes) in files {
                state.files.insert(((*repository).into(), (*reference).into(), (*path).into()), bytes.to_vec());
                state.repositories.insert((*repository).into());
            }
            state.repositories.insert("o/r".into());
        }
        fake
    }

    pub fn set(&self, change: impl FnOnce(&mut Fake)) {
        change(&mut self.0.lock_or_recover("a test's fixture"));
    }

    pub fn calls(&self) -> Vec<String> {
        self.0.lock_or_recover("a test's fixture").calls.clone()
    }

    pub fn answer<T>(&self, call: String, answer: impl FnOnce(&Fake) -> Result<T, Failure>) -> Result<T, Failure> {
        let mut state = self.0.lock_or_recover("a test's fixture");
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
