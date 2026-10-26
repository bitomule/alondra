//! `alondra report`: a crash report for whoever runs the alpha's report repository, built only
//! from what is known to be public, checked for anything private, and shown in full before it can
//! leave the Mac. It needs no daemon: it reads `~/.alondra` itself.
pub mod bundle;
pub mod canary;
pub mod collect;
pub mod daemon_log;
pub mod extract;
pub mod private;
pub mod redact;
pub mod templates;
pub mod time;
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
pub fn build(host: &dyn Host, window: Window, description: Option<String>) -> Result<Built> {
    let catalog = Catalog::built();
    let mut redactor = Redactor::new(&catalog.vocabulary);
    let collected = collect::collect(host, window, catalog, &mut redactor);
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

/// The armored file of a report already previewed, if its files are still those of the preview and
/// the final check finds nothing.
pub fn package(host: &dyn Host, id: &str) -> Result<String> {
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
            "the report cannot leave this Mac: {section}, line {line}, has {what}. Nothing was saved",
            "el informe no puede salir de este Mac: {section}, línea {line}, tiene {what}. No se ha guardado nada"
        ));
    }
    Ok(bundle::armored(id, &files))
}

pub fn default_file_name(id: &str) -> String {
    let date = id.split('-').nth(1).unwrap_or_default();
    format!("alondra-report-{date}.txt")
}

/// Writes the package of report `id` to `file`.
pub fn save(host: &dyn Host, id: &str, file: &Path) -> Result<()> {
    let armored = package(host, id)?;
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
    let dir = bundle::received_dir(home, &id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    for (name, text) in &files {
        std::fs::write(dir.join(name), text)?;
    }
    Ok((id, dir, files))
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
}

/// `alondra report`: builds a report, shows what it holds and lets the user read each section,
/// open its folder or save it as a file.
pub fn run(window_text: &str, description: Option<String>, json: bool, no_input: bool) -> Result<()> {
    use std::io::IsTerminal;
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
    let built = build(&host, window, description)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary_json(&built))?);
        return Ok(());
    }
    print!("{}", summary(&built));
    if no_input {
        return Ok(());
    }
    loop {
        let mut menu = cliclack::select(tr!("What now?", "¿Y ahora?"));
        menu = menu.item("view", tr!("Read a section", "Leer una sección"), "");
        menu = menu.item("folder", tr!("Open its folder", "Abrir su carpeta"), "");
        if built.hits.is_empty() {
            menu = menu.item("save", tr!("Save as a file", "Guardar como fichero"), tr!("to send it yourself", "para mandarlo tú"));
        }
        menu = menu.item("done", tr!("Done", "Listo"), tr!("the report stays on this Mac", "el informe se queda en este Mac"));
        match menu.interact()? {
            "view" => {
                let mut pick = cliclack::select(tr!("Which section?", "¿Qué sección?"));
                for (name, text) in &built.files {
                    pick = pick.item(name.as_str(), name.as_str(), size(text.len()));
                }
                let name = pick.interact()?;
                if let Some((_, text)) = built.files.iter().find(|(n, _)| n == name) {
                    print!("\n{text}\n");
                }
            }
            "folder" => {
                let _ = std::process::Command::new("/usr/bin/open").arg(&built.dir).status();
            }
            "save" => {
                let file = PathBuf::from(default_file_name(&built.id));
                save(&host, &built.id, &file)?;
                println!("{}", saved_notice(&file));
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
}

pub fn saved_notice(file: &Path) -> String {
    let file = file.display();
    trf!(
        "Saved in {file}. It is not encrypted: anyone who gets the file can read it, and it holds exactly what the preview showed. Send it to whoever asked for it; they open it with alondra report open {file}.",
        "Guardado en {file}. No va cifrado: cualquiera que reciba el fichero puede leerlo, y tiene exactamente lo que enseñó la vista previa. Mándaselo a quien te lo pidió; lo abre con alondra report open {file}."
    )
}

fn size(bytes: usize) -> String {
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
fn omitted_label(key: &str) -> String {
    match key {
        "daemon_log_lines_without_template" => tr!("log lines no template of the code has", "líneas del log que no son de ninguna plantilla del código").into(),
        "lifecycle_events_not_decoded" => tr!("lifecycle events it could not read", "eventos del ciclo de vida que no pudo leer").into(),
        "sdk_host_instances_with_unknown_names" => tr!("SDK hosts with a name it does not know", "hosts del SDK con un nombre que no conoce").into(),
        "sdk_host_frames_not_decoded" => tr!("SDK host frames it could not read", "mensajes del host del SDK que no pudo leer").into(),
        "sdk_host_frames_of_unknown_type" => tr!("SDK host frames of a type it does not know", "mensajes del host del SDK de un tipo que no conoce").into(),
        other => other.to_owned(),
    }
}

fn summary_json(built: &Built) -> Value {
    json!({
        "schema": "alondra.report/1",
        "id": built.id,
        "dir": built.dir,
        "compressed_bytes": built.compressed,
        "manifest": manifest_of(&built.files),
        "check": {"passed": built.hits.is_empty(), "hits": built.hits},
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
