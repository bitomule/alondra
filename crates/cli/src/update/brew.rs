//! What `brew upgrade alondra` prints, read as the steps `alondra update` shows.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    Starting,
    UpdatingHomebrew,
    Downloading,
    Installing,
}

#[derive(Debug, Default)]
pub struct Progress {
    stage: Option<Stage>,
    target: Option<String>,
    current: Option<String>,
}

impl Progress {
    /// Reads one line of Homebrew's output; returns the stage it starts, if it starts one.
    pub fn feed(&mut self, line: &str) -> Option<Stage> {
        let line = line.trim();
        if let Some(version) = upgrade_target(line) {
            self.target = Some(version);
        }
        if let Some(version) = already_installed(line) {
            self.current = Some(version);
        }
        let stage = if line.starts_with("==> Auto-updating Homebrew") || line.starts_with("==> Updating Homebrew") {
            Stage::UpdatingHomebrew
        } else if line.starts_with("==> Fetching") || line.starts_with("==> Downloading") {
            Stage::Downloading
        } else if (line.starts_with("==> Upgrading ") && !line.ends_with(':')) || line.starts_with("==> Installing") || line.starts_with("==> Pouring") {
            Stage::Installing
        } else {
            return None;
        };
        if stage as u8 <= self.stage() as u8 {
            return None;
        }
        self.stage = Some(stage);
        Some(stage)
    }

    pub fn stage(&self) -> Stage {
        self.stage.unwrap_or(Stage::Starting)
    }

    /// The version Homebrew said it upgrades to, or that it already has.
    pub fn version(&self) -> Option<&str> {
        self.target.as_deref().or(self.current.as_deref())
    }

    pub fn running(&self, stage: Stage) -> String {
        let release = self.release();
        match stage {
            Stage::Starting => tr!("Asking Homebrew for the latest alondra", "Preguntando a Homebrew por la última alondra").into(),
            Stage::UpdatingHomebrew => tr!("Updating Homebrew", "Actualizando Homebrew").into(),
            Stage::Downloading => trf!("Downloading {release}", "Descargando {release}"),
            Stage::Installing => trf!("Installing {release}", "Instalando {release}"),
        }
    }

    /// What a stage that finished says; the last one says it through the version installed.
    pub fn done(&self, stage: Stage) -> Option<String> {
        let release = self.release();
        match stage {
            Stage::Starting | Stage::Installing => None,
            Stage::UpdatingHomebrew => Some(tr!("Homebrew is up to date", "Homebrew al día").into()),
            Stage::Downloading => Some(trf!("Downloaded {release}", "Descargada {release}")),
        }
    }

    pub fn failed(&self) -> String {
        let release = self.release();
        match self.stage() {
            Stage::Starting => tr!("Homebrew could not upgrade alondra", "Homebrew no pudo actualizar alondra").into(),
            Stage::UpdatingHomebrew => tr!("Homebrew could not update itself", "Homebrew no pudo actualizarse").into(),
            Stage::Downloading => trf!("Could not download {release}", "No se pudo descargar {release}"),
            Stage::Installing => trf!("Could not install {release}", "No se pudo instalar {release}"),
        }
    }

    fn release(&self) -> String {
        self.target.as_deref().map_or_else(|| "alondra".into(), |v| format!("alondra {v}"))
    }
}

/// `bitomule/tap/alondra 0.3.5 -> 0.3.6`
fn upgrade_target(line: &str) -> Option<String> {
    let (name, versions) = line.split_once(' ')?;
    if name != "alondra" && !name.ends_with("/alondra") {
        return None;
    }
    let (_, to) = versions.split_once(" -> ")?;
    Some(to.trim().to_string())
}

/// `Warning: bitomule/tap/alondra 0.3.6 already installed`
fn already_installed(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Warning: ")?.strip_suffix(" already installed")?;
    let (name, version) = rest.split_once(' ')?;
    (name == "alondra" || name.ends_with("/alondra")).then(|| version.to_string())
}

fn is_noise(line: &str) -> bool {
    line.is_empty()
        || line.chars().all(|c| matches!(c, '#' | '%' | '.' | ' ' | '0'..='9' | '-' | '='))
        || ["Adjust how often", "HOMEBREW_NO_AUTO_UPDATE", "Hide these hints"].iter().any(|hint| line.starts_with(hint))
}

/// A line without the frame `alondra doctor --fix` draws around its steps.
fn undecorated(line: &str) -> &str {
    line.trim_start_matches(['│', '◇', '└', '┌', '◆', '▲', '■', '●', '○']).trim()
}

fn is_error(line: &str) -> bool {
    ["Error:", "curl: (", "fatal:", "Failed", "Failure"].iter().any(|start| line.starts_with(start))
}

/// The part of Homebrew's output that says why it failed: from its first error line, or its
/// last lines when it names none, without progress bars and hints.
pub fn failure_tail(output: &str, max: usize) -> Vec<String> {
    let lines: Vec<&str> = output.lines().map(undecorated).filter(|l| !is_noise(l.trim())).collect();
    let from = lines.iter().position(|l| is_error(l.trim())).unwrap_or(lines.len().saturating_sub(max));
    lines[from..].iter().take(max).map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPGRADED: &str = "\
==> Auto-updating Homebrew...
Adjust how often this is run with HOMEBREW_AUTO_UPDATE_SECS or disable with
HOMEBREW_NO_AUTO_UPDATE. Hide these hints with HOMEBREW_NO_ENV_HINTS (see `man brew`).
==> Auto-updated Homebrew!
Updated 2 taps (bitomule/tap and homebrew/core).
==> Upgrading 1 outdated package:
bitomule/tap/alondra 0.3.5 -> 0.3.6
==> Fetching downloads for: alondra
==> Downloading https://github.com/bitomule/homebrew-tap/releases/download/alondra-0.3.6/alondra-0.3.6-aarch64-apple-darwin.tar.gz
==> Downloading from https://release-assets.githubusercontent.com/github-production-release-asset/1
######################################################################## 100.0%
==> Upgrading bitomule/tap/alondra
  0.3.5 -> 0.3.6
==> Caveats
Run alondra to set up this Mac.
==> Summary
🍺  /opt/homebrew/Cellar/alondra/0.3.6: 214 files, 151.2MB, built in 4 seconds
==> Running `brew cleanup alondra`...
Removing: /opt/homebrew/Cellar/alondra/0.3.5... (210 files, 149.1MB)";

    const DOWNLOAD_FAILED: &str = "\
==> Auto-updating Homebrew...
Adjust how often this is run with HOMEBREW_AUTO_UPDATE_SECS or disable with
HOMEBREW_NO_AUTO_UPDATE. Hide these hints with HOMEBREW_NO_ENV_HINTS (see `man brew`).
==> Upgrading 1 outdated package:
bitomule/tap/alondra 0.3.5 -> 0.3.6
==> Fetching downloads for: alondra
==> Downloading https://github.com/bitomule/homebrew-tap/releases/download/alondra-0.3.6/alondra-0.3.6-aarch64-apple-darwin.tar.gz
######################################################################## 100.0%
curl: (22) The requested URL returned error: 404

Error: alondra: Failed to download resource \"alondra (0.3.6)\"
Download failed: https://github.com/bitomule/homebrew-tap/releases/download/alondra-0.3.6/alondra-0.3.6-aarch64-apple-darwin.tar.gz";

    fn english() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    }

    fn read(output: &str) -> (Progress, Vec<Stage>) {
        let mut progress = Progress::default();
        let stages = output.lines().filter_map(|l| progress.feed(l)).collect();
        (progress, stages)
    }

    #[test]
    fn an_upgrade_goes_through_homebrew_download_and_install_once_each() {
        english();
        let (progress, stages) = read(UPGRADED);
        assert_eq!(stages, [Stage::UpdatingHomebrew, Stage::Downloading, Stage::Installing]);
        assert_eq!(progress.version(), Some("0.3.6"));
        assert_eq!(progress.running(Stage::Downloading), "Downloading alondra 0.3.6");
        assert_eq!(progress.done(Stage::UpdatingHomebrew).as_deref(), Some("Homebrew is up to date"));
        assert_eq!(progress.done(Stage::Downloading).as_deref(), Some("Downloaded alondra 0.3.6"));
    }

    #[test]
    fn a_release_already_installed_is_read_from_the_warning() {
        let (progress, stages) = read("==> Auto-updating Homebrew...\nWarning: bitomule/tap/alondra 0.3.6 already installed");
        assert_eq!(stages, [Stage::UpdatingHomebrew]);
        assert_eq!(progress.version(), Some("0.3.6"));
    }

    #[test]
    fn a_failure_names_the_step_that_failed() {
        english();
        let (progress, _) = read(DOWNLOAD_FAILED);
        assert_eq!(progress.failed(), "Could not download alondra 0.3.6");
        assert_eq!(Progress::default().failed(), "Homebrew could not upgrade alondra");
    }

    #[test]
    fn the_failure_tail_starts_at_the_error_without_progress_bars_or_hints() {
        assert_eq!(failure_tail(DOWNLOAD_FAILED, 8), [
            "curl: (22) The requested URL returned error: 404",
            "Error: alondra: Failed to download resource \"alondra (0.3.6)\"",
            "Download failed: https://github.com/bitomule/homebrew-tap/releases/download/alondra-0.3.6/alondra-0.3.6-aarch64-apple-darwin.tar.gz",
        ]);
    }

    #[test]
    fn without_an_error_line_the_failure_tail_is_the_last_lines() {
        let output = "==> Auto-updating Homebrew...\n\none\ntwo\n###### 100.0%\nthree\n";
        assert_eq!(failure_tail(output, 2), ["two", "three"]);
        assert_eq!(failure_tail("", 2), Vec::<String>::new());
        let long = (0..20).map(|i| format!("Error: {i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(failure_tail(&long, 3), ["Error: 0", "Error: 1", "Error: 2"]);
        let framed = "Alondra 0.3.6 · checking this Mac\n│\n◇  Service  launchctl bootstrap failed\n└  1 problem.";
        assert_eq!(failure_tail(framed, 8), ["Alondra 0.3.6 · checking this Mac", "Service  launchctl bootstrap failed", "1 problem."]);
    }
}
