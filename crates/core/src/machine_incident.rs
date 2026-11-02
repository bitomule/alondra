//! Alondra's card for a machine that stops nodes: the disk nearly full, or a node that declared it
//! waits for the machine. One card per incident; its body holds the numbers so every client words
//! it in its own language. Memory never raises one: Alondra paces the nodes by itself.

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
    /// Only in records from before memory stopped raising cards; read to retire them.
    Memory,
    Load,
    /// A node said it waits for the machine and no gauge shows why.
    Declared,
}

/// What a node says it waits for. Nothing names the subscription's usage: a wait for it cannot be
/// declared.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineResource {
    Memory,
    Disk,
    Cpu,
    BuildQueue,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MachineCard {
    pub cause: MachineCause,
    pub disk_free: u64,
    /// The stopped nodes, by the title of their task.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// What a node declared, when it is the only explanation.
    #[serde(default)]
    pub reason: Option<String>,
    /// The resource that node declared; none from a CLI older than the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<MachineResource>,
    /// Minutes the nodes have waited, on the reminder.
    #[serde(default)]
    pub minutes: Option<u64>,
    /// The volume that is nearly full, when it is not the Mac's own disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<String>,
    /// The user said they fixed it and the gauge still measures the problem.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still: bool,
    /// Space macOS could purge by itself but has not, when that is why the disk is low: then
    /// `disk_free` is the space free right now.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub purgeable: u64,
}

fn is_zero(bytes: &u64) -> bool {
    *bytes == 0
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
        let purgeable = bytes_text(self.purgeable, lang);
        let unpurged = self.purgeable > 0;
        let need = match self.cause {
            MachineCause::Disk if unpurged && self.still => tf!(lang, "Still: {free} free right now{on}", "Sigue: {free} libres ahora mismo{on}"),
            MachineCause::Disk if unpurged && self.nodes.is_empty() => tf!(lang, "Free up disk space: only {free} free right now{on}", "Libera espacio en disco: solo {free} libres ahora mismo{on}"),
            MachineCause::Disk if unpurged => tf!(lang, "Free up disk space: {free} free right now{on}", "Libera espacio en disco: {free} libres ahora mismo{on}"),
            MachineCause::Disk if self.still => tf!(lang, "Still: {free} free left{on}", "Sigue: quedan {free} libres{on}"),
            MachineCause::Disk if self.nodes.is_empty() => tf!(lang, "Free up disk space: only {free} left{on}", "Libera espacio en disco: solo quedan {free}{on}"),
            MachineCause::Disk => tf!(lang, "Free up disk space: {free} left{on}", "Libera espacio en disco: quedan {free}{on}"),
            MachineCause::Memory => t!(lang, "Memory was at its limit", "La memoria estuvo al límite").into(),
            MachineCause::Load => t!(lang, "Lighten the Mac's load: the CPU is at its limit", "Alivia la carga del Mac: la CPU está al límite").into(),
            MachineCause::Declared => t!(lang, "Check the Mac: nodes are waiting for the machine", "Revisa el Mac: hay nodos esperando a la máquina").into(),
        };
        let stopped = match (self.nodes.len(), self.minutes) {
            (0, _) => t!(lang, "No node has stopped yet; past this point they fail.", "Aún no se ha parado ningún nodo; a partir de aquí empiezan a fallar.").into(),
            (_, Some(minutes)) => tf!(lang, "Still stopped after {minutes} min: {names}.", "Siguen parados tras {minutes} min: {names}.", names = self.names(lang)),
            _ => tf!(lang, "Stopped: {names}.", "Parados: {names}.", names = self.names(lang)),
        };
        let cause = match self.cause {
            MachineCause::Disk if unpurged => tf!(lang, "Only {free} free: macOS has not yet freed {purgeable} of snapshots and caches. Alondra starts no new node until there is space.",
                "Solo {free} libres: macOS aún no ha liberado {purgeable} de instantáneas y cachés. Alondra no lanzará nodos nuevos hasta que haya espacio."),
            MachineCause::Disk => match &self.disk {
                Some(disk) => tf!(lang, "{disk} nearly full ({free} free). Alondra starts no new node until there is space.",
                    "{disk} casi lleno ({free} libres). Alondra no lanzará nodos nuevos hasta que haya espacio."),
                None => tf!(lang, "Disk nearly full ({free} free). Alondra starts no new node until there is space.",
                    "Disco casi lleno ({free} libres). Alondra no lanzará nodos nuevos hasta que haya espacio."),
            },
            MachineCause::Memory => String::new(),
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
        MachineCard { cause, disk_free: 0, nodes: nodes.iter().map(|n| n.to_string()).collect(), reason: None, resource: None, minutes: None, disk: None, still: false, purgeable: 0 }
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
        let reminder = MachineCard { minutes: Some(30), ..card(MachineCause::Disk, &["a", "b", "c", "d", "e"]) };
        assert_eq!(reminder.text(Lang::En), ("Free up disk space: 0 B left".into(),
            "Disk nearly full (0 B free). Alondra starts no new node until there is space. Still stopped after 30 min: a, b, c and 2 more. They carry on by themselves once it recovers.".into()));
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
        let old = MachineCard::from_body(r#"{"cause":"disk","disk_free":0}"#).unwrap();
        assert_eq!((old.disk, old.purgeable), (None, 0));
    }

    #[test]
    fn a_disk_low_because_macos_has_not_purged_says_what_it_could_free() {
        let unpurged = MachineCard { disk_free: 1_200_000_000, purgeable: 14_300_000_000, ..card(MachineCause::Disk, &[]) };
        assert_eq!(MachineCard::from_body(&unpurged.to_body()), Some(unpurged.clone()));
        assert_eq!(unpurged.text(Lang::En), ("Free up disk space: only 1.2 GB free right now".into(),
            "Only 1.2 GB free: macOS has not yet freed 14.3 GB of snapshots and caches. Alondra starts no new node until there is space. No node has stopped yet; past this point they fail. They carry on by themselves once it recovers.".into()));
        assert_eq!(unpurged.text(Lang::Es), ("Libera espacio en disco: solo 1,2 GB libres ahora mismo".into(),
            "Solo 1,2 GB libres: macOS aún no ha liberado 14,3 GB de instantáneas y cachés. Alondra no lanzará nodos nuevos hasta que haya espacio. Aún no se ha parado ningún nodo; a partir de aquí empiezan a fallar. Continúan solos cuando se recupere.".into()));
        let still = MachineCard { still: true, disk: Some("Externo".into()), ..unpurged };
        assert_eq!(still.text(Lang::Es).0, "Sigue: 1,2 GB libres ahora mismo en Externo");
    }

    #[test]
    fn a_declared_wait_quotes_the_node() {
        let declared = MachineCard { reason: Some("build queue full.".into()), ..card(MachineCause::Declared, &["Quill"]) };
        assert!(declared.text(Lang::En).1.starts_with("A node says: build queue full. Stopped: Quill."));
    }

    #[test]
    fn a_memory_card_left_by_an_earlier_daemon_still_reads_without_asking_to_free_memory() {
        let left = r#"{"cause":"memory","disk_free":0,"memory_free_percent":38,"near_pressure":true,"single_node":true,"top":[["Xcode",3100000000]],"nodes":["Lumo"]}"#;
        let card = MachineCard::from_body(left).expect("an old memory card still reads");
        assert_eq!(card.text(Lang::Es), ("La memoria estuvo al límite".into(), "Parados: Lumo. Continúan solos cuando se recupere.".into()));
        assert!(!card.text(Lang::En).0.contains("Free up"));
    }
}
