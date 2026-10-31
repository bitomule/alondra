//! What a task is, where it works and what makes it risky: the orchestrator tags each task so the
//! model chosen for it, and how that model did, can be compared by kind of work.
use serde::{Deserialize, Serialize};

macro_rules! tag {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),* }
            }

            pub fn parse(text: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|tag| tag.as_str() == text)
            }

            pub fn names() -> Vec<&'static str> {
                Self::ALL.iter().map(|tag| tag.as_str()).collect()
            }
        }
    };
}

tag!(
    /// The kind of work.
    TaskKind {
        Feature => "feature", Fix => "fix", Refactor => "refactor", Tests => "tests", Docs => "docs",
        Migration => "migration", Design => "design", Review => "review", Release => "release", Research => "research",
    }
);

tag!(
    /// Where the work happens.
    TaskArea { Ui => "ui", Daemon => "daemon", Data => "data", Build => "build" }
);

tag!(
    /// What makes the work riskier than its kind says.
    TaskRisk {
        Concurrency => "concurrency", Security => "security", DataMigration => "data_migration",
        UnclearSpec => "unclear_spec", UserVisible => "user_visible",
    }
);

impl TaskRisk {
    /// The risks that call for the strongest model for the kind, and never for exploring one.
    pub fn is_severe(self) -> bool {
        matches!(self, Self::Concurrency | Self::Security | Self::DataMigration)
    }
}

tag!(
    /// Estimated size before the task starts.
    TaskSize { Small => "small", Medium => "medium", Large => "large" }
);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Impact { Normal, High }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskTags {
    pub kind: TaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TaskSize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<TaskArea>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<TaskRisk>,
}

impl TaskTags {
    pub fn impact(&self) -> Impact {
        self.impact_with(3, &[TaskArea::Data])
    }

    pub fn impact_with(&self, broad_count: usize, broad_areas: &[TaskArea]) -> Impact {
        let areas: std::collections::BTreeSet<_> = self.areas.iter().collect();
        if self.severe() || self.size == Some(TaskSize::Large)
            || areas.len() >= broad_count || self.areas.iter().any(|area| broad_areas.contains(area)) {
            Impact::High
        } else { Impact::Normal }
    }

    pub fn validate(&self) -> Result<(), String> {
        fn repeated<T: Ord>(items: &[T]) -> bool {
            let mut seen = std::collections::BTreeSet::new();
            !items.iter().all(|item| seen.insert(item))
        }
        if repeated(&self.areas) {
            return Err("tags.areas repeats an area".into());
        }
        if repeated(&self.risks) {
            return Err("tags.risks repeats a risk".into());
        }
        Ok(())
    }

    pub fn severe(&self) -> bool {
        self.risks.iter().any(|risk| risk.is_severe())
    }

    /// The areas as stored beside a decision: a JSON array, or nothing without any.
    pub fn areas_json(&self) -> Option<String> {
        (!self.areas.is_empty()).then(|| serde_json::to_string(&self.areas).expect("areas serialize"))
    }

    pub fn risks_json(&self) -> Option<String> {
        (!self.risks.is_empty()).then(|| serde_json::to_string(&self.risks).expect("risks serialize"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tags_read_the_words_the_tools_offer_and_refuse_others() {
        let tags: TaskTags = serde_json::from_value(json!({"kind": "docs", "areas": ["ui", "build"], "risks": ["data_migration"]})).unwrap();
        assert_eq!(tags.kind, TaskKind::Docs);
        assert_eq!(tags.areas_json().as_deref(), Some(r#"["ui","build"]"#));
        assert_eq!(tags.risks_json().as_deref(), Some(r#"["data_migration"]"#));
        assert!(tags.severe());
        assert!(tags.validate().is_ok());
        for bad in [json!({"kind": "chore"}), json!({"areas": ["ui"]}), json!({"kind": "fix", "areas": ["web"]}), json!({"kind": "fix", "risks": ["scary"]}), json!({"kind": "fix", "owner": "me"})] {
            assert!(serde_json::from_value::<TaskTags>(bad.clone()).is_err(), "{bad}");
        }
        let repeated: TaskTags = serde_json::from_value(json!({"kind": "fix", "areas": ["ui", "ui"]})).unwrap();
        assert!(repeated.validate().unwrap_err().contains("repeats"));
        let plain: TaskTags = serde_json::from_value(json!({"kind": "fix", "risks": ["user_visible"]})).unwrap();
        assert!(!plain.severe());
        assert_eq!(plain.areas_json(), None);
    }

    #[test]
    fn every_name_parses_back() {
        for name in TaskKind::names() { assert_eq!(TaskKind::parse(name).unwrap().as_str(), name); }
        for name in TaskArea::names() { assert_eq!(TaskArea::parse(name).unwrap().as_str(), name); }
        for name in TaskRisk::names() { assert_eq!(TaskRisk::parse(name).unwrap().as_str(), name); }
        assert_eq!(TaskKind::names().len(), 10);
    }
}
