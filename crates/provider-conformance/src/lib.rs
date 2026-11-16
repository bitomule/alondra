#![forbid(unsafe_code)]
//! One list of scenarios every provider adapter must pass. Each scenario is a native transcript per
//! provider and one expected sequence of neutral facts, the same for all of them. A transcript runs
//! through the provider's real normalizer, staged in a real store as the daemon stages it; what the
//! domain would be told is compared in its neutral form.
//!
//! A provider whose binding lacks a capability a scenario needs skips it, with that reason. A
//! difference between adapters known today is written down with its cause in
//! [`catalogue::DIVERGENCES`], pinned to the sequence the provider produces: the suite fails when
//! that sequence changes, also when it becomes the expected one, so whoever fixes it removes the
//! entry.
mod canonical;
pub mod catalogue;
mod store;

use alondra_agent_application::ObservationNormalizer;
use alondra_foundation::InputId;
use alondra_provider_contract::{Capability, OrderedObservation};
use alondra_scenarios_transcript::{Line, Transcript};
use alondra_store::OwnerGrant;
use catalogue::{DIVERGENCES, KnownDivergence, SCENARIOS, Scenario};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use store::{Observed, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}
impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }
}

/// The host an observation comes from: the first of the execution, or one launched after the
/// previous one ended, whose first observation has `first_cursor`. `ended` once the adapter said
/// its native process is gone: what that host still sends is the last of a host that ended.
#[derive(Clone, Copy, Debug, Default)]
pub struct Host {
    pub first_cursor: Option<u64>,
    pub ended: bool,
}

/// The few things only the provider's own crate knows: its normalizer, how its host frames an
/// observation, how it records an input's native identity, and when its native process is gone.
pub trait Native {
    type Normalizer: ObservationNormalizer;
    fn provider(&self) -> Provider;
    /// What the provider's production binding declares.
    fn capabilities(&self) -> BTreeSet<Capability>;
    fn normalizer(&self, owner: &OwnerGrant, host: Host) -> Self::Normalizer;
    /// The evidence the host journals for `observation`, a wire observation as JSON, at `cursor`.
    fn evidence(&self, owner: &OwnerGrant, cursor: u64, observation: &Value) -> Result<Vec<u8>, String>;
    /// What the adapter stores for an input of Alondra's whose native identity is `native`.
    fn input_record(&self, native: &str) -> Result<Vec<u8>, String>;
    fn ends_host(&self, observation: &OrderedObservation) -> bool;
}

/// Alondra handing the provider an input: in a transcript, an `expect_outbound` frame
/// `{"input": <Alondra's id>, "native": <the provider's id for it>}`.
fn handoff(frame: &Value) -> Result<(InputId, String), String> {
    let field = |key: &str| frame.get(key).and_then(Value::as_str).ok_or(format!("an outbound frame names its {key}: {frame}"));
    Ok((InputId::new(field("input")?).map_err(|error| error.to_string())?, field("native")?.to_owned()))
}

/// Runs `transcript` through the adapter and returns the neutral facts, in order.
pub fn replay<A: Native>(adapter: &A, transcript: &Transcript) -> Result<Vec<String>, String> {
    let mut lines = transcript.lines.iter();
    let Some(Line::ExpectOutbound { frame }) = lines.next() else {
        return Err("a transcript starts with the input Alondra hands over".into());
    };
    let (initial, native) = handoff(frame)?;
    let mut store = Store::open(adapter.capabilities(), &initial, adapter.input_record(&native)?)?;
    let mut canon = canonical::Canon::default();
    let mut host = Host::default();
    let mut cursor = 0;
    let mut facts = Vec::new();
    for line in lines {
        match line {
            Line::ExpectOutbound { frame } => {
                let (input, native) = handoff(frame)?;
                store.hand(&input, adapter.input_record(&native)?)?;
            }
            Line::EmitInbound { frame } => {
                cursor += 1;
                let batch = store.batch(cursor, adapter.evidence(store.owner(), cursor, frame)?);
                if !host.ended && adapter.ends_host(&batch) {
                    host.ended = true;
                    facts.push(canonical::HOST_ENDED.to_owned());
                }
                let normalizer = adapter.normalizer(store.owner(), host);
                match store.observe(&normalizer, &batch).map_err(|error| format!("observation {cursor}: {error}"))? {
                    Observed::Facts(observed) => facts.extend(observed.iter().map(|fact| canon.fact(fact))),
                    Observed::Refused(reason) => facts.push(format!("Refused({reason})")),
                }
            }
            Line::RuntimeExit { .. } => host = Host { first_cursor: Some(cursor + 1), ended: false },
            Line::TranscriptStart(_) => return Err("a second header".into()),
        }
    }
    canonical::settle_availability(&mut facts);
    Ok(facts)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Skipped(String),
    KnownDivergence(&'static str),
    Failed(String),
}
impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass => f.write_str("pass"),
            Verdict::Skipped(reason) => write!(f, "skipped ({reason})"),
            Verdict::KnownDivergence(cause) => write!(f, "known_divergence ({cause})"),
            Verdict::Failed(reason) => write!(f, "FAILED: {reason}"),
        }
    }
}

fn listed(facts: &[String]) -> String {
    facts.iter().map(|fact| format!("\n    {fact}")).collect()
}

/// What `facts`, the neutral sequence a provider produced, says of `scenario`, given the
/// divergence known for that provider, if any.
pub fn judge(scenario: &Scenario, facts: Result<Vec<String>, String>, known: Option<&KnownDivergence>) -> Verdict {
    let facts = match facts {
        Ok(facts) => facts,
        Err(error) => return Verdict::Failed(format!("the transcript did not run: {error}")),
    };
    let same = |expected: &[&str]| facts.len() == expected.len() && facts.iter().zip(expected).all(|(fact, expected)| fact == expected);
    match known {
        Some(known) if same(scenario.expected) => Verdict::Failed(format!(
            "the known divergence «{}» no longer happens: the adapter now produces the expected facts, remove its entry",
            known.cause
        )),
        Some(known) if same(known.actual) => Verdict::KnownDivergence(known.cause),
        Some(known) => Verdict::Failed(format!(
            "neither the expected facts nor those of its known divergence\n  expected:{}\n  known divergence:{}\n  produced:{}",
            listed(&scenario.expected.iter().map(|fact| fact.to_string()).collect::<Vec<_>>()),
            listed(&known.actual.iter().map(|fact| fact.to_string()).collect::<Vec<_>>()),
            listed(&facts)
        )),
        None if same(scenario.expected) => Verdict::Pass,
        None => Verdict::Failed(format!(
            "expected:{}\n  produced:{}",
            listed(&scenario.expected.iter().map(|fact| fact.to_string()).collect::<Vec<_>>()),
            listed(&facts)
        )),
    }
}

pub struct Report {
    provider: Provider,
    rows: Vec<(&'static str, Verdict)>,
    stale: Vec<String>,
}

impl Report {
    pub fn count(&self, kind: fn(&Verdict) -> bool) -> usize {
        self.rows.iter().filter(|(_, verdict)| kind(verdict)).count()
    }
    pub fn passed(&self) -> bool {
        self.stale.is_empty() && !self.rows.iter().any(|(_, verdict)| matches!(verdict, Verdict::Failed(_)))
    }
    /// Prints the table and fails the test unless every scenario passes, is skipped for a reason,
    /// or diverges exactly as written down.
    pub fn assert_conforms(&self) {
        println!("{self}");
        assert!(self.passed(), "{self}");
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "provider conformance: {}", self.provider.name())?;
        for (scenario, verdict) in &self.rows {
            writeln!(f, "  {scenario:<34} {verdict}")?;
        }
        for stale in &self.stale {
            writeln!(f, "  FAILED: {stale}")?;
        }
        write!(
            f,
            "{}: {} scenarios, {} pass, {} known divergences, {} skipped, {} failed",
            self.provider.name(),
            self.rows.len(),
            self.count(|verdict| *verdict == Verdict::Pass),
            self.count(|verdict| matches!(verdict, Verdict::KnownDivergence(_))),
            self.count(|verdict| matches!(verdict, Verdict::Skipped(_))),
            self.count(|verdict| matches!(verdict, Verdict::Failed(_))) + self.stale.len()
        )
    }
}

/// Runs every scenario of the catalogue through `adapter`.
pub fn run<A: Native>(adapter: &A) -> Report {
    let provider = adapter.provider();
    let capabilities = adapter.capabilities();
    let mut rows = Vec::new();
    let mut stale = Vec::new();
    for scenario in SCENARIOS {
        let known = DIVERGENCES.iter().find(|known| known.scenario == scenario.name && known.provider == provider);
        let lacking: Vec<String> = scenario.needs.iter().filter(|need| !capabilities.contains(need)).map(|need| format!("{need:?}")).collect();
        if !lacking.is_empty() {
            if known.is_some() {
                stale.push(format!("{}: a known divergence of a scenario it skips", scenario.name));
            }
            rows.push((scenario.name, Verdict::Skipped(format!("its binding declares no {}", lacking.join(", ")))));
            continue;
        }
        let facts = scenario
            .transcript(provider)
            .ok_or_else(|| format!("no {} transcript", provider.name()))
            .and_then(|text| Transcript::parse(text).map_err(|error| error.to_string()))
            .and_then(|transcript| {
                if transcript.header.provider != provider.name() || transcript.header.scenario != scenario.name {
                    return Err(format!("the transcript's header names {} {}", transcript.header.provider, transcript.header.scenario));
                }
                replay(adapter, &transcript)
            });
        rows.push((scenario.name, judge(scenario, facts, known)));
    }
    for known in DIVERGENCES.iter().filter(|known| known.provider == provider) {
        if !SCENARIOS.iter().any(|scenario| scenario.name == known.scenario) {
            stale.push(format!("a known divergence of {}, which is no scenario", known.scenario));
        }
    }
    Report { provider, rows, stale }
}

#[cfg(test)]
mod verdict_tests;
