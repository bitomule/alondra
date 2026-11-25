//! `alondra spec`: what the specs of a repository need validated, and the commands that run a
//! component's suites and record a verdict. It reads the files git lists and never talks to the
//! service (`docs/architecture/specs.md`).
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use alondra_spec_engine::{Code, Entry, Evidence, Files, Kind, Problem, Project, Report, ReportStatus, State, Verdict, load, read_results};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

const CLEAN: i32 = 0;
const PENDING: i32 = 1;
const UNREADABLE: i32 = 2;

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const LEFTOVER_OUTPUT_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

#[derive(Subcommand)]
pub enum SpecCmd {
    #[command(about = tr!("List what is not validated; it runs nothing and writes nothing", "Lista lo que falta por validar; no ejecuta ni escribe nada"),
        long_about = tr!("List every validation that is missing or stale. It never runs a command, starts an agent or writes a file. It exits with 0 when nothing is missing, 1 when something is and 2 when the specs cannot be read.", "Lista cada validación que falta o caducó. Nunca ejecuta un comando, lanza un agente ni escribe un archivo. Sale con 0 si no falta nada, con 1 si falta algo y con 2 si las specs no se pueden leer."))]
    Validate {
        #[arg(long, value_name = "FILE", help = tr!("Test results as JUnit XML: each declared test must appear in them as passed", "Resultados de test en JUnit XML: cada test declarado debe aparecer en ellos como aprobado"))]
        junit: Option<PathBuf>,
    },
    #[command(about = tr!("Run the suites of a component and record the ones that end well", "Ejecuta las suites de un componente y registra las que terminan bien"))]
    Run {
        #[arg(help = tr!("The id of the component", "El identificador del componente"))]
        component: String,
        #[arg(long, value_name = "ID", help = tr!("Run only this suite", "Ejecuta solo esta suite"))]
        suite: Option<String>,
    },
    #[command(about = tr!("Record the verdict of a judgment", "Registra el veredicto de un juicio"))]
    Record {
        #[arg(help = tr!("<spec>/<scenario>, or the id of a reference section", "<spec>/<escenario>, o el identificador de una sección de referencia"))]
        subject: String,
        #[arg(long, value_enum, help = tr!("What was judged", "Qué se juzgó"))]
        kind: RecordKind,
        #[arg(long, value_enum, help = tr!("The verdict", "El veredicto"))]
        verdict: RecordVerdict,
        #[arg(long, value_name = "FILE", help = tr!("An evidence file: a capture or a video", "Un archivo de evidencia: una captura o un vídeo"))]
        evidence: Vec<PathBuf>,
    },
    #[command(about = tr!("Print the specs, boundaries, decisions and reference sections as JSON", "Imprime las specs, fronteras, decisiones y secciones de referencia en JSON"))]
    List,
    #[command(about = tr!("Print what a fresh implementer of a component receives", "Imprime lo que recibe quien implementa un componente desde cero"))]
    Packet {
        #[arg(help = tr!("The id of the component", "El identificador del componente"))]
        component: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum RecordKind {
    Adequate,
    Judged,
    Reference,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum RecordVerdict {
    Met,
    Unmet,
}

#[derive(Parser)]
#[command(name = "alondra-spec", about = tr!("What the specs of this repository need validated", "Qué falta por validar en las specs de este repositorio"))]
struct Standalone {
    #[arg(long, global = true, help = tr!("Machine-readable output", "Salida para máquinas"))]
    json: bool,
    #[command(subcommand)]
    command: SpecCmd,
}

/// The `alondra-spec` binary: the same commands without the rest of the terminal.
pub fn standalone() -> Result<()> {
    let cli = Standalone::parse();
    run(cli.command, cli.json)
}

pub fn run(command: SpecCmd, json: bool) -> Result<()> {
    let root = repository_root(&std::env::current_dir()?)?;
    let paths = listed(&root)?;
    match run_in(&root, paths, command, json, &mut std::io::stdout())? {
        CLEAN => Ok(()),
        code => std::process::exit(code),
    }
}

/// The files of a checkout that git lists, read from the working tree. A path that leaves the
/// checkout through a link is not read.
struct Checkout {
    root: PathBuf,
    paths: Vec<String>,
}

impl Files for Checkout {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let real = self.root.join(path).canonicalize().ok()?;
        if !real.starts_with(&self.root) {
            return None;
        }
        std::fs::read(real).ok()
    }
}

fn repository_root(from: &Path) -> Result<PathBuf> {
    let output = Command::new("git").arg("-C").arg(from).args(["rev-parse", "--show-toplevel"]).stderr(Stdio::null()).output().context(tr!("git is not installed", "git no está instalado"))?;
    if !output.status.success() {
        bail!(tr!("This folder is not inside a git repository", "Esta carpeta no está dentro de un repositorio git"));
    }
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim_end()).canonicalize()?)
}

fn listed(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git").arg("-C").arg(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).stderr(Stdio::null()).output()?;
    if !output.status.success() {
        bail!(tr!("git could not list the files of this repository", "git no pudo listar los archivos de este repositorio"));
    }
    let mut paths: Vec<String> = String::from_utf8(output.stdout)?.split('\0').filter(|path| !path.is_empty() && root.join(path).is_file()).map(str::to_owned).collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(crate) fn run_in(root: &Path, paths: Vec<String>, command: SpecCmd, json: bool, out: &mut dyn Write) -> Result<i32> {
    let checkout = Checkout { root: root.canonicalize()?, paths };
    if let SpecCmd::Validate { junit } = &command {
        let report = validated(&checkout, junit.as_deref());
        match json {
            true => writeln!(out, "{}", report.to_json())?,
            false => write!(out, "{}", rendered(&report))?,
        }
        return Ok(exit_code(report.status));
    }
    let project = match load(&checkout) {
        Ok(project) => project,
        Err(problems) => {
            write!(out, "{}", rendered(&Report::unreadable(problems)))?;
            return Ok(UNREADABLE);
        }
    };
    match command {
        SpecCmd::Validate { .. } => Ok(CLEAN),
        SpecCmd::Run { component, suite } => run_suites(&checkout.root, &project, &component, suite.as_deref(), out),
        SpecCmd::Record { subject, kind, verdict, evidence } => record(&checkout.root, &project, &subject, kind, verdict, &evidence, out),
        SpecCmd::List => {
            writeln!(out, "{}", project.listing())?;
            Ok(CLEAN)
        }
        SpecCmd::Packet { component } => answered(project.packet(&component), out, |packet, out| Ok(write!(out, "{packet}")?)),
    }
}

fn validated(checkout: &Checkout, junit: Option<&Path>) -> Report {
    let project = match load(checkout) {
        Ok(project) => project,
        Err(problems) => return Report::unreadable(problems),
    };
    let Some(junit) = junit else { return project.report(None) };
    let path = junit.display().to_string();
    let unread = |problem: Problem| Report::unreadable(vec![Problem { path: path.clone(), ..problem }]);
    match std::fs::read(junit) {
        Ok(bytes) => match read_results(&bytes) {
            Ok(results) => project.report(Some(&results)),
            Err(problem) => unread(problem),
        },
        Err(error) => unread(Problem { code: Code::InvalidResults, path: String::new(), line: 0, message: error.to_string() }),
    }
}

fn exit_code(status: ReportStatus) -> i32 {
    match status {
        ReportStatus::Clean => CLEAN,
        ReportStatus::Pending => PENDING,
        ReportStatus::Unreadable => UNREADABLE,
    }
}

fn rendered(report: &Report) -> String {
    let mut lines = vec![];
    for problem in &report.problems {
        lines.push(format!("{}:{}: {}: {}", problem.path, problem.line, problem.code.as_str(), problem.message));
    }
    let pending: Vec<_> = report.items.iter().filter(|item| item.state != State::Valid).collect();
    for item in &pending {
        lines.push(format!("{:<9}  {:<9}  {}", item.state.as_str(), item.kind.as_str(), item.subject));
    }
    for path in &report.leftover {
        lines.push(trf!("leftover   {path}", "sobrante   {path}"));
    }
    lines.push(match report.status {
        ReportStatus::Clean => tr!("Everything is validated.", "Todo está validado.").to_owned(),
        ReportStatus::Pending => {
            let (missing, all) = (pending.len(), report.items.len());
            trf!("{missing} of {all} validations are missing.", "Faltan {missing} de {all} validaciones.")
        }
        ReportStatus::Unreadable => tr!("The specs cannot be read.", "Las specs no se pueden leer.").to_owned(),
    });
    lines.iter().map(|line| format!("{line}\n")).collect()
}

fn answered<T>(answer: std::result::Result<T, Problem>, out: &mut dyn Write, then: impl FnOnce(T, &mut dyn Write) -> Result<()>) -> Result<i32> {
    match answer {
        Ok(value) => {
            then(value, out)?;
            Ok(CLEAN)
        }
        Err(problem) => {
            writeln!(out, "{}: {}", problem.code.as_str(), problem.message)?;
            Ok(UNREADABLE)
        }
    }
}

pub(crate) enum Outcome {
    Passed,
    Failed,
    TimedOut,
    NotRunnable,
}

pub(crate) struct Limits {
    pub time: Duration,
    pub output: usize,
}

static RUNNING_GROUP: AtomicI32 = AtomicI32::new(0);

extern "C" fn interrupted(signal: libc::c_int) {
    let group = RUNNING_GROUP.load(Ordering::SeqCst);
    if group > 0 {
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    unsafe { libc::_exit(128 + signal) };
}

/// While it lives, interrupting this command also stops the suite it is running.
struct StopsWithUs;

impl StopsWithUs {
    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    fn new() -> Self {
        for signal in Self::SIGNALS {
            unsafe { libc::signal(signal, interrupted as *const () as libc::sighandler_t) };
        }
        Self
    }
}

impl Drop for StopsWithUs {
    fn drop(&mut self) {
        for signal in Self::SIGNALS {
            unsafe { libc::signal(signal, libc::SIG_DFL) };
        }
    }
}

/// Runs one command without a shell, from the root, in a process group of its own: at its time
/// limit every process of the group is stopped, not only the one that was started. Its output is
/// copied to `out` up to the output limit and dropped after it.
pub(crate) fn execute(root: &Path, arguments: &[String], limits: &Limits, out: &mut dyn Write) -> Outcome {
    let Some((program, rest)) = arguments.split_first() else { return Outcome::NotRunnable };
    let mut command = Command::new(program);
    command.args(rest).current_dir(root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let Ok(mut child) = command.spawn() else { return Outcome::NotRunnable };
    let group = libc::pid_t::try_from(child.id()).unwrap_or(0);
    RUNNING_GROUP.store(group, Ordering::SeqCst);
    let (sender, chunks) = sync_channel(64);
    forward(child.stdout.take(), sender.clone());
    forward(child.stderr.take(), sender);
    let mut output = Copied { out, room: limits.output, cut: false };
    let started = Instant::now();
    let outcome = loop {
        if !output.take(&chunks, POLL) {
            std::thread::sleep(POLL);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Outcome::Passed,
            Ok(Some(_)) => break Outcome::Failed,
            Ok(None) if started.elapsed() < limits.time => {}
            _ => {
                if group > 0 {
                    unsafe { libc::killpg(group, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                break Outcome::TimedOut;
            }
        }
    };
    RUNNING_GROUP.store(0, Ordering::SeqCst);
    let ended = Instant::now();
    while ended.elapsed() < LEFTOVER_OUTPUT_WAIT && output.take(&chunks, POLL) {}
    if output.cut {
        let limit = limits.output;
        let _ = writeln!(output.out, "{}", trf!("(output cut after {limit} bytes)", "(salida cortada a los {limit} bytes)"));
    }
    outcome
}

fn forward(pipe: Option<impl Read + Send + 'static>, chunks: SyncSender<Vec<u8>>) {
    let Some(mut pipe) = pipe else { return };
    std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        while let Ok(read) = pipe.read(&mut buffer) {
            if read == 0 || chunks.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
}

struct Copied<'a> {
    out: &'a mut dyn Write,
    room: usize,
    cut: bool,
}

impl Copied<'_> {
    /// Copies what arrives within `wait`, and says whether more can still arrive.
    fn take(&mut self, chunks: &Receiver<Vec<u8>>, wait: Duration) -> bool {
        match chunks.recv_timeout(wait) {
            Ok(chunk) => self.copy(&chunk),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
        loop {
            match chunks.try_recv() {
                Ok(chunk) => self.copy(&chunk),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn copy(&mut self, chunk: &[u8]) {
        let kept = chunk.len().min(self.room);
        let _ = self.out.write_all(&chunk[..kept]);
        self.room -= kept;
        self.cut |= kept < chunk.len();
    }
}

fn run_suites(root: &Path, project: &Project, component: &str, suite: Option<&str>, out: &mut dyn Write) -> Result<i32> {
    let plan = match project.plan(component, suite) {
        Ok(plan) => plan,
        Err(problem) => return answered(Err::<(), _>(problem), out, |(), _| Ok(())),
    };
    let mut failed = false;
    let _stops_with_us = StopsWithUs::new();
    for step in plan {
        let (suite, command) = (&step.suite, step.arguments.join(" "));
        writeln!(out, "{suite}: {command}")?;
        out.flush()?;
        let limits = Limits { time: Duration::from_secs(u64::from(step.timeout_minutes) * 60), output: OUTPUT_LIMIT };
        match execute(root, &step.arguments, &limits, out) {
            Outcome::Passed => match project.suite_passed(&step.component, &step.suite) {
                Ok(entry) => {
                    if !stored(root, &entry, out)? {
                        return Ok(UNREADABLE);
                    }
                    writeln!(out, "{}", trf!("{suite}: recorded", "{suite}: registrada"))?;
                }
                Err(problem) => return answered(Err::<(), _>(problem), out, |(), _| Ok(())),
            },
            Outcome::Failed => {
                failed = true;
                writeln!(out, "{}", trf!("{suite}: failed, nothing recorded", "{suite}: falló, no se registra nada"))?;
            }
            Outcome::TimedOut => {
                failed = true;
                let minutes = step.timeout_minutes;
                writeln!(out, "{}", trf!("{suite}: stopped after {minutes} minutes, nothing recorded", "{suite}: detenida a los {minutes} minutos, no se registra nada"))?;
            }
            Outcome::NotRunnable => {
                writeln!(out, "{}", trf!("{suite}: its command cannot be run", "{suite}: su comando no se puede ejecutar"))?;
                return Ok(UNREADABLE);
            }
        }
    }
    Ok(if failed { PENDING } else { CLEAN })
}

fn record(root: &Path, project: &Project, subject: &str, kind: RecordKind, verdict: RecordVerdict, evidence: &[PathBuf], out: &mut dyn Write) -> Result<i32> {
    let mut files = vec![];
    for path in evidence {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        files.push(Evidence { name, bytes: std::fs::read(path).with_context(|| path.display().to_string())? });
    }
    let kind = match kind {
        RecordKind::Adequate => Kind::Adequate,
        RecordKind::Judged => Kind::Judged,
        RecordKind::Reference => Kind::Reference,
    };
    let verdict = match verdict {
        RecordVerdict::Met => Verdict::Met,
        RecordVerdict::Unmet => Verdict::Unmet,
    };
    match project.verdict(subject, kind, verdict, &files) {
        Ok(entry) if stored(root, &entry, out)? => {
            writeln!(out, "{}", entry.path)?;
            Ok(CLEAN)
        }
        Ok(_) => Ok(UNREADABLE),
        Err(problem) => answered(Err::<(), _>(problem), out, |(), _| Ok(())),
    }
}

/// Writes a ledger entry and says so, or says why it was refused.
fn stored(root: &Path, entry: &Entry, out: &mut dyn Write) -> Result<bool> {
    let written = write_entry(root, entry)?;
    if !written {
        let (code, path) = (Code::PathOutsideRepository.as_str(), &entry.path);
        writeln!(out, "{}", trf!("{code}: {path}: it is reached through a link, nothing was written", "{code}: {path}: se llega por un enlace, no se escribió nada"))?;
    }
    Ok(written)
}

/// Writes a ledger entry without following a link: a folder on its way or a file in its place
/// that is a link is refused, so nothing outside the checkout is ever written.
fn write_entry(root: &Path, entry: &Entry) -> Result<bool> {
    let mut folders: Vec<&str> = entry.path.split('/').collect();
    let Some(name) = folders.pop() else { return Ok(false) };
    let mut folder = root.to_path_buf();
    for part in folders {
        folder.push(part);
        match std::fs::symlink_metadata(&folder) {
            Ok(found) if found.is_dir() => {}
            Ok(_) => return Ok(false),
            Err(_) => std::fs::create_dir(&folder)?,
        }
    }
    let target = folder.join(name);
    if std::fs::symlink_metadata(&target).is_ok_and(|found| !found.is_file()) {
        return Ok(false);
    }
    let staged = folder.join(format!(".{name}.{}", std::process::id()));
    std::fs::OpenOptions::new().write(true).create_new(true).open(&staged)?.write_all(&entry.bytes)?;
    std::fs::rename(staged, target)?;
    Ok(true)
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
