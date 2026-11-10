/// What one retention run released: the provider evidence, and the rows each policy changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Retained {
    pub evidence: alondra_store::EvidenceRetention,
    pub tables: Vec<(&'static str, usize)>,
    /// Ended tasks whose cards were compacted.
    pub compacted: usize,
    /// Ended tasks folded into their metrics.
    pub folded: usize,
}
