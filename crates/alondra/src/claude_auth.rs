//! Claude's local session is distinct from the Alondra account. Never print its profile.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ContextEnv {
    pub home: PathBuf,
    pub path: String,
    pub config_dir: Option<String>,
}

pub fn daemon_path(home: &Path, bin: &Path) -> String {
    format!(
        "{}:{}/.local/bin:{}/.claude/local:{}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        bin.display(),
        home.display(),
        home.display(),
        home.display()
    )
}

/// Resolve the account that actually executes Claude, never inherited USER/LOGNAME.
/// Claude uses this identity when finding its macOS login credentials.
pub(crate) fn effective_username() -> Result<String> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    let code = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        bail!(tr!("cannot resolve effective OS account for Claude; no login was attempted", "no se pudo resolver la cuenta efectiva del sistema para Claude; no se intentó iniciar sesión"));
    }
    let record = unsafe { record.assume_init() };
    if record.pw_name.is_null() {
        bail!(tr!("effective OS account has no username", "la cuenta efectiva del sistema no tiene nombre de usuario"));
    }
    let name = unsafe { std::ffi::CStr::from_ptr(record.pw_name) }
        .to_str()
        .context("effective OS username is not UTF-8")?;
    if name.is_empty() {
        bail!(tr!("effective OS account has an empty username", "la cuenta efectiva del sistema tiene el nombre de usuario vacío"));
    }
    Ok(name.to_owned())
}

impl ContextEnv {
    pub fn for_setup(home: &Path, bin: &Path) -> Self {
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .or_else(|_| std::env::var("CLAUDE_HOME"))
            .ok()
            .filter(|p| !p.is_empty())
            .map(|p| {
                let p = PathBuf::from(p);
                if p.is_absolute() {
                    p
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|_| home.to_owned())
                        .join(p)
                }
            })
            .map(|p| p.to_string_lossy().into_owned());
        Self {
            home: home.to_owned(),
            path: daemon_path(home, bin),
            config_dir,
        }
    }

    pub fn from_environment(home: &Path, vars: &BTreeMap<String, String>) -> Result<Self> {
        if let (Some(config), Some(watcher)) =
            (vars.get("CLAUDE_CONFIG_DIR"), vars.get("CLAUDE_HOME"))
        {
            if config != watcher {
                bail!(tr!("Claude profile variables disagree; rerun alondra setup with one profile", "las variables del perfil de Claude no coinciden; vuelve a ejecutar alondra setup con un único perfil"));
            }
        }
        Ok(Self {
            home: vars
                .get("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_owned()),
            path: vars
                .get("PATH")
                .context("daemon plist has no PATH; rerun alondra setup")?
                .clone(),
            config_dir: vars
                .get("CLAUDE_CONFIG_DIR")
                .or_else(|| vars.get("CLAUDE_HOME"))
                .cloned(),
        })
    }

    pub(crate) fn command(&self) -> Result<Command> {
        // Resolve using exactly the daemon PATH, not the interactive shell's executable.
        let executable = std::env::split_paths(&self.path).map(|p| p.join("claude")).find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }).context("Claude Code is missing from the daemon PATH; install Claude Code, then rerun alondra setup")?;
        let username = effective_username()?;
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.path)
            .env("USER", &username)
            .env("LOGNAME", &username)
            .current_dir(&self.home);
        // Presence changes Claude's credential namespace even for HOME/.claude.
        // An implicit default must remain absent, not an explicit default path.
        if let Some(dir) = &self.config_dir {
            command.env("CLAUDE_CONFIG_DIR", dir).env("CLAUDE_HOME", dir);
        }
        Ok(command)
    }

    pub fn status(&self) -> Result<bool> {
        let mut child = self
            .command()?
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not check Claude authentication")?;
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        let exit = wait(&mut child, Duration::from_secs(10))?;
        let bytes = rx
            .recv_timeout(Duration::from_secs(1))
            .context("Claude status output timed out")??;
        parse_status(exit.success(), &bytes)
    }

    fn login(&self) -> Result<()> {
        println!("Claude Code needs its own login. Opening `claude auth login`…");
        let mut child = self
            .command()?
            .args(["auth", "login"])
            .spawn()
            .context("could not start Claude login")?;
        if !wait(&mut child, Duration::from_secs(300))?.success() {
            bail!(tr!(
                "Claude login did not finish; run `claude auth login`, then rerun `alondra setup`",
                "el inicio de sesión de Claude no terminó; ejecuta `claude auth login` y vuelve a ejecutar `alondra setup`"
            ));
        }
        Ok(())
    }

    pub fn ensure(&self, no_input: bool) -> Result<()> {
        ensure_with(
            !no_input && std::io::stdin().is_terminal(),
            || self.status(),
            || self.login(),
        )
    }
}

fn wait(child: &mut std::process::Child, timeout: Duration) -> Result<std::process::ExitStatus> {
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!(tr!("Claude authentication command timed out; rerun alondra setup", "el comando de autenticación de Claude superó el tiempo de espera; vuelve a ejecutar alondra setup"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn parse_status(success: bool, bytes: &[u8]) -> Result<bool> {
    if bytes.len() > 65536 {
        bail!(tr!("Claude authentication response too large", "la respuesta de autenticación de Claude es demasiado grande"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!(tr!("Claude returned invalid authentication status; update Claude Code and rerun alondra setup", "Claude devolvió un estado de autenticación no válido; actualiza Claude Code y vuelve a ejecutar alondra setup")))?;
    match value.get("loggedIn").and_then(serde_json::Value::as_bool) {
        Some(false) => Ok(false),
        Some(true) if success => Ok(true),
        _ => bail!(tr!(
            "Claude authentication status could not be verified; update Claude Code and rerun alondra setup",
            "no se pudo verificar el estado de autenticación de Claude; actualiza Claude Code y vuelve a ejecutar alondra setup"
        )),
    }
}

fn ensure_with(
    interactive: bool,
    mut status: impl FnMut() -> Result<bool>,
    mut login: impl FnMut() -> Result<()>,
) -> Result<()> {
    if status()? {
        return Ok(());
    }
    if !interactive {
        bail!(tr!(
            "Claude Code is not signed in. Run `claude auth login` for this Mac user, then rerun `alondra setup`",
            "Claude Code no tiene la sesión iniciada. Ejecuta `claude auth login` con este usuario de Mac y vuelve a ejecutar `alondra setup`"
        ));
    }
    login()?;
    if !status()? {
        bail!(tr!(
            "Claude Code is still not signed in; complete `claude auth login` and rerun `alondra setup`",
            "Claude Code sigue sin tener la sesión iniciada; completa `claude auth login` y vuelve a ejecutar `alondra setup`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_fails_closed_without_exposing_profile() {
        assert!(parse_status(true, br#"{"loggedIn":true,"email":"private@example.org"}"#).unwrap());
        assert!(!parse_status(false, br#"{"loggedIn":false}"#).unwrap());
        for (ok, bytes) in [
            (true, b"corrupt private@example.org".as_slice()),
            (true, b"{}".as_slice()),
            (false, br#"{"loggedIn":true}"#.as_slice()),
        ] {
            let error = parse_status(ok, bytes).unwrap_err().to_string();
            assert!(!error.contains("private@example.org"));
        }
    }
    #[test]
    fn absent_executable_is_actionable() {
        let env = ContextEnv {
            home: "/tmp".into(),
            path: "/nonexistent-alondra-test".into(),
            config_dir: None,
        };
        assert!(
            env.status()
                .unwrap_err()
                .to_string()
                .contains("install Claude Code")
        );
    }
    #[test]
    fn noninteractive_never_starts_login() {
        assert!(ensure_with(false, || Ok(false), || panic!("must not login")).is_err());
        assert!(ensure_with(false, || Ok(true), || panic!("must not login")).is_ok());
        assert!(
            ensure_with(
                true,
                || bail!("corrupt status"),
                || panic!("must not login")
            )
            .is_err()
        );
    }
    #[test]
    fn interactive_rechecks_after_login() {
        let mut checks = 0;
        let mut logins = 0;
        ensure_with(
            true,
            || {
                checks += 1;
                Ok(checks == 2)
            },
            || {
                logins += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!((checks, logins), (2, 1));
        assert!(ensure_with(true, || Ok(false), || Ok(())).is_err());
    }
    #[test]
    fn status_executes_daemon_binary_in_its_home_and_config() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-claude-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let script = dir.join("claude");
        std::fs::write(&script, r#"#!/bin/sh
if [ "$1 $2 $3" = "auth status --json" ] && [ "$HOME" = "$PWD" ] && [ "$CLAUDE_CONFIG_DIR" = "$HOME/profile" ] && [ "$CLAUDE_HOME" = "$CLAUDE_CONFIG_DIR" ]; then
  printf '{"loggedIn":true}'
else
  printf '{"loggedIn":false}'
  exit 1
fi
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let env = ContextEnv {
            home: dir.clone(),
            path: dir.to_string_lossy().into_owned(),
            config_dir: Some(dir.join("profile").to_string_lossy().into_owned()),
        };
        assert!(env.status().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn os_identity_unlocks_mock_auth_without_inheriting_credentials() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_directory.parent().unwrap().join(format!(
            "alondra-claude-user-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let username = effective_username().unwrap();
        std::fs::write(dir.join("expected-user"), &username).unwrap();
        let script = dir.join("claude");
        std::fs::write(&script, r#"#!/bin/sh
expected=$(cat "$HOME/expected-user")
if [ "$USER" = "$expected" ] && [ "$LOGNAME" = "$expected" ] && [ "$CLAUDE_CONFIG_DIR" = "$HOME/custom" ] && [ "$CLAUDE_HOME" = "$CLAUDE_CONFIG_DIR" ] && [ -z "$ANTHROPIC_API_KEY" ]; then
  printf '{"loggedIn":true}'
else
  printf '{"loggedIn":false}'
fi
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let context = ContextEnv {
            home: dir.clone(),
            path: format!("{}:/usr/bin:/bin", dir.display()),
            config_dir: Some(dir.join("custom").to_string_lossy().into_owned()),
        };
        assert!(context.status().unwrap());
        // Reproduce the old env_clear omission with the same executable/profile.
        let output = context
            .command()
            .unwrap()
            .env_remove("USER")
            .env_remove("LOGNAME")
            .args(["auth", "status", "--json"])
            .output()
            .unwrap();
        assert!(!parse_status(output.status.success(), &output.stdout).unwrap());
        let output = context
            .command()
            .unwrap()
            .env("LOGNAME", "wrong-account")
            .args(["auth", "status", "--json"])
            .output()
            .unwrap();
        assert!(!parse_status(output.status.success(), &output.stdout).unwrap());
        let command = context.command().unwrap();
        let vars: BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(vars["USER"], username);
        assert_eq!(vars["LOGNAME"], username);
        assert_eq!(
            vars.len(),
            6,
            "only explicit identity/path/profile variables are passed"
        );
        // The same command factory is used for interactive login and plugin operations.
        context.ensure(true).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn installed_context_uses_daemon_configuration() {
        let vars = BTreeMap::from([
            ("HOME".into(), "/Users/other".into()),
            ("PATH".into(), "/opt/homebrew/bin".into()),
            ("CLAUDE_CONFIG_DIR".into(), "/Users/other/custom".into()),
        ]);
        let context = ContextEnv::from_environment(Path::new("/wrong"), &vars).unwrap();
        assert_eq!(context.home, Path::new("/Users/other"));
        assert_eq!(context.config_dir.as_deref(), Some("/Users/other/custom"));
        assert_eq!(context.path, "/opt/homebrew/bin");
    }
}
