//! What agents spend and how their tasks go, in terms no provider owns. Each provider's adapter
//! turns its own reports into these; the daemon's store keeps them and folds each ended task.
pub mod fold;
pub mod report;
pub mod steps;

use serde::{Deserialize, Serialize};

/// What one session reported it spent on one model so far: running totals, never a delta. The
/// four token counts do not overlap: `input` is what was neither read from nor written to the
/// prompt cache.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub model: String,
    /// In millionths of a dollar, as the provider priced it; none when it gave no price.
    pub cost_micros: Option<i64>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl Usage {
    pub fn tokens(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// A report with nothing in it, as a crashed run's last result may be.
    pub fn is_empty(&self) -> bool {
        self.tokens() == 0 && self.cost_micros.unwrap_or(0) == 0
    }
}

/// Dollars as the provider wrote them, in millionths.
pub fn micros(dollars: f64) -> Option<i64> {
    (dollars.is_finite() && dollars >= 0.0).then(|| (dollars * 1_000_000.0).round() as i64)
}

/// Who a session worked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Node,
    Orchestrator,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Orchestrator => "orchestrator",
        }
    }
}
