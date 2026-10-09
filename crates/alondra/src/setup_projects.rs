//! Select the directory used to discover and start projects; not a filesystem sandbox.
use std::{fs, io::{BufRead, Write}, path::{Path, PathBuf}};
use anyhow::{Context, Result, bail};
use alondra_core::options::OptionsFile;
use serde::Deserialize;

#[derive(Deserialize)]
struct RootValue { projects_root: Option<toml::Spanned<String>> }

pub struct Selection {
    file: PathBuf,
    original: Option<String>,
    root: PathBuf,
    write: bool,
}

pub(crate) fn resolve(value: &Path, home: &Path, cwd: &Path) -> Result<PathBuf> {
    if value.as_os_str().is_empty() { bail!(tr!("Projects directory cannot be empty", "la carpeta de proyectos no puede estar vacía")); }
    let path = if value == Path::new("~") { home.to_owned() }
        else if let Ok(rest) = value.strip_prefix("~/") { home.join(rest) }
        else if value.is_absolute() { value.to_owned() } else { cwd.join(value) };
    match fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(path.canonicalize()?),
        Ok(_) => {
            let shown = path.display();
            bail!(trf!("{shown} is not a directory; choose another --projects-root", "{shown} no es un directorio; elige otro --projects-root"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Check existing ancestors now, before any auth or writes.
            let mut ancestor = path.as_path();
            while !ancestor.exists() { ancestor = ancestor.parent().context("No existing parent for projects directory")?; }
            if !ancestor.is_dir() {
                let shown = ancestor.display();
                bail!(trf!("{shown} is not a directory", "{shown} no es un directorio"));
            }
            Ok(path)
        }
        Err(e) => Err(e).with_context(|| format!("Cannot access {}", path.display())),
    }
}

fn with_root(original: &str, root: &Path) -> Result<String> {
    let value = root.to_str().context("Projects directory must be valid UTF-8")?;
    let encoded = toml::Value::String(value.to_owned()).to_string();
    let parsed: RootValue = toml::from_str(original)?;
    let mut text = original.to_owned();
    if let Some(existing) = parsed.projects_root {
        text.replace_range(existing.span(), &encoded);
    } else { text = format!("projects_root = {encoded}\n{text}"); }
    OptionsFile::parse(&text).map_err(anyhow::Error::msg)?;
    Ok(text)
}

pub fn select(
    file: PathBuf, home: &Path, cwd: &Path, explicit: Option<&Path>, no_input: bool,
    dry_run: bool, input: &mut impl BufRead, output: &mut impl Write,
) -> Result<Selection> {
    match fs::symlink_metadata(&file) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            let shown = file.display();
            bail!(trf!("{shown} must be a regular file; repair it before setup", "{shown} debe ser un archivo normal; repáralo antes de la configuración"))
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let original = match fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let options = OptionsFile::parse(original.as_deref().unwrap_or(""))
        .map_err(anyhow::Error::msg).with_context(|| format!("Repair {} before setup; it was not changed", file.display()))?;
    let existing = options.projects_root.as_deref();
    if explicit.is_none() && let Some(value) = existing {
        match resolve(Path::new(value), home, cwd) {
            Ok(root) if root.is_dir() => {
                let write = root.to_str() != Some(value);
                return Ok(Selection { file, original, root, write });
            }
            result => {
                let why = result.err().map(|e| e.to_string()).unwrap_or_else(|| format!("{value} does not exist"));
                if no_input || dry_run { bail!(trf!("Configured projects_root is invalid: {why}. Rerun setup --projects-root <directory> to repair it", "El projects_root configurado no es válido: {why}. Vuelve a ejecutar setup --projects-root <directorio> para repararlo")); }
                writeln!(output, "Configured projects_root is invalid: {why}.")?;
            }
        }
    }
    let root = if let Some(path) = explicit { resolve(path, home, cwd)? }
    else if no_input || dry_run { resolve(&home.join("Projects"), home, cwd)? }
    else {
        writeln!(output, "{}", tr!("Choose the folder containing your projects. Alondra uses it to discover projects and start nodes, and trusts the repositories in it for Claude Code when it starts work there, as if you accepted Claude Code's trust prompt in each; it does not limit Claude's permissions.", "Elige la carpeta que contiene tus proyectos. Alondra la usa para descubrir proyectos y arrancar nodos, y marca como de confianza en Claude Code los repositorios que contiene cuando arranca trabajo en ellos, como si aceptaras el aviso de confianza de Claude Code en cada uno; no limita los permisos de Claude."))?;
        loop {
            write!(output, "{} [{}]: ", tr!("Projects folder (created if missing; :cancel to stop)", "Carpeta de proyectos (se crea si falta; :cancel para salir)"), home.join("Projects").display())?;
            output.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 || line.trim() == ":cancel" { bail!(tr!("Setup cancelled before authentication or changes", "Configuración cancelada antes de autenticar o hacer cambios")); }
            let value = line.trim();
            let proposed = if value.is_empty() { home.join("Projects") } else { PathBuf::from(value) };
            match resolve(&proposed, home, cwd) {
                Ok(root) => break root,
                Err(e) => writeln!(output, "{e}")?,
            }
        }
    };
    // Validate serialization before creating the chosen directory.
    with_root(original.as_deref().unwrap_or(""), &root)?;
    Ok(Selection { file, original, root, write: true })
}

impl Selection {
    pub fn apply(self, dry_run: bool) -> Result<()> {
        if dry_run {
            println!("would use projects root: {}{}", self.root.display(), if self.root.exists() { "" } else { " (create directory)" });
            return Ok(());
        }
        if self.write {
            let canonical = self.store()?;
            println!("Projects root: {}", canonical.display());
        } else { println!("Projects root: {} (unchanged)", self.root.display()); }
        Ok(())
    }

    /// Writes the chosen folder into options.toml, creating it if missing; returns it canonical.
    pub(crate) fn store(self) -> Result<PathBuf> {
        // Refuse overwriting options edited while the user was selecting a directory.
        let current = match fs::read_to_string(&self.file) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if current != self.original {
            let shown = self.file.display();
            bail!(trf!("{shown} changed during setup; rerun setup", "{shown} cambió durante la configuración; vuelve a ejecutar setup"));
        }
        fs::create_dir_all(&self.root).with_context(|| format!("Cannot create projects directory {}", self.root.display()))?;
        let canonical = self.root.canonicalize()?;
        let text = with_root(self.original.as_deref().unwrap_or(""), &canonical)?;
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700)
            .create(self.file.parent().context("Options parent")?)?;
        crate::service_client::atomic_write(&self.file, text.as_bytes())?;
        Ok(canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(crate::test_support::Directory);
    impl Fixture {
        fn new() -> Self {
            let dir = crate::test_support::Directory::new();
            fs::create_dir_all(&dir).unwrap(); Self(dir)
        }
        fn file(&self) -> PathBuf { self.0.join("config/options.toml") }
        fn choose(&self, explicit: Option<&Path>, no_input: bool, dry: bool, input: &str) -> Result<Selection> {
            select(self.file(), &self.0, &self.0, explicit, no_input, dry, &mut input.as_bytes(), &mut Vec::new())
        }
        fn write(&self, text: &str) { fs::create_dir_all(self.file().parent().unwrap()).unwrap(); fs::write(self.file(), text).unwrap(); }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    #[test]
    fn preserves_comments_multiline_value_quoted_key_and_other_settings() {
        let original = "# keep\n'projects_root' = '''\n/old\n''' # inline\n[defaults]\neffort = 'hard'\n[issues]\nowners = ['me']\n";
        let updated = with_root(original, Path::new("/new folder")).unwrap();
        assert_eq!(updated, "# keep\n'projects_root' = \"/new folder\" # inline\n[defaults]\neffort = 'hard'\n[issues]\nowners = ['me']\n");
        assert_eq!(OptionsFile::parse(&updated).unwrap().issues.owners, ["me"]);
        assert_eq!(with_root("# keep\n[issues]\nowners = ['me']\n", Path::new("/new")).unwrap(), "projects_root = \"/new\"\n# keep\n[issues]\nowners = ['me']\n");
    }
    #[test]
    fn default_no_input_creates_and_second_run_never_reads_stdin_or_writes() {
        let f = Fixture::new();
        f.choose(None, true, false, "").unwrap().apply(false).unwrap();
        assert!(f.0.join("Projects").is_dir());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(f.file().parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(f.file()).unwrap().permissions().mode() & 0o777, 0o600);
        let before = fs::read_to_string(f.file()).unwrap();
        let next = f.choose(None, false, false, "").unwrap();
        assert!(!next.write); next.apply(false).unwrap();
        assert_eq!(fs::read_to_string(f.file()).unwrap(), before);
    }
    #[test]
    fn custom_relative_and_tilde_paths_are_created_and_canonicalized() {
        let f = Fixture::new();
        f.choose(None, false, false, "./Work Space\n").unwrap().apply(false).unwrap();
        assert_eq!(OptionsFile::parse(&fs::read_to_string(f.file()).unwrap()).unwrap().projects_root.unwrap(), f.0.join("Work Space").canonicalize().unwrap().to_str().unwrap());
        f.choose(Some(Path::new("~/Other")), true, false, "").unwrap().apply(false).unwrap();
        assert!(f.0.join("Other").is_dir());
    }
    #[test]
    fn malformed_options_and_files_are_not_overwritten_or_created() {
        let f = Fixture::new(); f.write("this is broken");
        assert!(f.choose(Some(Path::new("new")), true, false, "").is_err());
        assert_eq!(fs::read_to_string(f.file()).unwrap(), "this is broken");
        assert!(!f.0.join("new").exists());
        f.write(""); fs::write(f.0.join("file"), "keep").unwrap();
        assert!(f.choose(Some(Path::new("file")), true, false, "").is_err());
        assert_eq!(fs::read_to_string(f.0.join("file")).unwrap(), "keep");
    }
    #[test]
    fn invalid_config_requires_explicit_repair_and_eof_cancels_without_mutation() {
        let f = Fixture::new(); f.write("projects_root = '/missing-alondra-root'\n");
        assert!(f.choose(None, true, false, "").is_err());
        assert!(f.choose(None, false, false, "").is_err());
        assert!(!f.0.join("Projects").exists());
        f.choose(None, false, false, "fixed\n").unwrap().apply(false).unwrap();
        assert!(f.0.join("fixed").is_dir());
    }
    #[test]
    fn dry_run_never_reads_stdin_creates_directory_or_writes_options() {
        let f = Fixture::new();
        f.choose(None, false, true, "").unwrap().apply(true).unwrap();
        assert!(!f.file().exists()); assert!(!f.0.join("Projects").exists());
        f.choose(Some(Path::new("custom")), false, true, "").unwrap().apply(true).unwrap();
        assert!(!f.0.join("custom").exists());
    }
    #[test]
    fn refuses_concurrent_options_edits() {
        let f = Fixture::new(); let selection = f.choose(None, true, false, "").unwrap();
        f.write("# changed\n"); assert!(selection.apply(false).is_err());
        assert_eq!(fs::read_to_string(f.file()).unwrap(), "# changed\n");
        assert!(!f.0.join("Projects").exists());
    }
}
