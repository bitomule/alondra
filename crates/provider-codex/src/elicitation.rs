//! What an MCP server's request for input asks, and the form values a user's written answer gives
//! it: shared by the cards of a `codex --bg` hook and the SDK's elicitation callback.
use serde_json::{Map, Value};

fn field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The fields an MCP server's form asks for: (name, title, JSON type).
pub fn elicitation_fields(schema: &Value) -> Vec<(String, String, String)> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(name, f)| (name.clone(), field(f, "title").unwrap_or_else(|| name.clone()), field(f, "type").unwrap_or_else(|| "string".into())))
                .collect()
        })
        .unwrap_or_default()
}

/// The form values for an accepted elicitation: a JSON object as written, the whole answer for a
/// one-field form, or `name: value` lines, each typed as the form's schema says.
pub fn elicitation_content(schema: &Value, said: &str) -> Map<String, Value> {
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(said) {
        return o;
    }
    let fields = elicitation_fields(schema);
    let typed = |kind: &str, v: &str| -> Value {
        let v = v.trim();
        match kind {
            "integer" => v.parse::<i64>().map(Value::from).unwrap_or_else(|_| v.into()),
            "number" => v.parse::<f64>().map(Value::from).unwrap_or_else(|_| v.into()),
            "boolean" => Value::Bool(matches!(v.to_lowercase().as_str(), "true" | "yes" | "y" | "sí" | "si" | "1")),
            _ => v.into(),
        }
    };
    let mut content = Map::new();
    if said.is_empty() {
        return content;
    }
    if let [(name, _, kind)] = fields.as_slice() {
        content.insert(name.clone(), typed(kind, said));
        return content;
    }
    for line in said.lines() {
        let Some((key, value)) = line.split_once(':').or_else(|| line.split_once('=')) else { continue };
        let key = key.trim();
        if let Some((name, _, kind)) = fields.iter().find(|(name, title, _)| name.eq_ignore_ascii_case(key) || title.eq_ignore_ascii_case(key)) {
            content.insert(name.clone(), typed(kind, value));
        }
    }
    content
}
