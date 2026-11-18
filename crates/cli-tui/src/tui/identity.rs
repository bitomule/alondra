//! Which daemon this terminal talks to, said across the top whenever it is not plainly the
//! account's own: a sandbox, another home, or the real one reached from a test environment.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where this process looks for its daemon, and the account's own place to compare with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Here {
    /// The account's home from the user database, which a changed `HOME` does not move.
    pub account_home: PathBuf,
    pub home: PathBuf,
    pub alondra_home: PathBuf,
    pub port: u16,
    pub sandbox_env: bool,
}

/// What the daemon at that port says of itself in `/daemon`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub home: PathBuf,
    pub sandbox: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Not the account's daemon.
    Elsewhere,
    /// The account's real daemon, from an environment that looks like a test.
    Real,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub text: String,
    pub tone: Tone,
}

impl Here {
    pub fn of_this_process() -> Here {
        let config = crate::client::config();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        Here {
            account_home: account_home().unwrap_or_else(|| home.clone()),
            home,
            alondra_home: config.home.clone(),
            port: config.port,
            sandbox_env: config.sandbox,
        }
    }

    fn real_home(&self) -> PathBuf {
        self.account_home.join(".alondra")
    }

    fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.account_home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }

    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn looks_like_a_test(&self) -> bool {
        self.sandbox_env || !same(&self.home, &self.account_home) || !same(&self.alondra_home, &self.real_home())
    }
}

impl Answer {
    pub fn from_daemon(body: &Value) -> Option<Answer> {
        Some(Answer {
            home: PathBuf::from(body.get("home")?.as_str()?),
            sandbox: body.get("sandbox")?.as_bool()?,
        })
    }
}

/// `None` keeps the screen as it always was: the account's daemon from its own environment.
pub fn mark(here: &Here, daemon: Option<&Answer>) -> Option<Mark> {
    let address = here.address();
    match daemon {
        Some(answer) if answer.sandbox => Some(Mark {
            text: format!("SANDBOX · {} · {address}", here.shown(&answer.home)),
            tone: Tone::Elsewhere,
        }),
        Some(answer) if !same(&answer.home, &here.real_home()) => {
            let home = here.shown(&answer.home);
            Some(Mark { text: trf!("Test daemon · {home} · {address}", "Daemon de prueba · {home} · {address}"), tone: Tone::Elsewhere })
        }
        Some(_) if here.looks_like_a_test() => {
            let home = here.shown(&here.real_home());
            Some(Mark {
                text: trf!(
                    "REAL DAEMON · {home} · {address} — not a sandbox: everything done here is real",
                    "DAEMON REAL · {home} · {address} — no es un sandbox: todo lo que hagas aquí es real"
                ),
                tone: Tone::Real,
            })
        }
        Some(_) => None,
        None if here.looks_like_a_test() || here.port != alondra_core::DEFAULT_PORT => {
            let (home, alondra_home) = (here.shown(&here.home), here.shown(&here.alondra_home));
            Some(Mark {
                text: trf!(
                    "HOME {home} · ALONDRA_HOME {alondra_home} · {address} — the daemon has not said which it is",
                    "HOME {home} · ALONDRA_HOME {alondra_home} · {address} — el daemon aún no ha dicho cuál es"
                ),
                tone: Tone::Elsewhere,
            })
        }
        None => None,
    }
}

fn same(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut found = std::ptr::null_mut();
    let status = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
    if status != 0 || found.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Here {
        Here {
            account_home: "/Users/someone".into(),
            home: "/Users/someone".into(),
            alondra_home: "/Users/someone/.alondra".into(),
            port: alondra_core::DEFAULT_PORT,
            sandbox_env: false,
        }
    }

    fn answer(home: &str, sandbox: bool) -> Answer {
        Answer { home: home.into(), sandbox }
    }

    fn text(here: &Here, daemon: Option<&Answer>) -> Option<(String, Tone)> {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        mark(here, daemon).map(|m| (m.text, m.tone))
    }

    #[test]
    fn the_real_daemon_from_the_accounts_own_environment_shows_nothing() {
        assert_eq!(text(&account(), Some(&answer("/Users/someone/.alondra", false))), None);
        assert_eq!(text(&account(), None), None);
    }

    #[test]
    fn a_sandbox_daemon_says_so_with_its_home_and_address() {
        let here = Here { home: "/tmp/s/tui-home".into(), alondra_home: "/tmp/s/mac-1".into(), port: 48211, sandbox_env: true, ..account() };
        assert_eq!(
            text(&here, Some(&answer("/tmp/s/mac-1", true))),
            Some(("SANDBOX · /tmp/s/mac-1 · 127.0.0.1:48211".into(), Tone::Elsewhere))
        );
    }

    #[test]
    fn a_daemon_on_an_explicit_port_with_another_home_is_a_test_daemon() {
        let here = Here { alondra_home: "/Users/someone/scratch/a".into(), port: 48300, ..account() };
        assert_eq!(
            text(&here, Some(&answer("/Users/someone/scratch/a", false))),
            Some(("Test daemon · ~/scratch/a · 127.0.0.1:48300".into(), Tone::Elsewhere))
        );
    }

    #[test]
    fn a_temporary_home_that_reaches_the_real_daemon_is_warned_in_red() {
        let here = Here { home: "/tmp/fake".into(), alondra_home: "/tmp/fake/.alondra".into(), ..account() };
        assert_eq!(
            text(&here, Some(&answer("/Users/someone/.alondra", false))),
            Some((
                "REAL DAEMON · ~/.alondra · 127.0.0.1:47800 — not a sandbox: everything done here is real".into(),
                Tone::Real
            ))
        );
    }

    #[test]
    fn asking_for_a_sandbox_and_reaching_the_real_daemon_is_warned_in_red() {
        let here = Here { sandbox_env: true, ..account() };
        assert_eq!(text(&here, Some(&answer("/Users/someone/.alondra", false))).map(|(_, tone)| tone), Some(Tone::Real));
    }

    #[test]
    fn before_the_daemon_answers_a_test_environment_says_where_it_looks() {
        let here = Here { home: "/tmp/fake".into(), alondra_home: "/tmp/fake/.alondra".into(), port: 48211, ..account() };
        assert_eq!(
            text(&here, None),
            Some((
                "HOME /tmp/fake · ALONDRA_HOME /tmp/fake/.alondra · 127.0.0.1:48211 — the daemon has not said which it is".into(),
                Tone::Elsewhere
            ))
        );
    }

    #[test]
    fn a_daemon_that_does_not_say_who_it_is_is_read_as_no_answer() {
        assert_eq!(Answer::from_daemon(&serde_json::json!({"version": "0.3.4"})), None);
        assert_eq!(
            Answer::from_daemon(&serde_json::json!({"home": "/tmp/s/mac-1", "sandbox": true})),
            Some(answer("/tmp/s/mac-1", true))
        );
    }

    #[test]
    fn the_account_home_comes_from_the_user_database() {
        assert!(account_home().is_some_and(|home| home.is_absolute()));
    }
}
