//! Byte attribution during the snapshot's single JSON serialization, for opt-in observation.
use alondra_view_model::LiveSnapshot;
use serde::Serialize;
use serde_json::ser::{CharEscape, CompactFormatter, Formatter};
use std::{cell::Cell, io::{self, Write}};

/// Values include their nested punctuation; structural bytes are root keys and separators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Breakdown {
    pub nodes: u64,
    pub pull_requests: u64,
    pub activity: u64,
    pub task_history: u64,
    pub task_maps: u64,
    pub other: u64,
    pub structural: u64,
}

impl Breakdown {
    pub(crate) fn total(&self) -> u64 {
        self.nodes + self.pull_requests + self.activity + self.task_history
            + self.task_maps + self.other + self.structural
    }

    fn value(&mut self, field: &str, bytes: u64) {
        let count = match field {
            "nodes" => &mut self.nodes,
            "pull_requests" => &mut self.pull_requests,
            "activity" => &mut self.activity,
            "task_history" => &mut self.task_history,
            "task_options" | "task_since" | "task_usage_until" | "task_started"
                | "task_costs" | "cancel_reasons" | "launch_refusals" | "start_waits" => &mut self.task_maps,
            _ => &mut self.other,
        };
        *count += bytes;
    }
}

pub(crate) fn serialize(live: &LiveSnapshot) -> (Vec<u8>, Breakdown) {
    encode(live)
}

fn encode(value: &impl Serialize) -> (Vec<u8>, Breakdown) {
    let written = Cell::new(0);
    let mut bytes = Vec::new();
    let mut breakdown = Breakdown::default();
    {
        let writer = CountingWriter { bytes: &mut bytes, written: &written };
        let formatter = Components {
            written: &written,
            breakdown: &mut breakdown,
            depth: 0,
            root_key: String::new(),
            in_root_key: false,
            value_start: 0,
        };
        value.serialize(&mut serde_json::Serializer::with_formatter(writer, formatter))
            .expect("live snapshot serialization");
    }
    breakdown.structural = written.get() - breakdown.total();
    (bytes, breakdown)
}

struct CountingWriter<'a> {
    bytes: &'a mut Vec<u8>,
    written: &'a Cell<u64>,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        self.written.set(self.written.get() + bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

struct Components<'a> {
    written: &'a Cell<u64>,
    breakdown: &'a mut Breakdown,
    depth: usize,
    root_key: String,
    in_root_key: bool,
    value_start: u64,
}

impl Formatter for Components<'_> {
    fn begin_array<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        CompactFormatter.begin_array(writer)
    }

    fn end_array<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        CompactFormatter.end_array(writer)
    }

    fn begin_object<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        CompactFormatter.begin_object(writer)
    }

    fn end_object<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        CompactFormatter.end_object(writer)
    }

    fn begin_object_key<W: Write + ?Sized>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if self.depth == 1 {
            self.root_key.clear();
            self.in_root_key = true;
        }
        CompactFormatter.begin_object_key(writer, first)
    }

    fn end_object_key<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.depth == 1 { self.in_root_key = false; }
        CompactFormatter.end_object_key(writer)
    }

    fn write_string_fragment<W: Write + ?Sized>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()> {
        if self.in_root_key { self.root_key.push_str(fragment); }
        CompactFormatter.write_string_fragment(writer, fragment)
    }

    fn write_char_escape<W: Write + ?Sized>(&mut self, writer: &mut W, escape: CharEscape) -> io::Result<()> {
        // Snapshot field names are ASCII constants; escaped unknown keys cannot match them.
        if self.in_root_key { self.root_key.push('\0'); }
        CompactFormatter.write_char_escape(writer, escape)
    }

    fn begin_object_value<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        CompactFormatter.begin_object_value(writer)?;
        if self.depth == 1 { self.value_start = self.written.get(); }
        Ok(())
    }

    fn end_object_value<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.depth == 1 {
            self.breakdown.value(&self.root_key, self.written.get() - self.value_start);
        }
        CompactFormatter.end_object_value(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshots_keep_the_exact_json_bytes_and_account_for_omissions_and_nulls() {
        let populated: LiveSnapshot = serde_json::from_value(json!({
            "nodes": [{
                "agent_id": "fixture-node", "name": "Café 🐦", "working": false,
                "last_message": "quotes: \" and slash \\ and newline\n",
                "last_message_at": null,
                "background": [{"kind": "shell", "label": "task_history", "since": "now"}]
            }],
            "task_options": {"nodes": {"effort": "high"}},
            "task_since": {"activity": "now"},
            "task_costs": {"pull_requests": 17},
            "task_history": {"task": [{"at": "now", "state": "done", "summary": "é\t"}]},
            "activity": {"nodes": [{"since": "now", "summary": "嵌套"}]},
            "issue_error": "escaped\u{0001}",
            "observed_at": 1000
        })).unwrap();
        for live in [LiveSnapshot::default(), populated] {
            let (bytes, counts) = serialize(&live);
            assert_eq!(bytes, serde_json::to_vec(&live).unwrap());
            assert_eq!(counts.total(), bytes.len() as u64);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let length = |field: &str| serde_json::to_vec(&value[field]).unwrap().len() as u64;
            assert_eq!(counts.nodes, length("nodes"));
            assert_eq!(counts.pull_requests, length("pull_requests"));
            assert_eq!(counts.activity, length("activity"));
            assert_eq!(counts.task_history, length("task_history"));
            assert_eq!(counts.task_maps, ["task_options", "task_since", "task_usage_until", "task_started",
                "task_costs", "cancel_reasons", "launch_refusals", "start_waits"].iter().map(|field| length(field)).sum::<u64>());
            assert!(!value.as_object().unwrap().contains_key("metrics"));
            assert!(value["orchestrator"].is_null());
            assert_eq!(value.as_object().unwrap().contains_key("observed_at"), live.observed_at.is_some());
        }
    }

    #[test]
    fn nested_field_names_stay_in_their_root_component() {
        let value = json!({
            "nodes": [{"activity": {"task_history": "é\n"}}],
            "activity": {"nodes": [{"pull_requests": null}]},
            "unclassified": {"nodes": ["not a root node"], "task_costs": 3},
            "no\ndes": ["escaped root key"],
            "task_costs": {"nodes": 9}
        });
        let (bytes, counts) = encode(&value);
        assert_eq!(bytes, serde_json::to_vec(&value).unwrap());
        let length = |field: &str| serde_json::to_vec(&value[field]).unwrap().len() as u64;
        assert_eq!(counts.nodes, length("nodes"));
        assert_eq!(counts.activity, length("activity"));
        assert_eq!(counts.other, length("unclassified") + length("no\ndes"));
        assert_eq!(counts.task_maps, length("task_costs"));
        assert_eq!((counts.pull_requests, counts.task_history), (0, 0));
        let structural = b"{\"nodes\":,\"activity\":,\"unclassified\":,\"no\\ndes\":,\"task_costs\":}".len() as u64;
        assert_eq!(counts.structural, structural);
        assert_eq!(counts.total(), bytes.len() as u64);
    }

    #[test]
    fn attribution_serializes_the_value_once() {
        struct Counted<'a>(&'a Cell<usize>);
        impl Serialize for Counted<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                json!({"nodes": []}).serialize(serializer)
            }
        }
        let calls = Cell::new(0);
        let (bytes, counts) = encode(&Counted(&calls));
        assert_eq!(calls.get(), 1);
        assert_eq!(counts.nodes, 2);
        assert_eq!(counts.total(), bytes.len() as u64);
    }
}
