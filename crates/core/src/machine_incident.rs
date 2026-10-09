//! Alondra's card for a machine that stops nodes: the disk nearly full, memory or CPU at the
//! limit, or a node that declared it waits for the machine. One card per incident; its body holds
//! the numbers so every client words it in its own language.

use serde::{Deserialize, Serialize};

use crate::views::Lang;
use crate::views::text::{t, tf};

pub const MACHINE_PREFIX: &str = "machine-";
pub const MACHINE_FIXED_OPTION: &str = "fixed";
pub const MACHINE_FIXED_LABEL: &str = "I've fixed it";
const NAMES_SHOWN: usize = 3;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineCause {
    Disk,
    Memory,
    Load,
    /// A node said it waits for the machine and no gauge shows why.
    Declared,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MachineCard {
    pub cause: MachineCause,
    pub disk_free: u64,
    #[serde(default)]
    pub memory_free_percent: Option<u32>,
    /// macOS reports memory pressure, which can happen with plenty of memory free.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub memory_pressure: bool,
    /// The stopped nodes, by the title of their task.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// What a node declared, when it is the only explanation.
    #[serde(default)]
    pub reason: Option<String>,
    /// Minutes the nodes have waited, on the reminder.
    #[serde(default)]
    pub minutes: Option<u64>,
    /// The volume that is nearly full, when it is not the Mac's own disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<String>,
    /// Memory stays at its limit with one node working, the rest paused: something else eats it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub single_node: bool,
    /// The processes using the most memory, by name and resident bytes, on that card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top: Vec<(String, u64)>,
    /// The user said they fixed it and the gauge still measures the problem.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still: bool,
}

/// "0 B", "450 MB", "4.2 GB".
pub fn bytes_text(bytes: u64, lang: Lang) -> String {
    let text = if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else {
        format!("{bytes} B")
    };
    match lang {
        Lang::En => text,
        Lang::Es => text.replace('.', ","),
    }
}

impl MachineCard {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    fn names(&self, lang: Lang) -> String {
        let mut names = self.nodes.iter().take(NAMES_SHOWN).cloned().collect::<Vec<_>>().join(", ");
        let more = self.nodes.len().saturating_sub(NAMES_SHOWN);
        if more > 0 {
            names.push_str(&tf!(lang, " and {more} more", " y {more} más"));
        }
        names
    }

    /// The card's `need` and `why`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let free = bytes_text(self.disk_free, lang);
        let on = self.disk.as_ref().map(|disk| tf!(lang, " on {disk}", " en {disk}")).unwrap_or_default();
        let memory = self.memory_free_percent.unwrap_or(0);
        let need = match self.cause {
            MachineCause::Disk if self.still => tf!(lang, "Still: {free} free left{on}", "Sigue: quedan {free} libres{on}"),
            MachineCause::Memory if self.still => tf!(lang, "Still: memory {memory}% free", "Sigue: memoria al {memory}% libre"),
            MachineCause::Disk if self.nodes.is_empty() => tf!(lang, "Free up disk space: only {free} left{on}", "Libera espacio en disco: solo quedan {free}{on}"),
            MachineCause::Disk => tf!(lang, "Free up disk space: {free} left{on}", "Libera espacio en disco: quedan {free}{on}"),
            MachineCause::Memory if self.memory_pressure && self.single_node => tf!(lang, "Free up memory: macOS reports memory pressure ({memory}% free) with a single node working",
                "Libera memoria: macOS avisa de presión de memoria ({memory}% libre) con un solo nodo trabajando"),
            MachineCause::Memory if self.memory_pressure => tf!(lang, "Free up memory: macOS reports memory pressure ({memory}% free)",
                "Libera memoria: macOS avisa de presión de memoria ({memory}% libre)"),
            MachineCause::Memory if self.single_node => tf!(lang, "Free up memory: {memory}% free with a single node working",
                "Libera memoria: {memory}% libre con un solo nodo trabajando"),
            MachineCause::Memory => tf!(lang, "Free up memory: {memory}% free", "Libera memoria: {memory}% libre"),
            MachineCause::Load => t!(lang, "Lighten the Mac's load: the CPU is at its limit", "Alivia la carga del Mac: la CPU está al límite").into(),
            MachineCause::Declared => t!(lang, "Check the Mac: nodes are waiting for the machine", "Revisa el Mac: hay nodos esperando a la máquina").into(),
        };
        let stopped = match (self.nodes.len(), self.minutes) {
            (0, _) => t!(lang, "No node has stopped yet; past this point they fail.", "Aún no se ha parado ningún nodo; a partir de aquí empiezan a fallar.").into(),
            (_, Some(minutes)) => tf!(lang, "Still stopped after {minutes} min: {names}.", "Siguen parados tras {minutes} min: {names}.", names = self.names(lang)),
            _ => tf!(lang, "Stopped: {names}.", "Parados: {names}.", names = self.names(lang)),
        };
        let cause = match self.cause {
            MachineCause::Disk => match &self.disk {
                Some(disk) => tf!(lang, "{disk} nearly full ({free} free). Alondra starts no new node until there is space.",
                    "{disk} casi lleno ({free} libres). Alondra no lanzará nodos nuevos hasta que haya espacio."),
                None => tf!(lang, "Disk nearly full ({free} free). Alondra starts no new node until there is space.",
                    "Disco casi lleno ({free} libres). Alondra no lanzará nodos nuevos hasta que haya espacio."),
            },
            MachineCause::Memory => {
                let limit = if self.memory_pressure {
                    tf!(lang, "macOS reports memory pressure ({memory}% free).", "macOS avisa de presión de memoria ({memory}% libre).")
                } else {
                    tf!(lang, "Memory at its limit ({memory}% free).", "Memoria al límite ({memory}% libre).")
                };
                if self.top.is_empty() {
                    limit
                } else {
                    let top = self.top.iter().map(|(name, bytes)| format!("{name} {}", bytes_text(*bytes, lang))).collect::<Vec<_>>().join(", ");
                    format!("{limit} {}", tf!(lang, "Using the most: {top}.", "Lo que más ocupa: {top}."))
                }
            }
            MachineCause::Load => t!(lang, "CPU at its limit.", "CPU al límite.").into(),
            MachineCause::Declared => match &self.reason {
                Some(reason) => tf!(lang, "A node says: {reason}.", "Un nodo dice: {reason}.", reason = reason.trim_end_matches('.')),
                None => String::new(),
            },
        };
        let action = t!(lang, "They carry on by themselves once it recovers.", "Continúan solos cuando se recupere.");
        let why = [cause, stopped, action.into()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
        (need, why.chars().take(240).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(cause: MachineCause, nodes: &[&str]) -> MachineCard {
        MachineCard { cause, disk_free: 0, memory_free_percent: Some(9), memory_pressure: false, nodes: nodes.iter().map(|n| n.to_string()).collect(), reason: None, minutes: None, disk: None, single_node: false, top: vec![], still: false }
    }

    #[test]
    fn a_full_disk_names_the_free_space_and_the_stopped_nodes_in_both_languages() {
        let full = card(MachineCause::Disk, &["Fase 0: quitar versión de identificadores", "Lumo"]);
        assert_eq!(MachineCard::from_body(&full.to_body()), Some(full.clone()));
        let (need, why) = full.text(Lang::Es);
        assert_eq!(need, "Libera espacio en disco: quedan 0 B");
        assert_eq!(why, "Disco casi lleno (0 B libres). Alondra no lanzará nodos nuevos hasta que haya espacio. Parados: Fase 0: quitar versión de identificadores, Lumo. Continúan solos cuando se recupere.");
        assert_eq!(full.text(Lang::En).0, "Free up disk space: 0 B left");
    }

    #[test]
    fn a_warning_before_any_node_stops_and_a_reminder_say_so() {
        let warning = MachineCard { disk_free: 4_200_000_000, ..card(MachineCause::Disk, &[]) };
        assert_eq!(warning.text(Lang::Es).0, "Libera espacio en disco: solo quedan 4,2 GB");
        assert!(warning.text(Lang::En).1.contains("No node has stopped yet"));
        let reminder = MachineCard { minutes: Some(30), ..card(MachineCause::Memory, &["a", "b", "c", "d", "e"]) };
        assert_eq!(reminder.text(Lang::En), ("Free up memory: 9% free".into(),
            "Memory at its limit (9% free). Still stopped after 30 min: a, b, c and 2 more. They carry on by themselves once it recovers.".into()));
    }

    #[test]
    fn a_nearly_full_external_disk_is_named_and_the_card_says_no_node_starts() {
        let external = MachineCard { disk_free: 3_200_000_000, disk: Some("Externo".into()), ..card(MachineCause::Disk, &[]) };
        assert_eq!(MachineCard::from_body(&external.to_body()), Some(external.clone()));
        assert_eq!(external.text(Lang::Es), ("Libera espacio en disco: solo quedan 3,2 GB en Externo".into(),
            "Externo casi lleno (3,2 GB libres). Alondra no lanzará nodos nuevos hasta que haya espacio. Aún no se ha parado ningún nodo; a partir de aquí empiezan a fallar. Continúan solos cuando se recupere.".into()));
        assert_eq!(external.text(Lang::En).0, "Free up disk space: only 3.2 GB left on Externo");
        assert!(external.text(Lang::En).1.starts_with("Externo nearly full (3.2 GB free). Alondra starts no new node until there is space."));
    }

    #[test]
    fn a_card_written_before_the_disk_field_still_reads() {
        assert_eq!(MachineCard::from_body(r#"{"cause":"disk","disk_free":0}"#).unwrap().disk, None);
    }

    #[test]
    fn a_declared_wait_quotes_the_node() {
        let declared = MachineCard { reason: Some("build queue full.".into()), ..card(MachineCause::Declared, &["Quill"]) };
        assert!(declared.text(Lang::En).1.starts_with("A node says: build queue full. Stopped: Quill."));
    }

    #[test]
    fn memory_at_its_limit_with_a_single_node_working_says_so_and_names_what_uses_the_most() {
        let single = MachineCard { memory_free_percent: Some(4), single_node: true, top: vec![("Simulator".into(), 1_900_000_000), ("Xcode".into(), 3_100_000_000)], ..card(MachineCause::Memory, &[]) };
        assert_eq!(MachineCard::from_body(&single.to_body()), Some(single.clone()));
        let (need, why) = single.text(Lang::Es);
        assert_eq!(need, "Libera memoria: 4% libre con un solo nodo trabajando");
        assert!(why.starts_with("Memoria al límite (4% libre). Lo que más ocupa: Simulator 1,9 GB, Xcode 3,1 GB."), "{why}");
        assert_eq!(single.text(Lang::En).0, "Free up memory: 4% free with a single node working");
    }

    #[test]
    fn memory_pressure_with_memory_free_says_macos_reports_it_in_both_languages() {
        let pressure = MachineCard { memory_free_percent: Some(31), memory_pressure: true, ..card(MachineCause::Memory, &["Lumo"]) };
        assert_eq!(MachineCard::from_body(&pressure.to_body()), Some(pressure.clone()));
        assert_eq!(pressure.text(Lang::Es), ("Libera memoria: macOS avisa de presión de memoria (31% libre)".into(),
            "macOS avisa de presión de memoria (31% libre). Parados: Lumo. Continúan solos cuando se recupere.".into()));
        assert_eq!(pressure.text(Lang::En).0, "Free up memory: macOS reports memory pressure (31% free)");
        assert!(!MachineCard::from_body(r#"{"cause":"memory","disk_free":0}"#).unwrap().memory_pressure);
    }
}
