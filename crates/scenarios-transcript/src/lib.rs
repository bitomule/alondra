//! The transcript of a provider's process: what the daemon is expected to send it and what it
//! answers, in order, one JSON object per line. One reader serves every user of a transcript: the
//! scenarios, and the sandbox's simulated provider.
//!
//! ```text
//! {"type":"transcript_start","provider":"claude", ...}
//! {"type":"expect_outbound","frame":{...}}
//! {"type":"emit_inbound","frame":{...}}
//! {"type":"runtime_exit","code":0}
//! ```

pub mod anonymize;
pub mod frames;
pub mod guard;
pub mod outbox;
pub mod scratch;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where a transcript comes from: written by hand, or a real session ported with its text replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Written,
    PortedIncident,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub provider: String,
    pub protocol_version: u16,
    pub sdk_version: String,
    pub model: String,
    pub captured_at: String,
    pub scenario: String,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Line {
    TranscriptStart(Header),
    /// A frame the daemon is expected to send the provider next.
    ExpectOutbound { frame: Value },
    /// A frame the provider sends the daemon.
    EmitInbound { frame: Value },
    RuntimeExit { code: Option<i32> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub header: Header,
    pub lines: Vec<Line>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Empty,
    NoHeader,
    MisplacedHeader { line: usize },
    Invalid { line: usize, reason: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Empty => f.write_str("the transcript is empty"),
            Error::NoHeader => f.write_str("the transcript does not start with its header"),
            Error::MisplacedHeader { line } => write!(f, "line {line}: the header is only the first line"),
            Error::Invalid { line, reason } => write!(f, "line {line}: {reason}"),
        }
    }
}
impl std::error::Error for Error {}

impl Transcript {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut header = None;
        let mut lines = Vec::new();
        for (index, raw) in text.lines().enumerate().filter(|(_, raw)| !raw.trim().is_empty()) {
            let number = index + 1;
            let line: Line = serde_json::from_str(raw).map_err(|error| Error::Invalid { line: number, reason: error.to_string() })?;
            match line {
                Line::TranscriptStart(start) if header.is_none() && lines.is_empty() => header = Some(start),
                Line::TranscriptStart(_) => return Err(Error::MisplacedHeader { line: number }),
                other if header.is_none() => {
                    let _ = other;
                    return Err(Error::NoHeader);
                }
                other => lines.push(other),
            }
        }
        match header {
            Some(header) => Ok(Self { header, lines }),
            None if lines.is_empty() => Err(Error::Empty),
            None => Err(Error::NoHeader),
        }
    }

    pub fn render(&self) -> String {
        let mut text = serde_json::to_string(&Line::TranscriptStart(self.header.clone())).expect("a header renders");
        text.push('\n');
        for line in &self.lines {
            text.push_str(&serde_json::to_string(line).expect("a line renders"));
            text.push('\n');
        }
        text
    }

    /// The frames the provider sends, in order.
    pub fn inbound(&self) -> impl Iterator<Item = &Value> {
        self.lines.iter().filter_map(|line| match line {
            Line::EmitInbound { frame } => Some(frame),
            _ => None,
        })
    }

    pub fn outbound(&self) -> impl Iterator<Item = &Value> {
        self.lines.iter().filter_map(|line| match line {
            Line::ExpectOutbound { frame } => Some(frame),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header() -> String {
        r#"{"type":"transcript_start","provider":"claude","protocol_version":1,"sdk_version":"0.3.300","model":"model-a","captured_at":"2026-11-15","scenario":"demo","origin":"written"}"#.into()
    }

    #[test]
    fn a_transcript_reads_back_as_it_was_written() {
        let text = format!(
            "{}\n{}\n{}\n{}\n",
            header(),
            r#"{"type":"expect_outbound","frame":{"a":1}}"#,
            r#"{"type":"emit_inbound","frame":{"b":2}}"#,
            r#"{"type":"runtime_exit","code":0}"#
        );
        let transcript = Transcript::parse(&text).unwrap();
        assert_eq!(transcript.header.origin, Origin::Written);
        assert_eq!(transcript.inbound().collect::<Vec<_>>(), vec![&json!({"b": 2})]);
        assert_eq!(transcript.outbound().collect::<Vec<_>>(), vec![&json!({"a": 1})]);
        assert_eq!(Transcript::parse(&transcript.render()).unwrap(), transcript);
    }

    #[test]
    fn a_transcript_without_its_header_or_with_two_is_refused() {
        assert_eq!(Transcript::parse("").unwrap_err(), Error::Empty);
        assert_eq!(Transcript::parse(r#"{"type":"runtime_exit","code":0}"#).unwrap_err(), Error::NoHeader);
        assert_eq!(Transcript::parse(&format!("{}\n{}", header(), header())).unwrap_err(), Error::MisplacedHeader { line: 2 });
    }

    #[test]
    fn an_unknown_field_is_refused_with_its_line() {
        let text = format!("{}\n{}", header(), r#"{"type":"emit_inbound","frame":{},"extra":1}"#);
        assert!(matches!(Transcript::parse(&text).unwrap_err(), Error::Invalid { line: 2, .. }));
    }
}
