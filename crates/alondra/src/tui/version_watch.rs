//! A TUI left open across an update keeps its old code: this decides, from what the daemon says
//! about itself, whether to restart on the new binary, warn, or do nothing.

use alondra_core::update::newer;

pub const IDLE_MS: i64 = 10_000;
pub const COUNTDOWN_MS: i64 = 3_000;
/// A daemon restarts on the new binary within a minute; until then an older one is expected.
pub const OLDER_GRACE_MS: i64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Same,
    ServiceNewer,
    ServiceOlder,
}

/// Versions that do not parse are treated as the same: a development daemon never forces anything.
pub fn compare(cli: &str, service: &str) -> Side {
    if newer(service, cli) {
        Side::ServiceNewer
    } else if newer(cli, service) {
        Side::ServiceOlder
    } else {
        Side::Same
    }
}

/// What `<stable path> --version --json` said about the binary a restart would run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disk {
    Unchecked,
    Version(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Inputs<'a> {
    pub cli: &'a str,
    pub service: Option<&'a str>,
    /// The other Mac this instance talks to; restarting here changes nothing for it.
    pub remote: bool,
    pub disk: &'a Disk,
    pub idle_ms: i64,
    /// An action sent without an answer yet, or a file still uploading.
    pub busy: bool,
    /// A confirmation waiting for the person.
    pub confirming: bool,
    /// The service version this process was already restarted for.
    pub restarted_for: Option<&'a str>,
    /// Esc during the countdown for this version.
    pub stayed: bool,
    /// R pressed for this version.
    pub asked: bool,
    pub countdown_left_ms: Option<i64>,
    pub older_for_ms: i64,
    /// The header already says the installed version did not start.
    pub rejected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    CheckDisk,
    Countdown,
    Restart,
    Warn(Warning),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Warning {
    Updated,
    WaitAction,
    DiskOld,
    ServiceOld,
    RemoteNewer,
}

pub fn decide(i: &Inputs<'_>) -> Decision {
    let Some(service) = i.service else {
        return Decision::Nothing;
    };
    match compare(i.cli, service) {
        Side::Same => Decision::Nothing,
        Side::ServiceOlder if i.rejected || i.older_for_ms < OLDER_GRACE_MS => Decision::Nothing,
        Side::ServiceOlder => Decision::Warn(Warning::ServiceOld),
        Side::ServiceNewer if i.remote => Decision::Warn(Warning::RemoteNewer),
        Side::ServiceNewer => match i.disk {
            Disk::Unchecked => Decision::CheckDisk,
            Disk::Failed(_) => Decision::Warn(Warning::DiskOld),
            Disk::Version(on_disk) if on_disk != service => Decision::Warn(Warning::DiskOld),
            Disk::Version(_) if i.restarted_for == Some(service) => Decision::Warn(Warning::Updated),
            Disk::Version(_) if i.asked && (i.busy || i.confirming) => Decision::Warn(Warning::WaitAction),
            Disk::Version(_) if i.asked => Decision::Restart,
            Disk::Version(_) if i.stayed || i.busy || i.confirming || i.idle_ms < IDLE_MS => {
                Decision::Warn(Warning::Updated)
            }
            Disk::Version(_) => match i.countdown_left_ms {
                Some(left) if left <= 0 => Decision::Restart,
                _ => Decision::Countdown,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: &str = "0.2.50";
    const NEW: &str = "0.2.53";

    fn safe(disk: &Disk) -> Inputs<'_> {
        Inputs {
            cli: CLI,
            service: Some(NEW),
            remote: false,
            disk,
            idle_ms: IDLE_MS,
            busy: false,
            confirming: false,
            restarted_for: None,
            stayed: false,
            asked: false,
            countdown_left_ms: None,
            older_for_ms: 0,
            rejected: false,
        }
    }

    #[test]
    fn versions_compare_by_number_and_unparsable_ones_count_as_the_same() {
        assert_eq!(compare("0.2.50", "0.2.50"), Side::Same);
        assert_eq!(compare("0.2.50", "0.2.53"), Side::ServiceNewer);
        assert_eq!(compare("0.2.53", "0.2.50"), Side::ServiceOlder);
        assert_eq!(compare("0.2.9", "0.2.10"), Side::ServiceNewer);
        assert_eq!(compare("0.9.0", "1.0.0"), Side::ServiceNewer);
        assert_eq!(compare("0.2.50", "dev"), Side::Same);
        assert_eq!(compare("0.2.50", "0.2.53-beta"), Side::Same);
    }

    #[test]
    fn the_safe_to_restart_table() {
        let new = Disk::Version(NEW.into());
        let old = Disk::Version(CLI.into());
        let broken = Disk::Failed("permission denied".into());
        let unchecked = Disk::Unchecked;
        let rows: Vec<(&str, Inputs<'_>, Decision)> = vec![
            ("idle 10 s with nothing pending counts down", safe(&new), Decision::Countdown),
            ("the countdown ran out", Inputs { countdown_left_ms: Some(0), ..safe(&new) }, Decision::Restart),
            ("the countdown still runs", Inputs { countdown_left_ms: Some(1_500), ..safe(&new) }, Decision::Countdown),
            ("typing waits for 10 s without a key", Inputs { idle_ms: 4_000, ..safe(&new) }, Decision::Warn(Warning::Updated)),
            ("an action without an answer waits", Inputs { busy: true, ..safe(&new) }, Decision::Warn(Warning::Updated)),
            ("an open confirmation waits", Inputs { confirming: true, ..safe(&new) }, Decision::Warn(Warning::Updated)),
            ("another Mac never restarts this terminal", Inputs { remote: true, ..safe(&new) }, Decision::Warn(Warning::RemoteNewer)),
            ("the binary on disk is still the old one", safe(&old), Decision::Warn(Warning::DiskOld)),
            ("the binary on disk does not run", safe(&broken), Decision::Warn(Warning::DiskOld)),
            ("the binary on disk is checked first", safe(&unchecked), Decision::CheckDisk),
            ("Esc stays with the fixed line", Inputs { stayed: true, ..safe(&new) }, Decision::Warn(Warning::Updated)),
            ("R restarts at once", Inputs { asked: true, idle_ms: 0, ..safe(&new) }, Decision::Restart),
            ("R during an action waits for it", Inputs { asked: true, busy: true, ..safe(&new) }, Decision::Warn(Warning::WaitAction)),
            ("R with a confirmation open waits", Inputs { asked: true, confirming: true, ..safe(&new) }, Decision::Warn(Warning::WaitAction)),
            ("same version does nothing", Inputs { service: Some(CLI), ..safe(&new) }, Decision::Nothing),
            ("no answer from the daemon does nothing", Inputs { service: None, ..safe(&new) }, Decision::Nothing),
        ];
        for (situation, inputs, expected) in rows {
            assert_eq!(decide(&inputs), expected, "{situation}");
        }
    }

    #[test]
    fn a_restart_never_happens_twice_for_the_same_daemon_version() {
        let new = Disk::Version(NEW.into());
        let looped = Inputs { restarted_for: Some(NEW), countdown_left_ms: Some(0), ..safe(&new) };
        assert_eq!(decide(&looped), Decision::Warn(Warning::Updated));
        let asked = Inputs { restarted_for: Some(NEW), asked: true, ..safe(&new) };
        assert_eq!(decide(&asked), Decision::Warn(Warning::Updated));
        let newer_again = Disk::Version("0.2.54".into());
        let next = Inputs { service: Some("0.2.54"), restarted_for: Some(NEW), countdown_left_ms: Some(0), ..safe(&newer_again) };
        assert_eq!(decide(&next), Decision::Restart);
    }

    #[test]
    fn an_older_daemon_is_left_alone_while_it_restarts_or_when_the_header_already_says_why() {
        let disk = Disk::Unchecked;
        let older = Inputs { cli: NEW, service: Some(CLI), ..safe(&disk) };
        assert_eq!(decide(&Inputs { older_for_ms: OLDER_GRACE_MS - 1, ..older.clone() }), Decision::Nothing);
        assert_eq!(decide(&Inputs { older_for_ms: OLDER_GRACE_MS, ..older.clone() }), Decision::Warn(Warning::ServiceOld));
        assert_eq!(decide(&Inputs { older_for_ms: OLDER_GRACE_MS, rejected: true, ..older }), Decision::Nothing);
    }
}
