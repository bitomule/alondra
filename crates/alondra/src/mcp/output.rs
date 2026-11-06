use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// Leave room for the JSON-RPC envelope around the tool result.
pub(super) const MAX_REPLY_BYTES: usize = 23 * 1024;
const SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const SNAPSHOT_COUNT: usize = 8;

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Snapshot {
    tool: String,
    args: Value,
    hash: String,
    text: String,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Responses {
    snapshots: std::collections::VecDeque<Snapshot>,
    bytes: usize,
}

impl Responses {
    pub(super) fn lookup(&self, name: &str, args: &Value) -> Option<String> {
        if args["response_offset"].as_u64().unwrap_or(0) == 0 {
            return None;
        }
        let hash = args["response_sha256"].as_str()?;
        let clean = arguments(args);
        self.snapshots
            .iter()
            .find(|s| s.tool == name && s.hash == hash && s.args == clean)
            .map(|s| s.text.clone())
    }

    pub(super) fn remember(&mut self, name: &str, args: &Value, text: &str) -> Result<()> {
        if !read_tool(name) || (fits(&text_reply(text)) && args.get("response_offset").is_none()) {
            return Ok(());
        }
        let clean = arguments(args);
        let hash = digest(text);
        if self
            .snapshots
            .iter()
            .any(|s| s.tool == name && s.hash == hash && s.args == clean)
        {
            return Ok(());
        }
        let bytes = text.len() + serde_json::to_vec(&clean)?.len() + name.len() + hash.len();
        ensure!(
            bytes <= SNAPSHOT_BYTES,
            "invalid_input: response exceeds the 16 MiB snapshot budget; narrow this read with a section, filter or smaller range"
        );
        while self.snapshots.len() >= SNAPSHOT_COUNT || self.bytes + bytes > SNAPSHOT_BYTES {
            if let Some(oldest) = self.snapshots.pop_front() {
                self.bytes -= oldest.bytes;
            }
        }
        self.snapshots.push_back(Snapshot {
            tool: name.into(),
            args: clean,
            hash,
            text: text.into(),
            bytes,
        });
        self.bytes += bytes;
        Ok(())
    }
}

pub(super) fn read_tool(name: &str) -> bool {
    matches!(
        name,
        "tasks_in_progress"
            | "node"
            | "open_cards"
            | "machine"
            | "daemon_log"
            | "get_launch_plan"
            | "read_skill"
            | "read_file"
            | "glob_files"
            | "search_files"
            | "batch_outcomes"
    )
}

pub(super) fn text_reply(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

pub(super) fn fits(reply: &Value) -> bool {
    serde_json::to_vec(reply).expect("JSON value").len() <= MAX_REPLY_BYTES
}

pub(super) fn arguments(args: &Value) -> Value {
    let mut clean = args.clone();
    if let Some(fields) = clean.as_object_mut() {
        fields.retain(|key, _| {
            !matches!(
                key.as_str(),
                "response_offset" | "response_bytes" | "response_sha256"
            )
        });
    }
    clean
}

pub(super) fn reply(name: &str, args: &Value, text: String) -> Result<Value> {
    let full = text_reply(&text);
    if fits(&full) && args.get("response_offset").is_none() {
        return Ok(full);
    }
    if !read_tool(name) {
        return Ok(text_reply(&json!({"completed":true, "detail_omitted":true,
            "see":"Read batch_outcomes and current task state for the recorded outcome. Do not repeat this operation to retrieve its response."}).to_string()));
    }
    let hash = digest(&text);
    let offset = args
        .get("response_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let offset = usize::try_from(offset)?;
    if offset > 0 {
        ensure!(
            args["response_sha256"].as_str() == Some(hash.as_str()),
            "conflict: the response changed, its snapshot expired or response_sha256 is missing; restart this read at response_offset 0"
        );
    }
    ensure!(
        offset <= text.len() && text.is_char_boundary(offset),
        "invalid_input: response_offset must be a UTF-8 boundary within the response"
    );
    let bytes = args
        .get("response_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(12000);
    ensure!(
        (1..=12000).contains(&bytes),
        "invalid_input: response_bytes must be between 1 and 12000"
    );
    let mut end = (offset + bytes as usize).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    ensure!(
        end > offset || offset == text.len(),
        "invalid_input: response_bytes is too small for the next UTF-8 character"
    );
    loop {
        let next = (end < text.len()).then(|| {
            let mut next = args.clone();
            next["response_offset"] = json!(end);
            next["response_sha256"] = json!(hash);
            next
        });
        let page = json!({"fragment": &text[offset..end], "response_offset":offset,
            "total_bytes":text.len(), "response_sha256":hash, "next":next,
            "see":"Repeat this read tool with next as its arguments; concatenate fragments before parsing. Snapshots are session-local and may expire; restart at response_offset 0 if continuation conflicts."});
        let reply = text_reply(&page.to_string());
        if fits(&reply) {
            return Ok(reply);
        }
        let width = end - offset;
        ensure!(
            width > 0,
            "invalid_input: continuation arguments exceed the response budget; shorten them"
        );
        end = offset + width / 2;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        ensure!(
            end > offset,
            "invalid_input: continuation arguments exceed the response budget; shorten them"
        );
    }
}

pub(super) fn bounded_error(mut reply: Value) -> Value {
    if !fits(&reply) {
        let text = reply["content"][0]["text"]
            .as_str()
            .unwrap_or("Alondra error");
        let preview: String = text.chars().take(1000).collect();
        reply = json!({"content":[{"type":"text","text":format!("{preview}\n[Error detail omitted: response size limit]")}],"isError":true});
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_unicode_fragments_are_bounded_and_reassemble_exactly() {
        let source = "\"\\\n\u{0001}猫🙂".repeat(20000);
        let mut args = json!({"node":"worker", "response_bytes":3000});
        let mut restored = String::new();
        loop {
            let answer = reply("node", &args, source.clone()).unwrap();
            assert!(fits(&answer));
            let page: Value =
                serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap();
            restored.push_str(page["fragment"].as_str().unwrap());
            if page["next"].is_null() {
                break;
            }
            args = page["next"].clone();
        }
        assert_eq!(restored, source);
        assert_eq!(arguments(&args), json!({"node":"worker"}));
    }

    #[test]
    fn changed_reads_invalid_offsets_and_mutation_receipts() {
        let text = "x".repeat(50000);
        let answer = reply("machine", &json!({}), text.clone()).unwrap();
        let page: Value =
            serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(reply("machine", &page["next"], format!("{text}changed")).is_err());
        assert!(reply("node", &json!({"response_offset":1}), text.clone()).is_err());
        let receipt = reply("create_task", &json!({}), text).unwrap();
        assert!(fits(&receipt));
        assert!(receipt.to_string().contains("Do not repeat"));
        assert!(fits(&bounded_error(
            json!({"content":[{"type":"text","text":"\\".repeat(50000)}],"isError":true})
        )));
    }

    #[test]
    fn live_reads_continue_from_immutable_tool_and_argument_bound_snapshots() {
        let mut responses = Responses::default();
        let original = "first reading\n".repeat(4000);
        let args = json!({"node":"worker"});
        responses.remember("node", &args, &original).unwrap();
        let first = reply("node", &args, original.clone()).unwrap();
        let first: Value =
            serde_json::from_str(first["content"][0]["text"].as_str().unwrap()).unwrap();
        let next = first["next"].clone();
        responses
            .remember("node", &args, &"new reading\n".repeat(4000))
            .unwrap();
        assert_eq!(responses.lookup("node", &next), Some(original));
        assert!(responses.lookup("machine", &next).is_none());
        let mut changed = next.clone();
        changed["node"] = json!("another");
        assert!(responses.lookup("node", &changed).is_none());
        changed = next;
        changed["response_sha256"] = json!("different");
        assert!(responses.lookup("node", &changed).is_none());
        assert!(responses.lookup("node", &args).is_none());
    }

    #[test]
    fn snapshot_count_and_byte_limits_evict_oldest_and_reject_oversized_reads() {
        let mut responses = Responses::default();
        let text = "x".repeat(30_000);
        for i in 0..=SNAPSHOT_COUNT {
            responses
                .remember("node", &json!({"node":i}), &text)
                .unwrap();
        }
        assert_eq!(responses.snapshots.len(), SNAPSHOT_COUNT);
        let next = json!({"node":0,"response_offset":1,"response_sha256":digest(&text)});
        assert!(responses.lookup("node", &next).is_none());
        let medium = "x".repeat(SNAPSHOT_BYTES / 2);
        responses
            .remember("machine", &json!({"sample":1}), &medium)
            .unwrap();
        responses
            .remember("machine", &json!({"sample":2}), &medium)
            .unwrap();
        assert_eq!(responses.snapshots.len(), 1);
        assert!(responses.bytes <= SNAPSHOT_BYTES);
        drop(medium);
        let oversized = "x".repeat(SNAPSHOT_BYTES);
        assert!(
            responses
                .remember("machine", &json!({}), &oversized)
                .is_err()
        );
        assert_eq!(responses.snapshots.len(), 1);
    }

    #[test]
    fn large_requested_fragments_shrink_to_the_actual_escaped_budget() {
        let source = "\u{0001}".repeat(30_000);
        let answer = reply("machine", &json!({"response_bytes":12000}), source.clone()).unwrap();
        assert!(fits(&answer));
        let page: Value =
            serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap();
        let fragment = page["fragment"].as_str().unwrap();
        assert!(!fragment.is_empty());
        assert!(fragment.len() < 12000);
        assert_eq!(page["next"]["response_offset"], fragment.len());
        assert_eq!(page["next"]["response_sha256"], digest(&source));
    }
}
