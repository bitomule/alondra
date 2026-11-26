//! The crash report's words that the screens show too, and what of `alondra doctor` it reads.
use std::sync::OnceLock;

pub mod bundle {
    /// The sections of a report, after its manifest and description.
    pub const SECTIONS: &[&str] = &[
        "versions.json", "machine.json", "machine-samples.csv", "doctor.json", "daemon.log", "lifecycle.jsonl", "tasks.jsonl", "nodes.jsonl", "sdk-host.jsonl", "apps.jsonl",
    ];

    /// A report's id: `r-`, its day and eight hex digits.
    pub fn is_id(id: &str) -> bool {
        let parts: Vec<&str> = id.split('-').collect();
        parts.len() == 3 && parts[0] == "r" && parts[1].len() == 8 && parts[1].chars().all(|c| c.is_ascii_digit()) && parts[2].len() == 8 && parts[2].chars().all(|c| c.is_ascii_hexdigit())
    }
}

pub mod send {
    use serde::{Deserialize, Serialize};

    /// What failed, for the issue a report is sent as.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
    #[serde(rename_all = "kebab-case")]
    pub enum Kind {
        SessionEnded,
        MessageNotDelivered,
        NotResumed,
        Other,
    }

    impl Kind {
        pub const ALL: [Kind; 4] = [Kind::SessionEnded, Kind::MessageNotDelivered, Kind::NotResumed, Kind::Other];

        pub fn word(self) -> &'static str {
            match self {
                Kind::SessionEnded => "session-ended",
                Kind::MessageNotDelivered => "message-not-delivered",
                Kind::NotResumed => "not-resumed",
                Kind::Other => "other",
            }
        }

        pub fn from_word(word: &str) -> Option<Kind> {
            Kind::ALL.into_iter().find(|k| k.word() == word)
        }

        pub fn label(self) -> &'static str {
            match self {
                Kind::SessionEnded => tr!("A node's session ended on its own", "La sesión de un nodo terminó sola"),
                Kind::MessageNotDelivered => tr!("A message to a node did nothing", "Un mensaje a un nodo no hizo nada"),
                Kind::NotResumed => tr!("A node did not come back after a restart", "Un nodo no volvió tras reiniciar"),
                Kind::Other => tr!("Something else", "Otra cosa"),
            }
        }
    }
}

/// A size as a person reads it: bytes, KB or MB.
pub fn size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes} B") } else if bytes < 1024 * 1024 { format!("{} KB", bytes.div_ceil(1024)) } else { format!("{:.1} MB", bytes as f64 / 1_048_576.0) }
}

/// What a counter of the manifest's `omitted` counts, for the user.
pub fn omitted_label(key: &str) -> String {
    match key {
        "daemon_log_lines_without_template" => tr!("log lines no template of the code has", "líneas del log que no son de ninguna plantilla del código").into(),
        "lifecycle_events_not_decoded" => tr!("lifecycle events it could not read", "eventos del ciclo de vida que no pudo leer").into(),
        "sdk_host_instances_with_unknown_names" => tr!("SDK hosts with a name it does not know", "hosts del SDK con un nombre que no conoce").into(),
        "sdk_host_frames_not_decoded" => tr!("SDK host frames it could not read", "mensajes del host del SDK que no pudo leer").into(),
        "sdk_host_frames_of_unknown_type" => tr!("SDK host frames of a type it does not know", "mensajes del host del SDK de un tipo que no conoce").into(),
        other => other.to_owned(),
    }
}

/// The checks of `alondra doctor`, by id: the only ids `doctor.json` may carry.
pub const CHECK_IDS: &[&str] = &[
    "mac", "providers", "codex", "antigravity", "claude", "claude.profile", "claude.signin", "projects", "identity", "config", "service",
    "alondrad", "login-item", "state", "startup", "upgrade", "update", "hooks", "codex.signin", "plugin", "validate",
    "relay", "direct", "devices", "notifications",
];

static DIAGNOSIS: OnceLock<fn() -> Vec<(String, String)>> = OnceLock::new();

/// Installs what runs the doctor's checks for the report; the first one stays.
pub fn install_diagnosis(diagnose: fn() -> Vec<(String, String)>) {
    let _ = DIAGNOSIS.set(diagnose);
}

/// Each doctor check's id and status (`pass`, `warn`, `fail`), when this build can run them.
pub fn diagnosis() -> Option<Vec<(String, String)>> {
    DIAGNOSIS.get().map(|diagnose| diagnose())
}
