//! `alondra report`: a crash report for whoever runs the alpha's report repository, built only
//! from what is known to be public, checked for anything private, and shown in full before it can
//! leave the Mac. It needs no daemon: it reads `~/.alondra` itself.
pub mod bundle;
pub mod canary;
pub mod collect;
pub mod daemon_log;
pub mod extract;
pub mod github;
pub mod private;
pub mod redact;
pub mod send;
pub mod templates;
pub mod time;
#[cfg(test)]
mod send_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use self::bundle::{Draft, Files};
use self::canary::Hit;
use self::collect::{Host, Status, Window};
use self::redact::Redactor;
use self::templates::Catalog;

/// The most an app's own section may weigh.
pub const MAX_APP_SECTION: usize = 512 * 1024;

/// What `alondra report` built: where it is and what the final check found.
pub struct Built {
    pub id: String,
    pub dir: PathBuf,
    pub files: Files,
    pub sections: Vec<collect::Section>,
    pub hits: Vec<Hit>,
    pub compressed: usize,
}

pub fn window(text: &str, now: i64) -> Result<Window> {
    let hours = match text {
        "24h" => 24,
        "72h" | "3d" => 72,
        "7d" => 168,
        _ => bail!(trf!("{text} is not a window: use 24h, 72h or 7d", "{text} no es una ventana: usa 24h, 72h o 7d")),
    };
    Ok(Window { from: now - i64::from(hours) * 3_600_000, to: now, hours })
}

/// Collects, redacts, checks and writes a new report into `<home>/reports/<id>/`.
pub fn build(host: &dyn Host, window: Window, description: Option<String>, app: Option<&str>) -> Result<Built> {
    let catalog = Catalog::built();
    let mut redactor = Redactor::new(&catalog.vocabulary);
    let collected = collect::collect(host, window, catalog, &mut redactor, app);
    let now = host.now();
    let mut draft = Draft {
        id: bundle::new_id(now),
        created_at: now,
        window,
        sections: collected.sections,
        description,
        omitted: collected.omitted,
        log_capped: collected.log_capped,
        daemon_running: collected.daemon_running,
    };
    let (files, _) = bundle::assemble(&mut draft);
    let hits = final_check(host, &files, &redactor.pseudonyms, catalog);
    let reports = host.home().join("reports");
    bundle::prune(&reports, now);
    let dir = reports.join(&draft.id);
    bundle::write(&dir, &files, &redactor.pseudonyms)?;
    let compressed = bundle::compressed(&files).len();
    Ok(Built { id: draft.id, dir, files, sections: draft.sections, hits, compressed })
}

fn final_check(host: &dyn Host, files: &Files, pseudonyms: &redact::Pseudonyms, catalog: &Catalog) -> Vec<Hit> {
    let mut private = private::of_this_mac(host, &catalog.vocabulary);
    for original in pseudonyms.originals() {
        private.add("a value the report replaced", original, &catalog.vocabulary);
    }
    bundle::check(files, &private)
}

/// The files of a report already previewed, if they are still those of the preview and the final
/// check finds nothing.
pub fn checked(host: &dyn Host, id: &str) -> Result<Files> {
    if !bundle::is_id(id) {
        bail!(trf!("{id} is not a report id; alondra report prints it", "{id} no es el id de un informe; alondra report lo muestra"));
    }
    let dir = host.home().join("reports").join(id);
    if !dir.is_dir() {
        bail!(trf!("there is no report {id} on this Mac; create one with alondra report", "no hay ningún informe {id} en este Mac; crea uno con alondra report"));
    }
    let files = bundle::read(&dir)?;
    let pseudonyms: redact::Pseudonyms = std::fs::read(dir.join(bundle::PSEUDONYMS)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let hits = final_check(host, &files, &pseudonyms, Catalog::built());
    if let Some(hit) = hits.first() {
        let (section, line, what) = (&hit.section, hit.line, &hit.what);
        bail!(trf!(
            "the report cannot leave this Mac: {section}, line {line}, has {what}. Nothing was saved or sent",
            "el informe no puede salir de este Mac: {section}, línea {line}, tiene {what}. No se ha guardado ni enviado nada"
        ));
    }
    Ok(files)
}

pub fn default_file_name(id: &str) -> String {
    let date = id.split('-').nth(1).unwrap_or_default();
    format!("alondra-report-{date}.txt")
}

/// Writes the package of report `id` to `file`.
pub fn save(host: &dyn Host, id: &str, file: &Path) -> Result<()> {
    let armored = bundle::armored(id, &checked(host, id)?);
    use std::io::Write;
    let shown = file.display();
    let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(file).with_context(|| trf!("cannot create {shown}", "no se puede crear {shown}"))?;
    out.write_all(armored.as_bytes())?;
    Ok(())
}

/// Opens a saved report into `<home>/reports/received/<id>/` and returns its id, folder and files.
pub fn open_file(home: &Path, file: &Path) -> Result<(String, PathBuf, Files)> {
    let shown = file.display();
    let text = std::fs::read_to_string(file).with_context(|| trf!("cannot read {shown}", "no se puede leer {shown}"))?;
    let (id, files) = bundle::unarmored(&text)?;
    let dir = keep_received(home, &id, &files)?;
    Ok((id, dir, files))
}

fn keep_received(home: &Path, id: &str, files: &Files) -> Result<PathBuf> {
    let dir = bundle::received_dir(home, id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (name, text) in files {
        std::fs::write(dir.join(name), text)?;
    }
    Ok(dir)
}

/// Where the relay's announcement was last kept, for when the relay cannot be asked.
const ANNOUNCED: &str = "announced.json";

/// The report repository: `chosen` (`report.repo`) when this Mac has one, else the one the relay
/// announces in `/v1/bootstrap` (`ask` asks the relay), else the last one that relay announced.
pub fn repo_name(home: &Path, chosen: Option<String>, ask: &dyn Fn(&str) -> Option<Value>) -> Option<String> {
    if let Some(repo) = chosen {
        return Some(repo);
    }
    let relay = relay_origin(home);
    let reports = home.join("reports");
    match ask(&relay) {
        Some(body) => {
            let repo = body["report_repo"].as_str().filter(|r| alondra_core::config::is_github_repo(r)).map(str::to_owned);
            let _ = std::fs::create_dir_all(&reports);
            let _ = std::fs::write(reports.join(ANNOUNCED), json!({"relay": relay, "report_repo": repo}).to_string());
            repo
        }
        None => {
            let kept: Value = serde_json::from_slice(&std::fs::read(reports.join(ANNOUNCED)).ok()?).ok()?;
            if kept["relay"].as_str() != Some(relay.as_str()) {
                return None;
            }
            kept["report_repo"].as_str().map(str::to_owned)
        }
    }
}

fn relay_origin(home: &Path) -> String {
    std::fs::read_to_string(home.join("service.toml"))
        .ok()
        .and_then(|text| toml::from_str::<toml::Table>(&text).ok())
        .and_then(|table| table.get("relay_url").and_then(|v| v.as_str()).map(|u| u.trim_end_matches('/').to_owned()))
        .unwrap_or_else(|| crate::service_client::RELAY.to_owned())
}

fn bootstrap(relay: &str) -> Option<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build())
        .timeout_global(Some(Duration::from_secs(5)))
        .max_redirects(0)
        .build()
        .into();
    agent.get(format!("{relay}/v1/bootstrap")).call().ok()?.body_mut().with_config().limit(4096).read_json().ok()
}

fn repo_of(host: &dyn Host) -> Result<String> {
    host.report_repo().ok_or_else(|| send::Blocked::NoRepo.into())
}

/// `alondra report send`: sends report `id`, already previewed, to the report repository.
pub fn send_report(id: &str, kind: Option<send::Kind>, retry: bool, json: bool) -> Result<()> {
    let host = Mac::new();
    let sent = checked(&host, id).and_then(|files| {
        let gh = github::Gh::default();
        let repo = github::Repo { gh: &gh, name: repo_of(&host)? };
        let outcome = send::send(&repo, host.home(), id, &files, kind, host.now(), &backoff)?;
        Ok((repo.name, outcome))
    });
    if retry {
        return sent.map(drop);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&sent_json(id, &sent))?);
        return Ok(());
    }
    let (repo, outcome) = sent?;
    println!("{}", outcome_notice(&repo, &outcome));
    Ok(())
}

/// What `alondra report send --json` prints: sent, pending, or refused with why, so whoever asked
/// gets the answer in the same document either way.
pub fn sent_json(id: &str, sent: &Result<(String, send::Outcome)>) -> Value {
    match sent {
        Ok((repo, outcome @ send::Outcome::Sent { url, .. })) => {
            json!({"schema": "alondra.report-send/1", "id": id, "state": "sent", "url": url, "message": outcome_notice(repo, outcome)})
        }
        Ok((repo, outcome @ send::Outcome::Pending(_))) => {
            json!({"schema": "alondra.report-send/1", "id": id, "state": "pending", "message": outcome_notice(repo, outcome)})
        }
        Err(error) => json!({"schema": "alondra.report-send/1", "id": id, "state": "refused", "message": format!("{error}")}),
    }
}

/// How long a pending report waits; seconds in a sandbox with fast timing.
fn backoff(attempts: u32) -> i64 {
    if alondra_core::config::Config::load().is_ok_and(|c| c.fast_timing) { 1_000 } else { send::backoff_ms(attempts) }
}

pub fn outcome_notice(repo: &str, outcome: &send::Outcome) -> String {
    match outcome {
        send::Outcome::Sent { url, .. } => trf!(
            "Sent: {url}. Its issue in {repo} shows the version, macOS and the kind of failure, and links the report: anyone who sees it can read the report.",
            "Enviado: {url}. Su issue en {repo} enseña la versión, macOS y el tipo de fallo, y enlaza el informe: cualquiera que lo vea puede leerlo."
        ),
        send::Outcome::Pending(why) => trf!(
            "GitHub cannot take it now ({why}). It stays pending on this Mac and Alondra sends it on its own when GitHub answers again.",
            "GitHub no puede recibirlo ahora ({why}). Queda pendiente en este Mac y Alondra lo envía sola cuando GitHub vuelva a responder."
        ),
    }
}

/// `alondra report list`: the open reports of the report repository.
pub fn list_reports(json: bool) -> Result<()> {
    let host = Mac::new();
    let gh = github::Gh::default();
    let repo = github::Repo { gh: &gh, name: repo_of(&host)? };
    let reports = open_reports(&repo)?;
    if json {
        let rows: Vec<Value> = reports
            .iter()
            .map(|(i, p)| json!({"number": i.number, "url": i.url, "author": i.author, "created_at": i.created_at, "report": p.id, "alondra": p.alondra, "macos": p.macos, "failure": p.kind.word()}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema": "alondra.reports/1", "repo": repo.name, "reports": rows}))?);
        return Ok(());
    }
    print!("{}", reports_text(&repo.name, &reports));
    Ok(())
}

/// The open issues of `repo` that are crash reports, newest first.
pub fn open_reports(repo: &github::Repo) -> Result<Vec<(github::Issue, send::Public)>> {
    let issues = repo.issues("state=open")?;
    Ok(issues.into_iter().filter(|i| i.open && i.title.starts_with(send::TITLE_PREFIX)).filter_map(|i| send::Public::parse(&i.body).map(|p| (i, p))).collect())
}

pub fn reports_text(repo: &str, reports: &[(github::Issue, send::Public)]) -> String {
    if reports.is_empty() {
        return trf!("{repo} has no open reports.\n", "{repo} no tiene informes abiertos.\n");
    }
    let mut out = String::new();
    for (issue, public) in reports {
        let date = issue.created_at.get(..10).unwrap_or(&issue.created_at);
        out.push_str(&format!("#{:<5} {:<20} {date}  Alondra {:<8} macOS {:<7} {}\n", issue.number, issue.author, public.alondra, public.macos, public.kind.label()));
    }
    out.push_str(&format!("\n{}\n", tr!("Open one with alondra report open <number>.", "Abre uno con alondra report open <número>.")));
    out
}

/// `alondra report open <number|URL|file>`.
pub fn open_report(target: &str) -> Result<()> {
    let host = Mac::new();
    if Path::new(target).is_file() {
        let (id, dir, files) = open_file(host.home(), Path::new(target))?;
        print!("{}", received_summary(&id, &dir, &files));
        return Ok(());
    }
    let gh = github::Gh::default();
    let (repo, issue) = found_issue(&host, target, &gh)?;
    let (id, dir, files) = open_issue(host.home(), &repo, &issue)?;
    let (number, author) = (issue.number, issue.author.as_str());
    println!("{}", trf!("Issue #{number}, sent by {author}", "Issue #{number}, enviado por {author}"));
    print!("{}", received_summary(&id, &dir, &files));
    Ok(())
}

/// Downloads the report `issue` names into `<home>/reports/received/<id>/`.
pub fn open_issue(home: &Path, repo: &github::Repo, issue: &github::Issue) -> Result<(String, PathBuf, Files)> {
    let (public, file) = send::download(repo, issue)?;
    let (id, files) = bundle::unarmored(&file)?;
    if id != public.id {
        bail!(tr!("the report on GitHub is not the one its issue names", "el informe en GitHub no es el que nombra su issue"));
    }
    let dir = keep_received(home, &id, &files)?;
    Ok((id, dir, files))
}

fn found_issue<'a>(host: &dyn Host, target: &str, gh: &'a dyn github::Github) -> Result<(github::Repo<'a>, github::Issue)> {
    let Some((number, named)) = send::issue_ref(target) else {
        bail!(trf!("{target} is not a file, an issue number or an issue URL", "{target} no es un fichero, ni un número ni una URL de issue"));
    };
    let name = match named {
        Some(name) => name,
        None => repo_of(host)?,
    };
    let repo = github::Repo { gh, name };
    let issue = repo.issue(number)?.with_context(|| {
        let name = repo.name.as_str();
        trf!("{name} has no issue {number}", "{name} no tiene el issue {number}")
    })?;
    Ok((repo, issue))
}

/// `alondra report delete <number|URL>`: closes a report's issue and, when you sent it, deletes
/// its gist.
pub fn delete_report(target: &str) -> Result<()> {
    let host = Mac::new();
    let gh = github::Gh::default();
    let (repo, issue) = found_issue(&host, target, &gh)?;
    println!("{}", delete_issue_report(&repo, &issue)?);
    Ok(())
}

pub fn delete_issue_report(repo: &github::Repo, issue: &github::Issue) -> Result<String> {
    let Some(public) = send::Public::parse(&issue.body) else {
        let number = issue.number;
        bail!(trf!("issue {number} is not an Alondra crash report", "el issue {number} no es un informe de fallo de Alondra"));
    };
    let login = repo.login()?;
    let gist = repo.gist(send::gist_id(&public.gist))?;
    let deleted = match gist {
        Some(gist) if gist.owner == login => repo.delete_gist(&gist.id)?,
        _ => false,
    };
    if issue.open {
        repo.close_issue(issue.number)?;
    }
    let (number, url, gist, author) = (issue.number, issue.url.as_str(), public.gist.as_str(), issue.author.as_str());
    Ok(if deleted {
        trf!("Deleted the report's gist and closed issue #{number}: {url}", "Borrado el gist del informe y cerrado el issue #{number}: {url}")
    } else if issue.author == login {
        trf!("Closed issue #{number}; its gist was already deleted: {url}", "Cerrado el issue #{number}; su gist ya estaba borrado: {url}")
    } else {
        trf!(
            "Closed issue #{number}: {url}. Its gist {gist} belongs to {author}: only they can delete it, with alondra report delete {number}",
            "Cerrado el issue #{number}: {url}. Su gist {gist} es de {author}: solo puede borrarlo esa cuenta, con alondra report delete {number}"
        )
    })
}

/// The real Mac.
pub struct Mac {
    home: PathBuf,
}

impl Mac {
    pub fn new() -> Mac {
        Mac { home: alondra_core::config::alondra_home(&|k: &str| std::env::var(k).ok()) }
    }
}

impl Host for Mac {
    fn home(&self) -> &Path {
        &self.home
    }

    fn now(&self) -> i64 {
        alondra_core::now_millis()
    }

    fn daemon(&self) -> Option<Value> {
        crate::client::Client::new(Duration::from_millis(500)).get::<Value>("/daemon").ok()
    }

    fn doctor(&self) -> Option<Vec<(String, String)>> {
        let sys = crate::doctor::system::Real::new();
        let env = crate::doctor::Env { sys: &sys, deep: false, first_run: false, yes: false, select: crate::doctor::Selection::Skip(vec!["validate".into()]) };
        Some(crate::doctor::diagnose(&env).into_iter().map(|o| (o.id.to_owned(), o.status.word().to_owned())).collect())
    }

    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn bin_dir(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()?.canonicalize().ok()?.parent().map(Path::to_path_buf)
    }

    fn service_label(&self) -> String {
        alondra_core::config::Config::load().map(|c| c.service_label).unwrap_or_else(|_| alondra_core::config::Config::defaults().service_label)
    }

    fn uid(&self) -> u32 {
        unsafe { libc::getuid() }
    }

    fn report_repo(&self) -> Option<String> {
        repo_name(&self.home, alondra_core::config::Config::load().ok().and_then(|c| c.report_repo), &bootstrap)
    }
}

/// `alondra report`: builds a report, shows what it holds and asks whether to send it.
pub fn run(window_text: &str, description: Option<String>, app: Option<&Path>, kind: Option<send::Kind>, json: bool, no_input: bool) -> Result<()> {
    use std::io::{IsTerminal, Read};
    let app = match app {
        None => None,
        Some(path) if path == Path::new("-") => {
            let mut text = String::new();
            std::io::stdin().take(MAX_APP_SECTION as u64 + 1).read_to_string(&mut text).context("reading the app's section")?;
            Some(text)
        }
        Some(path) => Some(std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?),
    };
    if app.as_ref().is_some_and(|text| text.len() > MAX_APP_SECTION) {
        bail!(tr!("the app's section is larger than 512 KB", "la sección de la app pasa de 512 KB"));
    }
    let no_input = no_input || !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());
    let host = Mac::new();
    let window = window(window_text, host.now())?;
    let description = match description {
        Some(text) => Some(text),
        None if json || no_input => None,
        None => {
            let text: String = cliclack::input(tr!("What happened? (optional; it goes as you write it)", "¿Qué pasó? (opcional; va tal cual lo escribas)"))
                .placeholder(tr!("A node's session ended on its own after a long wait", "La sesión de un nodo terminó sola tras esperar mucho"))
                .required(false)
                .interact()?;
            Some(text).filter(|t| !t.trim().is_empty())
        }
    };
    let collecting = (!json && !no_input).then(cliclack::spinner);
    if let Some(spinner) = &collecting {
        spinner.start(tr!("Collecting the report: the logs, the tasks and the nodes of the last hours…", "Recogiendo el informe: los logs, las tareas y los nodos de las últimas horas…"));
    }
    let built = build(&host, window, description, app.as_deref());
    if let Some(spinner) = collecting {
        spinner.clear();
    }
    let built = built?;
    if json {
        let repo = if built.hits.is_empty() { host.report_repo() } else { None };
        println!("{}", serde_json::to_string_pretty(&summary_json(&built, repo.as_deref()))?);
        return Ok(());
    }
    print!("{}", summary(&built));
    if no_input {
        return Ok(());
    }
    if !built.hits.is_empty() {
        return Ok(());
    }
    let id = built.id.as_str();
    let Some(repo) = host.report_repo() else {
        println!("{}", trf!(
            "This Mac knows no report repository, so it stays here. To save it as a file: alondra report save {id}",
            "Este Mac no conoce ningún repositorio de informes, así que se queda aquí. Para guardarlo como fichero: alondra report save {id}"
        ));
        return Ok(());
    };
    let question = trf!("Send it to {repo}? Anyone who sees its issue will be able to read it.", "¿Lo envío a {repo}? Cualquiera que vea el issue podrá leerlo.");
    let send_it = cliclack::select(question)
        .item(true, tr!("Yes, send it", "Sí, enviarlo"), "")
        .item(false, tr!("No, keep it on this Mac", "No, que se quede en este Mac"), "")
        .interact()?;
    let later = trf!("To send it later: alondra report send {id}", "Para enviarlo luego: alondra report send {id}");
    if !send_it {
        println!("{} {later}", tr!("It stays on this Mac.", "Se queda en este Mac."));
        return Ok(());
    }
    let gh = github::Gh::default();
    let target = github::Repo { gh: &gh, name: repo.clone() };
    let sending = cliclack::spinner();
    sending.start(trf!("Sending it to {repo}…", "Enviándolo a {repo}…"));
    let sent = checked(&host, id).and_then(|files| send::send(&target, host.home(), id, &files, kind, host.now(), &backoff));
    sending.clear();
    match sent {
        Ok(outcome) => println!("{}", outcome_notice(&repo, &outcome)),
        Err(error) => println!("{error}. {} {later}", tr!("Nothing was sent; it stays on this Mac.", "No se ha enviado nada; se queda en este Mac.")),
    }
    Ok(())
}

pub fn saved_notice(file: &Path) -> String {
    let file = file.display();
    trf!(
        "Saved in {file}. It is not encrypted: anyone who gets the file can read it, and it holds exactly what the preview showed. Send it to whoever asked for it; they open it with alondra report open {file}.",
        "Guardado en {file}. No va cifrado: cualquiera que reciba el fichero puede leerlo, y tiene exactamente lo que enseñó la vista previa. Mándaselo a quien te lo pidió; lo abre con alondra report open {file}."
    )
}

pub(crate) fn size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes} B") } else if bytes < 1024 * 1024 { format!("{} KB", bytes.div_ceil(1024)) } else { format!("{:.1} MB", bytes as f64 / 1_048_576.0) }
}

fn manifest_of(files: &Files) -> Value {
    files.iter().find(|(n, _)| n == bundle::MANIFEST).and_then(|(_, t)| serde_json::from_str(t).ok()).unwrap_or(Value::Null)
}

pub fn summary(built: &Built) -> String {
    let manifest = manifest_of(&built.files);
    let hours = manifest["window"]["hours"].as_u64().unwrap_or_default();
    let (id, compressed, dir) = (&built.id, size(built.compressed), built.dir.display());
    let mut out = trf!(
        "Crash report {id} · last {hours} hours · {compressed} compressed\nIt is in {dir}; nothing has left this Mac.\n\n",
        "Informe de fallo {id} · últimas {hours} horas · {compressed} comprimido\nEstá en {dir}; no ha salido nada de este Mac.\n\n"
    );
    for section in &built.sections {
        let line = match (&section.status, built.files.iter().find(|(n, _)| n == section.name)) {
            (_, Some((_, text))) => format!("  {:<22}{:>9}\n", section.name, size(text.len())),
            (Status::Empty(reason), None) => format!("  {:<22}{:>9}  {reason}\n", section.name, "—"),
            (Status::Ok, None) => format!("  {:<22}{:>9}\n", section.name, "—"),
        };
        out.push_str(&line);
    }
    if let Some(omitted) = manifest["omitted"].as_object().filter(|o| o.values().any(|v| v.as_u64().unwrap_or_default() > 0)) {
        out.push_str(&format!("\n{}\n", tr!("Left out:", "Fuera:")));
        for (what, count) in omitted.iter().filter(|(_, c)| c.as_u64().unwrap_or_default() > 0) {
            out.push_str(&format!("  {}: {count}\n", omitted_label(what)));
        }
    }
    for trim in manifest["trimmed"].as_array().into_iter().flatten() {
        let (lines, section) = (trim["lines"].as_u64().unwrap_or_default(), trim["section"].as_str().unwrap_or_default());
        out.push_str(&trf!("Cut to fit in 5 MB: the oldest {lines} lines of {section}\n", "Recortado para que quepa en 5 MB: las {lines} líneas más antiguas de {section}\n"));
    }
    out.push('\n');
    if built.hits.is_empty() {
        out.push_str(tr!("The final check found nothing private.\n", "La comprobación final no encontró nada privado.\n"));
    } else {
        out.push_str(tr!("The report cannot leave this Mac; the final check found:\n", "El informe no puede salir de este Mac; la comprobación final encontró:\n"));
        for hit in &built.hits {
            let (section, line, what) = (&hit.section, hit.line, &hit.what);
            out.push_str(&trf!("  {section}, line {line}: {what}\n", "  {section}, línea {line}: {what}\n"));
        }
    }
    out
}

/// What a counter of the manifest's `omitted` counts, for the user.
pub(crate) fn omitted_label(key: &str) -> String {
    match key {
        "daemon_log_lines_without_template" => tr!("log lines no template of the code has", "líneas del log que no son de ninguna plantilla del código").into(),
        "lifecycle_events_not_decoded" => tr!("lifecycle events it could not read", "eventos del ciclo de vida que no pudo leer").into(),
        "sdk_host_instances_with_unknown_names" => tr!("SDK hosts with a name it does not know", "hosts del SDK con un nombre que no conoce").into(),
        "sdk_host_frames_not_decoded" => tr!("SDK host frames it could not read", "mensajes del host del SDK que no pudo leer").into(),
        "sdk_host_frames_of_unknown_type" => tr!("SDK host frames of a type it does not know", "mensajes del host del SDK de un tipo que no conoce").into(),
        other => other.to_owned(),
    }
}

/// `repo` is where it would be sent, when the final check passed and this Mac knows one; `summary`
/// is the text `alondra report` shows, for a screen to show the same.
pub fn summary_json(built: &Built, repo: Option<&str>) -> Value {
    json!({
        "schema": "alondra.report/1",
        "id": built.id,
        "dir": built.dir,
        "compressed_bytes": built.compressed,
        "manifest": manifest_of(&built.files),
        "check": {"passed": built.hits.is_empty(), "hits": built.hits},
        "repo": repo,
        "summary": summary(built),
    })
}

/// What `alondra report open` prints: the user's words, the versions, the checks that did not pass,
/// the failures in the lifecycle journal and what was left out.
pub fn received_summary(id: &str, dir: &Path, files: &Files) -> String {
    let get = |name: &str| files.iter().find(|(n, _)| n == name).map(|(_, t)| t.as_str());
    let json_of = |name: &str| get(name).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
    let manifest = json_of(bundle::MANIFEST);
    let versions = json_of("versions.json");
    let machine = json_of("machine.json");
    let created = manifest["created_at"].as_str().unwrap_or_default();
    let mut out = trf!("Report {id}, created {created}\n", "Informe {id}, creado {created}\n");
    out.push_str(&format!("{}\n", get(bundle::DESCRIPTION).map_or_else(|| tr!("(no description)", "(sin descripción)").to_owned(), |d| format!("«{}»", d.trim()))));
    let text = |v: &Value| v.as_str().map(str::to_owned).unwrap_or_else(|| "?".into());
    out.push_str(&format!(
        "\nalondra {} · alondrad {} · Claude {} · macOS {}\n",
        text(&versions["cli"]),
        text(&versions["daemon"]),
        text(&versions["claude"]),
        text(&machine["macos"])
    ));
    if versions["daemon_running"] == false {
        out.push_str(tr!("The daemon was not running.\n", "El daemon no estaba en marcha.\n"));
    }
    let failing: Vec<String> = json_of("doctor.json")["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| format!("{} ({})", text(&c["id"]), text(&c["status"])))
        .collect();
    if !failing.is_empty() {
        out.push_str(&format!("Doctor: {}\n", failing.join(", ")));
    }
    let failures: Vec<String> = get("lifecycle.jsonl")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| matches!(e["fact"].as_str(), Some("host_ended" | "connection_lost" | "input_dropped")))
        .map(|e| format!("  {} {} #{}", text(&e["task"]), text(&e["fact"]), e["revision"]))
        .collect();
    if !failures.is_empty() {
        out.push_str(&format!("\n{}\n{}\n", tr!("Failures in the lifecycle journal:", "Fallos en el diario de ciclo de vida:"), failures.join("\n")));
    }
    if let Some(omitted) = manifest["omitted"].as_object().filter(|o| !o.is_empty()) {
        let parts: Vec<String> = omitted.iter().map(|(k, v)| format!("{}: {v}", omitted_label(k))).collect();
        out.push_str(&format!("\n{} {}\n", tr!("Left out:", "Fuera:"), parts.join(", ")));
    }
    let dir = dir.display();
    out.push_str(&trf!("\nIts files are in {dir}\n", "\nSus ficheros están en {dir}\n"));
    out
}
