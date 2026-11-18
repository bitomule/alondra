//! Claude's local session is distinct from the Alondra account. Never print its profile.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ContextEnv {
    pub home: PathBuf,
    pub path: String,
    pub config_dir: Option<String>,
    /// How long `claude auth status` may take; `None` waits for it to end, for a test's script,
    /// whose first run waits while macOS checks the new executable.
    pub status_timeout: Option<Duration>,
}

pub(crate) struct CodexContext {
    pub home: PathBuf,
    pub path: String,
    pub environment: BTreeMap<String, String>,
}
impl CodexContext {
    pub(crate) fn from_job(job: &serde_json::Value) -> Result<Self> {
        let vars = job.get("EnvironmentVariables").context("The service has no native environment")?;
        let home = vars.get("HOME").and_then(serde_json::Value::as_str).context("The service has no HOME")?;
        let path = vars.get("PATH").and_then(serde_json::Value::as_str).context("The service has no PATH")?;
        let mut environment = BTreeMap::new();
        if let Some(value) = vars.get("CODEX_HOME").and_then(serde_json::Value::as_str).filter(|value| !value.is_empty()) {
            anyhow::ensure!(Path::new(value).is_absolute(), "The service CODEX_HOME must be absolute");
            environment.insert("CODEX_HOME".into(), value.to_owned());
        }
        Ok(Self { home: home.into(), path: path.into(), environment })
    }
}
pub(crate) fn installed_codex_context(user_home: &Path) -> Result<CodexContext> {
    let config = alondra_core::config::Config::load().map_err(anyhow::Error::msg)?;
    let job = crate::setup::read_plist(&user_home.join("Library/LaunchAgents").join(format!("{}.plist", config.service_label)))
        .context("Cannot determine the service's Codex profile; run alondra doctor --fix before signing in")?;
    CodexContext::from_job(&job)
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
        .context(tr!("the macOS user name is not UTF-8", "el nombre de usuario de macOS no es UTF-8"))?;
    if name.is_empty() {
        bail!(tr!("effective OS account has an empty username", "la cuenta efectiva del sistema tiene el nombre de usuario vacío"));
    }
    Ok(name.to_owned())
}

/// The profile a launchd job's environment names, as the daemon reads its own.
pub(crate) fn job_profile(job: &serde_json::Value) -> Option<PathBuf> {
    let vars = job.get("EnvironmentVariables")?;
    alondra_core::claude_account::configured_profile(&|key| vars.get(key)?.as_str().map(str::to_owned))
}

impl ContextEnv {
    pub fn for_setup(home: &Path, bin: &Path, env: &impl Fn(&str) -> Option<String>) -> Self {
        let config_dir = alondra_core::claude_account::configured_profile(env)
            .map(|p| {
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
            status_timeout: Some(alondra_core::claude_account::STATUS_TIMEOUT),
        }
    }

    pub fn from_environment(home: &Path, vars: &BTreeMap<String, String>) -> Result<Self> {
        if let (Some(config), Some(watcher)) =
            (vars.get("CLAUDE_CONFIG_DIR"), vars.get("CLAUDE_HOME"))
        {
            if config != watcher {
                bail!(tr!("Claude profile variables disagree; run alondra doctor --fix with one profile", "las variables del perfil de Claude no coinciden; ejecuta alondra doctor --fix con un único perfil"));
            }
        }
        Ok(Self {
            home: vars
                .get("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_owned()),
            path: vars
                .get("PATH")
                .context(tr!("the service has no PATH; run alondra doctor --fix", "el servicio no tiene PATH; ejecuta alondra doctor --fix"))?
                .clone(),
            config_dir: alondra_core::claude_account::configured_profile(&|key| vars.get(key).cloned())
                .map(|p| p.to_string_lossy().into_owned()),
            status_timeout: Some(alondra_core::claude_account::STATUS_TIMEOUT),
        })
    }

    pub(crate) fn command(&self) -> Result<Command> {
        // Resolve using exactly the daemon PATH, not the interactive shell's executable.
        let executable = std::env::split_paths(&self.path).map(|p| p.join("claude")).find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }).context(tr!("alondrad cannot find Claude Code; install Claude Code, then run alondra doctor --fix",
            "alondrad no encuentra Claude Code; instala Claude Code y luego ejecuta alondra doctor --fix"))?;
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
            .context(tr!("could not check the Claude sign-in", "no se pudo comprobar la sesión de Claude"))?;
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes);
            let _ = tx.send(result);
        });
        let exit = wait(&mut child, self.status_timeout)?;
        let bytes = rx
            .recv_timeout(Duration::from_secs(1))
            .context(tr!("Claude took too long to say whether it is signed in", "Claude tardó demasiado en decir si hay sesión iniciada"))??;
        parse_status(exit.success(), &bytes)
    }
}

fn wait(child: &mut std::process::Child, timeout: Option<Duration>) -> Result<std::process::ExitStatus> {
    let Some(timeout) = timeout else { return Ok(child.wait()?) };
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!(tr!("Claude authentication command timed out; run alondra doctor --fix", "el comando de autenticación de Claude superó el tiempo de espera; ejecuta alondra doctor --fix"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn parse_status(success: bool, bytes: &[u8]) -> Result<bool> {
    if bytes.len() > 65536 {
        bail!(tr!("Claude authentication response too large", "la respuesta de autenticación de Claude es demasiado grande"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!(tr!("Claude returned invalid authentication status; update Claude Code and run alondra doctor --fix", "Claude devolvió un estado de autenticación no válido; actualiza Claude Code y ejecuta alondra doctor --fix")))?;
    match value.get("loggedIn").and_then(serde_json::Value::as_bool) {
        Some(false) => Ok(false),
        Some(true) if success => Ok(true),
        _ => bail!(tr!(
            "Claude authentication status could not be verified; update Claude Code and run alondra doctor --fix",
            "no se pudo verificar el estado de autenticación de Claude; actualiza Claude Code y ejecuta alondra doctor --fix"
        )),
    }
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
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let env = ContextEnv {
            home: "/tmp".into(),
            path: "/nonexistent-alondra-test".into(),
            config_dir: None,
            status_timeout: None,
        };
        assert!(
            env.status()
                .unwrap_err()
                .to_string()
                .contains("install Claude Code")
        );
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
            status_timeout: None,
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
            status_timeout: None,
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
        assert_eq!(context.status_timeout, Some(alondra_core::claude_account::STATUS_TIMEOUT));
        assert_eq!(ContextEnv::for_setup(Path::new("/Users/other"), Path::new("/opt/homebrew/bin"), &|_| None).status_timeout, Some(alondra_core::claude_account::STATUS_TIMEOUT));
    }

    // The daemon's nodes run with `claude_account::configured_profile` of this same environment:
    // setup and doctor check the sign-in of exactly that profile.
    #[test]
    fn setup_and_doctor_check_the_profile_the_daemons_nodes_run_with() {
        let job = |vars: &[(&str, &str)]| {
            let vars: BTreeMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).chain([("PATH".into(), "/bin".into())]).collect();
            let nodes = alondra_core::claude_account::configured_profile(&|key| vars.get(key).cloned());
            let checked = ContextEnv::from_environment(Path::new("/Users/ana"), &vars).unwrap().config_dir.map(PathBuf::from);
            assert_eq!(job_profile(&serde_json::json!({"EnvironmentVariables": vars})), nodes, "{vars:?}");
            assert_eq!(checked, nodes, "{vars:?}");
            checked
        };
        assert_eq!(job(&[]), None);
        assert_eq!(job(&[("CLAUDE_CONFIG_DIR", "")]), None);
        let work = Some(PathBuf::from("/Users/ana/.claude-work"));
        assert_eq!(job(&[("CLAUDE_CONFIG_DIR", "/Users/ana/.claude-work"), ("CLAUDE_HOME", "/Users/ana/.claude-work")]), work);
        assert_eq!(job(&[("CLAUDE_HOME", "/Users/ana/.claude-work")]), work);
    }
    #[test]
    fn codex_login_uses_service_namespace_and_never_copies_secrets() {
        let job = serde_json::json!({"EnvironmentVariables":{"HOME":"/service/user","PATH":"/service/bin","CODEX_HOME":"/service/codex","OPENAI_API_KEY":"secret","CLAUDE_CONFIG_DIR":"/wrong"}});
        let context = CodexContext::from_job(&job).unwrap();
        assert_eq!(context.home, PathBuf::from("/service/user")); assert_eq!(context.path, "/service/bin");
        assert_eq!(context.environment, BTreeMap::from([("CODEX_HOME".into(), "/service/codex".into())]));
        let default = CodexContext::from_job(&serde_json::json!({"EnvironmentVariables":{"HOME":"/service/user","PATH":"/service/bin"}})).unwrap();
        assert!(default.environment.is_empty());
        assert!(CodexContext::from_job(&serde_json::json!({"EnvironmentVariables":{"HOME":"/service/user","PATH":"/service/bin","CODEX_HOME":"relative"}})).is_err());
    }

}
