//! Turns the frames a real session's host wrote into a transcript that can be committed: free text
//! becomes stable markers, identity moves to the demo world, ids to ids of a visible pattern, and
//! the account's configuration is emptied. What stays is the structure: the order of the frames,
//! the names of kinds and tools, and which id is which.

use crate::guard::{self, DEMO_EMAIL, DEMO_HOME};
use crate::{Header, Line, Origin, Transcript};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Keys whose strings name the frame's own structure and are kept when they are identifiers.
const STRUCTURE: &[&str] = &[
    "type", "subtype", "kind", "status", "state", "phase", "outcome", "role", "terminal_reason", "error", "stop_reason", "surface", "code",
    "observation", "control", "model", "name", "sdk_version", "host_release", "origin", "reason", "decision", "behavior", "permissionMode",
];
/// Account configuration: the tools, servers, skills and plugins a person set up are theirs.
const CONFIGURATION: &[&str] = &[
    "tools", "mcp_servers", "mcpServers", "skills", "plugins", "agents", "slash_commands", "commands", "rate_limit_info", "usage", "modelUsage",
];
const PATHS: &[&str] = &["cwd", "path", "file_path", "filePath", "directory", "workspace", "home"];
const IDENTITY: &[&str] = &["account_id", "profile_id", "host_id", "host_instance_id", "process_instance_id", "data_epoch", "account", "organization", "hostname", "machine"];
const DEMO_TIME: &str = "2026-01-01T00:00:00Z";

pub struct Anonymizer {
    literals: Vec<String>,
    ids: BTreeMap<String, usize>,
    texts: BTreeMap<(&'static str, String), usize>,
    labels: BTreeMap<&'static str, usize>,
    paths: BTreeMap<String, usize>,
    identities: BTreeMap<String, usize>,
}

fn pseudo_uuid(number: usize) -> String {
    format!("00000000-0000-4000-8000-{number:012}")
}

fn is_id_key(key: &str) -> bool {
    key == "id" || key == "uuid" || key.ends_with("_id") || key.ends_with("_uuid") || key.ends_with("_uuids") || key.ends_with("Id")
}

fn emptied(value: &Value) -> Value {
    match value {
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Object(_) => Value::Object(Map::new()),
        Value::String(_) => Value::String(String::new()),
        other => other.clone(),
    }
}

impl Anonymizer {
    /// `literals` are private values of this Mac, lowercased: any string that holds one becomes text.
    pub fn new(literals: Vec<String>) -> Self {
        Self { literals, ids: BTreeMap::new(), texts: BTreeMap::new(), labels: BTreeMap::new(), paths: BTreeMap::new(), identities: BTreeMap::new() }
    }

    fn id(&mut self, native: &str) -> String {
        let next = self.ids.len() + 1;
        pseudo_uuid(*self.ids.entry(native.to_owned()).or_insert(next))
    }

    fn marker(&mut self, speaker: &'static str, text: &str) -> String {
        let counter = self.labels.entry(speaker).or_insert(0);
        let known = self.texts.contains_key(&(speaker, text.to_owned()));
        if !known {
            *counter += 1;
        }
        let number = *self.texts.entry((speaker, text.to_owned())).or_insert(*counter);
        format!("«{speaker} {number}»")
    }

    fn path(&mut self, key: &str, native: &str) -> String {
        if key == "cwd" || key == "workspace" || key == "home" {
            return format!("{DEMO_HOME}/demo");
        }
        let next = self.paths.len() + 1;
        format!("{DEMO_HOME}/demo/path-{}", *self.paths.entry(native.to_owned()).or_insert(next))
    }

    fn identity(&mut self, key: &str, native: &str) -> String {
        if native.contains('@') {
            return DEMO_EMAIL.to_owned();
        }
        let next = self.identities.len() + 1;
        format!("demo-{}-{}", key.replace('_', "-"), *self.identities.entry(native.to_owned()).or_insert(next))
    }

    fn private(&self, text: &str) -> bool {
        let lowered = text.to_lowercase();
        self.literals.iter().any(|literal| lowered.contains(literal.as_str()))
    }

    fn string(&mut self, key: Option<&str>, speaker: &'static str, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let key = key.unwrap_or("");
        if IDENTITY.contains(&key) || key.to_lowercase().contains("email") {
            return self.identity(key, text);
        }
        if is_id_key(key) {
            return self.id(text);
        }
        if PATHS.contains(&key) {
            return self.path(key, text);
        }
        if key.ends_with("_at") || key == "timestamp" {
            return DEMO_TIME.to_owned();
        }
        if STRUCTURE.contains(&key) && guard::is_token(text) && !text.contains(['@', '/']) && !self.private(text) {
            return text.to_owned();
        }
        self.marker(speaker, text)
    }

    fn walk(&mut self, key: Option<&str>, speaker: &'static str, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(key, speaker, text)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.walk(key, speaker, item)).collect()),
            Value::Object(map) => {
                let speaker = match map.get("role").or_else(|| map.get("type")).and_then(Value::as_str) {
                    Some("user") => "user text",
                    Some("assistant") => "model text",
                    _ => speaker,
                };
                Value::Object(
                    map.iter()
                        .map(|(name, item)| {
                            let item = if CONFIGURATION.contains(&name.as_str()) { emptied(item) } else { self.walk(Some(name), speaker, item) };
                            (name.clone(), item)
                        })
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }

    /// The transcript line of a frame the host sent, or `None` for the frames the peer writes
    /// itself (hello, challenge, attached) and for those the daemon sent.
    fn inbound(&mut self, frame: &Value) -> Option<Line> {
        let body = frame.get("body")?;
        if !matches!(body.get("type").and_then(Value::as_str), Some("event" | "receipt")) {
            return None;
        }
        let mut frame = frame.clone();
        let body = frame.get_mut("body")?.as_object_mut()?;
        for key in ["fence", "execution", "cursor", "command_id", "command_sha256"] {
            body.remove(key);
        }
        Some(Line::EmitInbound { frame: self.walk(None, "text", &frame) })
    }

    pub fn transcript(&mut self, frames: &[Value], header: Header) -> Transcript {
        let lines = frames.iter().filter_map(|frame| self.inbound(frame)).collect();
        Transcript { header, lines }
    }
}

/// The header of a transcript of a real session, from what the host announced.
pub fn header_of(frames: &[Value], scenario: &str, captured_at: &str) -> Header {
    let hello = frames.iter().find(|frame| frame["body"]["type"] == "hello");
    let model = frames.iter().find_map(|frame| frame["body"]["observation"]["payload"]["model"].as_str());
    Header {
        provider: "claude".into(),
        protocol_version: hello.and_then(|hello| hello["protocol_version"].as_u64()).and_then(|version| u16::try_from(version).ok()).unwrap_or(1),
        sdk_version: hello.and_then(|hello| hello["body"]["sdk_version"].as_str()).unwrap_or("unknown").to_owned(),
        model: model.unwrap_or("unknown").to_owned(),
        captured_at: captured_at.to_owned(),
        scenario: scenario.to_owned(),
        origin: Origin::PortedIncident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    const CANARIES: &[&str] = &[
        "canaryman", "canary.person", "canary-mail", "sk-ant-canary", "eyJcanaryheader", "projectcanaryhorizon", "9f8e7d6c", "canary-account", "canary-host",
        "refactor the canary billing", "Canary Corp", "canary-branch",
    ];

    fn session() -> Vec<Value> {
        let event = |payload: Value| json!({"protocol_version": 1, "body": {"type": "event", "cursor": 4, "fence": {"account_id": "canary-account"}, "execution": {"task_id": "t"}, "observation": {"type": "sdk", "kind": "message", "native": {}, "payload": payload, "failures": []}}});
        vec![
            json!({"protocol_version": 1, "body": {"type": "hello", "sdk_version": "0.3.300", "capabilities": []}}),
            event(json!({"type": "system", "subtype": "init", "model": "model-a", "cwd": "/Users/canaryman/Code/ProjectCanaryHorizon", "session_id": "9f8e7d6c-1111-4222-8333-444455556666",
                "tools": ["Bash", "mcp__canary-corp__deploy"], "mcp_servers": [{"name": "canary-corp", "status": "connected"}], "skills": ["canary-skill"],
                "account": {"email": "canary.person@canary-mail.io", "name": "Canary Corp"}, "branch": "canary-branch", "api_key": "sk-ant-canary0123456789abcdef"})),
            event(json!({"type": "user", "uuid": "9f8e7d6c-aaaa-4bbb-8ccc-ddddeeeeffff", "session_id": "9f8e7d6c-1111-4222-8333-444455556666",
                "message": {"role": "user", "content": [{"type": "text", "text": "refactor the canary billing"}]}})),
            event(json!({"type": "assistant", "session_id": "9f8e7d6c-1111-4222-8333-444455556666", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Reading /Users/canaryman/Code/ProjectCanaryHorizon/billing.rs with Bearer eyJcanaryheader.payloadcanary12345"},
                {"type": "tool_use", "id": "toolu_canary_01", "name": "Read", "input": {"file_path": "/Users/canaryman/Code/ProjectCanaryHorizon/billing.rs"}}]}})),
            event(json!({"type": "system", "subtype": "note", "name": "projectcanaryhorizon"})),
            json!({"protocol_version": 1, "body": {"type": "receipt", "command_id": "c1", "command_sha256": "0".repeat(64), "outcome": {"status": "accepted", "phase": "applied"}}}),
        ]
    }

    fn anonymized() -> Transcript {
        let frames = session();
        let header = header_of(&frames, "canary", "2026-11-15");
        Anonymizer::new(vec!["projectcanaryhorizon".into()]).transcript(&frames, header)
    }

    #[test]
    fn a_session_with_canaries_leaves_without_one() {
        let text = anonymized().render();
        for canary in CANARIES {
            assert!(!text.to_lowercase().contains(&canary.to_lowercase()), "{canary} is still in:\n{text}");
        }
        let findings = guard::scan_transcript(&text, &["projectcanaryhorizon".into()], &BTreeSet::new());
        assert!(findings.is_empty(), "{findings:?}\n{text}");
    }

    #[test]
    fn what_correlates_still_correlates_and_the_configuration_is_gone() {
        let transcript = anonymized();
        let frames: Vec<&Value> = transcript.inbound().collect();
        assert_eq!(frames.len(), 5, "the hello is the peer's to write");
        let payload = |index: usize| &frames[index]["body"]["observation"]["payload"];
        assert_eq!(payload(0)["session_id"], payload(1)["session_id"]);
        assert_eq!(payload(0)["session_id"], payload(2)["session_id"]);
        assert_ne!(payload(0)["session_id"], payload(1)["uuid"]);
        assert_eq!(payload(0)["tools"], json!([]));
        assert_eq!(payload(0)["mcp_servers"], json!([]));
        assert_eq!(payload(0)["cwd"], "/Users/ana/demo");
        assert_eq!(payload(0)["account"]["email"], DEMO_EMAIL);
        assert_eq!(payload(0)["model"], "model-a");
        assert_eq!(payload(1)["message"]["content"][0]["text"], "«user text 1»");
        assert_eq!(payload(2)["message"]["content"][1]["name"], "Read");
        assert!(payload(3)["name"].as_str().unwrap().starts_with('«'), "a structure value that holds a private literal is text");
        assert!(frames[4]["body"].get("command_id").is_none());
    }

    #[test]
    fn the_same_text_gets_the_same_marker_and_a_new_one_the_next() {
        let mut anonymizer = Anonymizer::new(Vec::new());
        let again = [json!({"body": {"type": "event", "observation": {"a": {"text": "one"}, "b": {"text": "two"}, "c": {"text": "one"}}}})];
        let header = header_of(&again, "same", "2026-11-15");
        let transcript = anonymizer.transcript(&again, header);
        let text = transcript.render();
        assert!(text.contains(r#""a":{"text":"«text 1»"},"b":{"text":"«text 2»"},"c":{"text":"«text 1»"}"#), "{text}");
    }

    #[test]
    fn what_the_anonymizer_writes_reads_back() {
        let transcript = anonymized();
        assert_eq!(Transcript::parse(&transcript.render()).unwrap(), transcript);
    }
}
