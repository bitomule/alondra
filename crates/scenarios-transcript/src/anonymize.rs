//! Turns the frames a real session's host wrote into a transcript that can be committed: free text
//! becomes stable markers, identity moves to the demo world, ids to ids of a visible pattern, and
//! the account's configuration is emptied. What stays is the structure: the order of the frames,
//! the names of kinds and tools, and which id is which. A key of an object is kept only when it is a
//! name of the structure; any other becomes a marker too.

use crate::guard::{self, DEMO_EMAIL, DEMO_HOME};
use crate::{Header, Line, Origin, Transcript};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Keys whose strings name the frame's own structure and are kept when they are identifiers.
const STRUCTURE: &[&str] = &[
    "type", "subtype", "kind", "status", "state", "phase", "outcome", "role", "terminal_reason", "error", "stop_reason", "surface", "code",
    "observation", "control", "model", "name", "sdk_version", "host_release", "origin", "reason", "decision", "behavior", "permissionMode", "method",
];
/// The longest value of a structure key that is kept: a name, not a sentence.
const MAX_STRUCTURE: usize = 40;
/// What the daemon fills in on each command; the transcript does not record it.
const COMMAND_RUN_FIELDS: &[&str] = &["command_id", "deadline_ms", "fence"];
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
    servers: BTreeMap<String, usize>,
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
        Self { literals, ids: BTreeMap::new(), texts: BTreeMap::new(), labels: BTreeMap::new(), paths: BTreeMap::new(), identities: BTreeMap::new(), servers: BTreeMap::new() }
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

    /// Whether a text carries a private literal or any shape the guard refuses.
    fn private(&self, text: &str) -> bool {
        !guard::scan_line(text, &self.literals).is_empty()
    }

    /// A key that names the structure stays; any other is text.
    fn key(&mut self, key: &str) -> Option<String> {
        (!guard::is_key(key) || self.private(key)).then(|| self.marker("key", key))
    }

    /// A name of the structure stays when it is a word and nothing private; a tool of an MCP
    /// server keeps that it is one, under the server of the demo world.
    fn structure(&mut self, text: &str) -> Option<String> {
        if let Some(rest) = text.strip_prefix("mcp__") {
            let server = rest.split("__").next().unwrap_or_default().to_owned();
            let next = self.servers.len() + 1;
            let number = *self.servers.entry(server).or_insert(next);
            let tool = self.marker("tool", text);
            return Some(format!("mcp__demo-{number}__{}", tool.trim_start_matches('«').trim_end_matches('»').replace(' ', "-")));
        }
        (text.len() <= MAX_STRUCTURE && guard::is_word(text) && !self.private(text)).then(|| text.to_owned())
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
        if STRUCTURE.contains(&key)
            && let Some(kept) = self.structure(text)
        {
            return kept;
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
                        .map(|(name, item)| match self.key(name) {
                            Some(marker) => {
                                let item = self.walk(None, speaker, item);
                                (marker, item)
                            }
                            None if CONFIGURATION.contains(&name.as_str()) => (name.clone(), emptied(item)),
                            None => (name.clone(), self.walk(Some(name), speaker, item)),
                        })
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }

    /// The transcript line of a frame: what the host sent, what the daemon sent it, or `None` for
    /// the frames the peer writes itself (hello, challenge, attached) and the acknowledgements.
    fn line(&mut self, frame: &Value) -> Option<Line> {
        let mut frame = frame.clone();
        let body = frame.get_mut("body")?.as_object_mut()?;
        match body.get("type").and_then(Value::as_str) {
            Some("event" | "receipt") => {
                for key in ["fence", "execution", "cursor", "command_id", "command_sha256"] {
                    body.remove(key);
                }
                Some(Line::EmitInbound { frame: self.walk(None, "text", &frame) })
            }
            Some("command") => {
                for key in COMMAND_RUN_FIELDS {
                    body.remove(*key);
                }
                if let Some(conversation) = body.remove("execution").and_then(|execution| execution.get("conversation_id").cloned()) {
                    body.insert("execution".into(), serde_json::json!({"conversation_id": conversation}));
                }
                Some(Line::ExpectOutbound { frame: self.walk(None, "user text", &frame) })
            }
            _ => None,
        }
    }

    pub fn transcript(&mut self, frames: &[Value], header: Header) -> Transcript {
        let lines = frames.iter().filter_map(|frame| self.line(frame)).collect();
        Transcript { header, lines }
    }
}

/// The header of a transcript of a real session, from what the host announced.
pub fn header_of(frames: &[Value], provider: &str, scenario: &str, captured_at: &str) -> Header {
    let hello = frames.iter().find(|frame| frame["body"]["type"] == "hello");
    let model = frames.iter().find_map(|frame| frame["body"]["observation"]["payload"]["model"].as_str());
    Header {
        provider: provider.to_owned(),
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
        "refactor the canary billing", "Canary Corp", "canary-corp", "canary-branch", "canary-org", "CanaryDisk", "AKIACANARY", "canary-conversation", "canary-input",
    ];
    const CONVERSATION: &str = "9f8e7d6c-2222-4333-8444-555566667777";

    fn session() -> Vec<Value> {
        let event = |payload: Value| json!({"protocol_version": 1, "body": {"type": "event", "cursor": 4, "fence": {"account_id": "canary-account"}, "execution": {"task_id": "t", "conversation_id": CONVERSATION}, "observation": {"type": "sdk", "kind": "message", "native": {}, "payload": payload, "failures": []}}});
        vec![
            json!({"protocol_version": 1, "body": {"type": "hello", "sdk_version": "0.3.300", "capabilities": []}}),
            json!({"protocol_version": 1, "body": {"type": "command", "command_id": "c1", "deadline_ms": 30000, "fence": {"account_id": "canary-account", "host_id": "canary-host"},
                "execution": {"task_id": "t", "agent_id": "canary-host", "conversation_id": CONVERSATION},
                "action": {"type": "start", "input_id": "9f8e7d6c-0000-4000-8000-canary000001", "cwd": "/Users/canaryman/Code/ProjectCanaryHorizon", "prompt": "refactor the canary billing"}}}),
            event(json!({"type": "system", "subtype": "init", "model": "model-a", "cwd": "/Users/canaryman/Code/ProjectCanaryHorizon", "session_id": "9f8e7d6c-1111-4222-8333-444455556666",
                "tools": ["Bash", "mcp__canary-corp__deploy"], "mcp_servers": [{"name": "canary-corp", "status": "connected"}], "skills": ["canary-skill"],
                "account": {"email": "canary.person@canary-mail.io", "name": "Canary Corp"}, "branch": "canary-branch", "api_key": "sk-ant-canary0123456789abcdef",
                "trust": {"/Users/canaryman/Code/ProjectCanaryHorizon": true, "canary.person@canary-mail.io": "owner", "refactor the canary billing": 1, "projectcanaryhorizon": 2}})),
            event(json!({"type": "user", "uuid": "9f8e7d6c-aaaa-4bbb-8ccc-ddddeeeeffff", "session_id": "9f8e7d6c-1111-4222-8333-444455556666",
                "message": {"role": "user", "content": [{"type": "text", "text": "refactor the canary billing"}]}})),
            event(json!({"type": "assistant", "session_id": "9f8e7d6c-1111-4222-8333-444455556666", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Reading /Users/canaryman/Code/ProjectCanaryHorizon/billing.rs with Bearer eyJcanaryheader.payloadcanary12345"},
                {"type": "tool_use", "id": "toolu_canary_01", "name": "Read", "input": {"file_path": "/Users/canaryman/Code/ProjectCanaryHorizon/billing.rs"}},
                {"type": "tool_use", "id": "toolu_canary_02", "name": "mcp__canary-corp__deploy", "input": {}}]}})),
            event(json!({"type": "system", "subtype": "note", "name": "projectcanaryhorizon", "model": "/Volumes/CanaryDisk/model", "code": "AKIACANARY0123456789",
                "error": "https://github.com/canary-org/canary-repo/pull/7", "reason": "sk-ant-canary0123456789abcdef", "logicalConversationId": CONVERSATION})),
            json!({"protocol_version": 1, "body": {"type": "receipt", "command_id": "c1", "command_sha256": "0".repeat(64), "outcome": {"status": "accepted", "phase": "applied"}}}),
            json!({"protocol_version": 1, "body": {"type": "acknowledge", "cursor": 4}}),
        ]
    }

    fn anonymized() -> Transcript {
        let frames = session();
        let header = header_of(&frames, "claude", "canary", "2026-11-15");
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
        assert_eq!(frames.len(), 5, "the hello and the acknowledgement are the peer's to write");
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
        assert!(frames[4]["body"].get("command_id").is_none());
    }

    #[test]
    fn a_key_that_is_not_a_name_of_the_structure_is_text() {
        let transcript = anonymized();
        let trust = &transcript.inbound().next().unwrap()["body"]["observation"]["payload"]["trust"];
        let keys: Vec<&String> = trust.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 4);
        assert!(keys.iter().all(|key| key.starts_with("«key ")), "{keys:?}");
    }

    #[test]
    fn a_value_under_a_structure_key_stays_only_when_it_is_a_word_with_nothing_private() {
        let transcript = anonymized();
        let frames: Vec<&Value> = transcript.inbound().collect();
        let note = &frames[3]["body"]["observation"]["payload"];
        for key in ["name", "model", "code", "error", "reason"] {
            assert!(note[key].as_str().unwrap().starts_with('«'), "{key} is {}", note[key]);
        }
        assert_eq!(note["subtype"], "note");
        let tool = frames[2]["body"]["observation"]["payload"]["message"]["content"][2]["name"].as_str().unwrap();
        assert!(tool.starts_with("mcp__demo-1__"), "a tool of an MCP server stays one, of a demo server: {tool}");
    }

    #[test]
    fn what_the_daemon_sent_is_expected_with_the_ids_of_what_the_host_answered() {
        let transcript = anonymized();
        let outbound: Vec<&Value> = transcript.outbound().collect();
        assert_eq!(outbound.len(), 1);
        let body = &outbound[0]["body"];
        assert!(body.get("command_id").is_none() && body.get("deadline_ms").is_none() && body.get("fence").is_none());
        assert_eq!(body["execution"].as_object().unwrap().len(), 1, "of the execution only the conversation is the transcript's");
        assert_eq!(body["action"]["cwd"], "/Users/ana/demo");
        assert!(body["action"]["prompt"].as_str().unwrap().starts_with("«user text"));
        let note = &transcript.inbound().nth(3).unwrap()["body"]["observation"]["payload"];
        assert_eq!(body["execution"]["conversation_id"], note["logicalConversationId"], "the conversation keeps one id across the frames");
        assert!(matches!(transcript.lines[0], Line::ExpectOutbound { .. }), "the command comes first, as it was sent");
    }

    #[test]
    fn the_same_text_gets_the_same_marker_and_a_new_one_the_next() {
        let mut anonymizer = Anonymizer::new(Vec::new());
        let again = [json!({"body": {"type": "event", "observation": {"a": {"text": "one"}, "b": {"text": "two"}, "c": {"text": "one"}}}})];
        let header = header_of(&again, "claude", "same", "2026-11-15");
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
