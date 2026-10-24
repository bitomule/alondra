//! A tool call's arguments checked against the inputSchema the tool publishes, before anything
//! reads them. A node that sends a field the tool does not have, or a list or object as text,
//! learns exactly that, in the tool's own words, and nothing reaches alondrad: no refusal is
//! counted against its card and no internal field name leaks back as advice.
//!
//! Only the shape is checked here (types, required and unknown fields, enums, the kind of a
//! union). Lengths, digests and every other rule on the values stay with alondrad, whose
//! refusals say how to fix each one.

use serde_json::Value;

pub(crate) fn check(tool: &str, schema: &Value, arguments: &Value) -> Result<(), String> {
    let mut problems = vec![];
    shape(schema, arguments, "", &mut problems);
    if problems.is_empty() {
        return Ok(());
    }
    Err(format!(
        "invalid_input: the arguments of {tool} do not match its schema: {}. Nothing was sent: fix them and call {tool} again with the same ids.",
        problems.join("; ")
    ))
}

fn shape(schema: &Value, value: &Value, path: &str, problems: &mut Vec<String>) {
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        if !value.is_object() && types(schema).iter().any(|t| *t == "object") {
            problems.push(wrong_type(path, &["object"], value));
            return;
        }
        match branch(branches, value, path) {
            Ok(chosen) => shape(chosen, value, path, problems),
            Err(problem) => problems.push(problem),
        }
        return;
    }
    let allowed = types(schema);
    if !allowed.is_empty() && !allowed.iter().any(|t| is(t, value)) {
        problems.push(wrong_type(path, &allowed, value));
        return;
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        problems.push(format!("`{}` must be one of {}, got {value}", name(path), listed(choices)));
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        problems.push(format!("`{}` must be {expected}, got {value}", name(path)));
    }
    match value {
        Value::Object(fields) => object(schema, fields, path, problems),
        Value::Array(items) => {
            if let Some(item) = schema.get("items") {
                for (i, each) in items.iter().enumerate() {
                    shape(item, each, &format!("{path}[{i}]"), problems);
                }
            }
        }
        _ => {}
    }
}

fn object(schema: &Value, fields: &serde_json::Map<String, Value>, path: &str, problems: &mut Vec<String>) {
    let properties = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let takes_null = |key: &str| properties.and_then(|p| p.get(key)).is_some_and(|property| types(property).contains(&"null"));
    let missing: Vec<String> = required.iter()
        .filter(|key| !takes_null(key) && fields.get(**key).is_none_or(Value::is_null))
        .map(|key| format!("`{}`", join(path, key))).collect();
    if !missing.is_empty() {
        problems.push(format!("missing {}", missing.join(", ")));
    }
    for (key, value) in fields {
        let at = join(path, key);
        match properties.and_then(|p| p.get(key)) {
            Some(_) if value.is_null() && !required.contains(&key.as_str()) => {}
            Some(_) if value.is_null() && !takes_null(key) => {}
            Some(property) => shape(property, value, &at, problems),
            None => match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    let known: Vec<&str> = properties.into_iter().flat_map(|p| p.keys()).map(String::as_str).collect();
                    let owner = if path.is_empty() { "it".to_owned() } else { format!("`{path}`") };
                    problems.push(format!("{owner} has no field `{key}`: its fields are {}", known.join(", ")));
                }
                Some(extra @ Value::Object(_)) => shape(extra, value, &at, problems),
                _ => {}
            },
        }
    }
}

/// The form of a union the value takes: the one its `kind` names, or the first it fits.
fn branch<'a>(branches: &'a [Value], value: &Value, path: &str) -> Result<&'a Value, String> {
    let kinds: Vec<&Value> = branches.iter().filter_map(|b| b.pointer("/properties/kind/const")).collect();
    if kinds.len() == branches.len() {
        let kind = value.get("kind");
        return branches.iter().find(|b| b.pointer("/properties/kind/const") == kind).ok_or_else(|| match kind {
            None | Some(Value::Null) => format!("missing `{}`: one of {}", join(path, "kind"), listed(&kinds.into_iter().cloned().collect::<Vec<_>>())),
            Some(kind) => format!("`{}` must be one of {}, got {kind}", join(path, "kind"), listed(&kinds.into_iter().cloned().collect::<Vec<_>>())),
        });
    }
    let mut first = None;
    for candidate in branches {
        let mut problems = vec![];
        shape(candidate, value, path, &mut problems);
        if problems.is_empty() {
            return Ok(candidate);
        }
        first.get_or_insert(problems);
    }
    Err(first.map(|p| p.join("; ")).unwrap_or_else(|| format!("`{}` matches none of its forms", name(path))))
}

fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn is(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn wrong_type(path: &str, allowed: &[&str], value: &Value) -> String {
    let expected: Vec<&str> = allowed.iter().map(|t| article(t)).collect();
    let mut problem = format!("`{}` must be {}, got {}", name(path), expected.join(" or "), article(kind_of(value)));
    if value.is_string() && allowed.iter().any(|t| *t == "object" || *t == "array") {
        problem.push_str(": send it as JSON itself, not as text that holds JSON");
    }
    problem
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::Null => "null",
    }
}

fn article(kind: &str) -> &'static str {
    match kind {
        "object" => "an object",
        "array" => "a list",
        "string" => "a string",
        "boolean" => "true or false",
        "integer" => "an integer",
        "number" => "a number",
        "null" => "null",
        _ => "something else",
    }
}

fn listed(choices: &[Value]) -> String {
    choices.iter().map(|c| c.as_str().map_or_else(|| c.to_string(), str::to_owned)).collect::<Vec<_>>().join(", ")
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_owned() } else { format!("{path}.{key}") }
}

fn name(path: &str) -> &str {
    if path.is_empty() { "arguments" } else { path }
}
