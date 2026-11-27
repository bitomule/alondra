//! Alondra's reasoning effort and Antigravity's thinking levels. A model offers its own levels in
//! the `thought_level` option of its session; Alondra ships no list of them.

/// The level id that carries `effort` among those the model offers. `xhigh` is called Extra in
/// Antigravity, and an older id for it is `extra`. An effort outside the table names a level id as
/// it is.
pub fn level_for(effort: &str, offered: &[&str]) -> Result<String, LevelRefused> {
    let candidates = if effort == "xhigh" { vec!["xhigh", "extra"] } else { vec![effort] };
    candidates.iter().find(|candidate| offered.contains(candidate)).map(|level| (*level).to_owned()).ok_or_else(|| LevelRefused { effort: effort.to_owned() })
}

/// The effort a level id the catalog read stands for: its own id when it is not in the table.
pub fn effort_of(level: &str) -> &str {
    match level {
        "extra" => "xhigh",
        other => other,
    }
}

/// The model does not offer the level an effort asks for: the launch is refused before any host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelRefused {
    pub effort: String,
}

impl std::fmt::Display for LevelRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the model does not offer the {} thinking level", self.effort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: antigravity.models/levels-read-as-efforts
    #[test]
    fn each_effort_is_the_level_of_the_same_name_and_xhigh_is_extra() {
        let neon = ["low", "medium", "high", "xhigh"];
        for effort in ["low", "medium", "high", "xhigh"] {
            assert_eq!(level_for(effort, &neon).unwrap(), effort);
        }
        assert_eq!(level_for("xhigh", &["low", "extra"]).unwrap(), "extra");
        assert_eq!(level_for("max", &["low", "max"]).unwrap(), "max");
        assert_eq!(effort_of("extra"), "xhigh");
        assert_eq!(effort_of("xhigh"), "xhigh");
    }

    // spec: antigravity.models/levels-read-as-efforts
    #[test]
    fn a_level_id_outside_the_table_is_kept_as_it_is() {
        assert_eq!(effort_of("minimal"), "minimal");
        assert_eq!(level_for("minimal", &["minimal", "low"]).unwrap(), "minimal");
    }

    #[test]
    fn a_level_the_model_does_not_offer_is_refused() {
        let carbon = ["low", "medium", "high"];
        assert_eq!(level_for("xhigh", &carbon), Err(LevelRefused { effort: "xhigh".into() }));
        assert_eq!(level_for("max", &carbon).unwrap_err().to_string(), "the model does not offer the max thinking level");
    }
}
