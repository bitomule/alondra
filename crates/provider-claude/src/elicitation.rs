//! The form values an accepted MCP elicitation answers with: shared by the cards of a `claude --bg`
//! hook and the SDK's elicitation callback.
use serde_json::{Map, Value};

/// The one text field a form asks for, by name and title: only then do the user's own words fill it.
pub fn only_text_field(schema: &Value) -> Option<(String, String)> {
    let properties = schema.get("properties")?.as_object()?;
    let mut fields = properties.iter();
    let (Some((name, field)), None) = (fields.next(), fields.next()) else { return None };
    let text = field.get("type").and_then(Value::as_str) == Some("string") && ["enum", "oneOf", "anyOf"].iter().all(|key| field.get(key).is_none());
    let title = field.get("title").and_then(Value::as_str).unwrap_or(name);
    text.then(|| (name.clone(), title.to_owned()))
}

/// The values for an accepted elicitation: those the user filled in its form, as they are; for a
/// form of one text field, the words they wrote; otherwise none.
pub fn elicitation_content(schema: &Value, said: &str, filled: Option<&Map<String, Value>>) -> Map<String, Value> {
    if let Some(filled) = filled {
        return filled.clone();
    }
    match only_text_field(schema) {
        Some((name, _)) if !said.is_empty() => Map::from_iter([(name, Value::String(said.to_owned()))]),
        _ => Map::new(),
    }
}
