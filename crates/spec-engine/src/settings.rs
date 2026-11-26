use serde::Deserialize;

use crate::pattern::folder;
use crate::problem::{TOML_MAX_BYTES, finish, key_line, problem, table_key_line, toml_line, too_large};
use crate::{Code, FolderSettings, Problem, Settings, SpecsSettings};

pub(crate) const PATH: &str = "alondra.toml";
const FORMAT: i64 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    format: i64,
    #[serde(default)]
    specs: RawSpecs,
    #[serde(default)]
    decisions: RawFolder,
    #[serde(default)]
    reference: RawFolder,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawSpecs {
    enabled: Option<bool>,
    committed: Option<bool>,
    dir: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawFolder {
    enabled: Option<bool>,
    dir: Option<String>,
}

fn defaults() -> Settings {
    Settings {
        specs: SpecsSettings { enabled: false, committed: true, dir: "specs".to_string() },
        decisions: FolderSettings { enabled: false, dir: "docs/decisions".to_string() },
        reference: FolderSettings { enabled: false, dir: "docs/reference".to_string() },
    }
}

pub(crate) fn read(bytes: Option<&[u8]>) -> Result<Settings, Vec<Problem>> {
    let Some(bytes) = bytes else {
        return Ok(defaults());
    };
    if let Some(found) = too_large(PATH, bytes, TOML_MAX_BYTES) {
        return Err(vec![found]);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Err(vec![problem(Code::InvalidUtf8, PATH, 0, "the settings are not valid UTF-8")]);
    };
    let raw: RawSettings = match toml::from_str(source) {
        Ok(raw) => raw,
        Err(failure) => {
            return Err(vec![problem(Code::InvalidSettings, PATH, toml_line(source, &failure), failure.message().to_string())]);
        }
    };
    if raw.format != FORMAT {
        return Err(vec![problem(
            Code::UnsupportedFormat,
            PATH,
            key_line(source, "format"),
            format!("format {} is not supported; this engine reads format {FORMAT}", raw.format),
        )]);
    }
    let base = defaults();
    let mut problems = Vec::new();
    let specs_dir = checked_folder(raw.specs.dir, base.specs.dir, table_key_line(source, "specs", "dir"), &mut problems);
    let decisions_dir =
        checked_folder(raw.decisions.dir, base.decisions.dir, table_key_line(source, "decisions", "dir"), &mut problems);
    let reference_dir =
        checked_folder(raw.reference.dir, base.reference.dir, table_key_line(source, "reference", "dir"), &mut problems);
    let settings = Settings {
        specs: SpecsSettings {
            enabled: raw.specs.enabled.unwrap_or(false),
            committed: raw.specs.committed.unwrap_or(true),
            dir: specs_dir,
        },
        decisions: FolderSettings { enabled: raw.decisions.enabled.unwrap_or(false), dir: decisions_dir },
        reference: FolderSettings { enabled: raw.reference.enabled.unwrap_or(false), dir: reference_dir },
    };
    finish(settings, problems)
}

fn checked_folder(written: Option<String>, fallback: String, line: usize, problems: &mut Vec<Problem>) -> String {
    let Some(written) = written else {
        return fallback;
    };
    match folder(&written) {
        Some(dir) => dir,
        None => {
            problems.push(problem(
                Code::PathOutsideRepository,
                PATH,
                line,
                format!("the folder {written:?} is not a relative path inside the repository"),
            ));
            written
        }
    }
}
