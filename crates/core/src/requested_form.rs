//! The form an MCP server asks the user to fill (the `requestedSchema` of an elicitation in form
//! mode), typed so a screen draws one control per field, and the check of the values a screen
//! sends back. The schema is the MCP subset: a flat object of text, number, boolean and enum
//! properties (<https://modelcontextprotocol.io/specification/2026-07-28/client/elicitation>).
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// The most a form's values weigh, like the text of an answer.
pub const MAX_CONTENT_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedForm {
    pub fields: Vec<RequestedField>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedField {
    /// The key of its value in the answer.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub kind: RequestedFieldKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestedFieldKind {
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<TextFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_length: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_length: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    Number {
        /// Only whole numbers.
        integer: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Number>,
    },
    Boolean {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<bool>,
    },
    /// One of the options.
    Choice {
        options: Vec<RequestedChoice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    /// Any number of the options, each once.
    Choices {
        options: Vec<RequestedChoice>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_items: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_items: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        default: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedChoice {
    /// What the answer carries.
    pub value: String,
    /// What the user reads, when the server names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

/// Why a form's values do not fit it, naming the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormProblem {
    TooLarge,
    UnknownField(String),
    Missing(String),
    WrongType(String),
    OutOfRange(String),
    NotAnOption(String),
}

impl std::fmt::Display for FormProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormProblem::TooLarge => write!(f, "The form's values weigh more than {MAX_CONTENT_BYTES} bytes"),
            FormProblem::UnknownField(name) => write!(f, "The form has no field {name}"),
            FormProblem::Missing(name) => write!(f, "The form needs a value for {name}"),
            FormProblem::WrongType(name) => write!(f, "The value of {name} is not of the field's type"),
            FormProblem::OutOfRange(name) => write!(f, "The value of {name} is outside the field's limits"),
            FormProblem::NotAnOption(name) => write!(f, "The value of {name} is not one of its options"),
        }
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn whole(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn number(value: &Value, key: &str) -> Option<Number> {
    match value.get(key) {
        Some(Value::Number(number)) => Some(number.clone()),
        _ => None,
    }
}

/// `enum` (with the older `enumNames` as titles) or `oneOf`/`anyOf` of `{const, title}`.
fn choices(schema: &Value, titled: &str) -> Option<Vec<RequestedChoice>> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let names = schema.get("enumNames").and_then(Value::as_array);
        return values.iter().enumerate().map(|(index, value)| Some(RequestedChoice {
            value: value.as_str()?.to_owned(),
            title: names.and_then(|names| names.get(index)).and_then(Value::as_str).map(str::to_owned),
        })).collect();
    }
    schema.get(titled).and_then(Value::as_array)?.iter()
        .map(|option| Some(RequestedChoice { value: text(option, "const")?, title: text(option, "title") }))
        .collect()
}

fn kind(schema: &Value) -> Option<RequestedFieldKind> {
    Some(match schema.get("type").and_then(Value::as_str)? {
        "string" if schema.get("enum").is_some() || schema.get("oneOf").is_some() => RequestedFieldKind::Choice {
            options: choices(schema, "oneOf")?,
            default: text(schema, "default"),
        },
        "string" => RequestedFieldKind::Text {
            format: schema.get("format").and_then(|format| serde_json::from_value(Value::String(format.as_str()?.replace('-', "_"))).ok()),
            min_length: whole(schema, "minLength"),
            max_length: whole(schema, "maxLength"),
            default: text(schema, "default"),
        },
        kind @ ("number" | "integer") => RequestedFieldKind::Number {
            integer: kind == "integer",
            minimum: number(schema, "minimum"),
            maximum: number(schema, "maximum"),
            default: number(schema, "default"),
        },
        "boolean" => RequestedFieldKind::Boolean { default: schema.get("default").and_then(Value::as_bool) },
        "array" => RequestedFieldKind::Choices {
            options: choices(schema.get("items")?, "anyOf")?,
            min_items: whole(schema, "minItems"),
            max_items: whole(schema, "maxItems"),
            default: schema.get("default").and_then(Value::as_array).into_iter().flatten()
                .filter_map(Value::as_str).map(str::to_owned).collect(),
        },
        _ => return None,
    })
}

impl RequestedForm {
    /// The form a `requestedSchema` describes, or none when it is not the flat MCP subset: then
    /// the card is answered with words, never with a form that guesses.
    pub fn from_schema(schema: &Value) -> Option<Self> {
        if schema.get("type").is_some_and(|kind| kind != "object") {
            return None;
        }
        let required: Vec<&str> = schema.get("required").and_then(Value::as_array).into_iter().flatten()
            .filter_map(Value::as_str).collect();
        let fields = schema.get("properties")?.as_object()?.iter().map(|(name, property)| Some(RequestedField {
            name: name.clone(),
            title: text(property, "title"),
            description: text(property, "description"),
            required: required.contains(&name.as_str()),
            kind: kind(property)?,
        })).collect::<Option<Vec<_>>>()?;
        Some(Self { fields })
    }

    /// Whether `content` fills this form: only its fields, every required one, each of its type
    /// and within its limits.
    pub fn check(&self, content: &Map<String, Value>) -> Result<(), FormProblem> {
        if serde_json::to_vec(content).map_or(true, |bytes| bytes.len() > MAX_CONTENT_BYTES) {
            return Err(FormProblem::TooLarge);
        }
        if let Some(name) = content.keys().find(|name| !self.fields.iter().any(|field| &field.name == *name)) {
            return Err(FormProblem::UnknownField(name.clone()));
        }
        for field in &self.fields {
            match content.get(&field.name) {
                None if field.required => return Err(FormProblem::Missing(field.name.clone())),
                None => {}
                Some(value) => field.kind.check(value).map_err(|problem| problem(field.name.clone()))?,
            }
        }
        Ok(())
    }
}

type Problem = fn(String) -> FormProblem;

impl RequestedFieldKind {
    fn check(&self, value: &Value) -> Result<(), Problem> {
        let within = |n: Option<f64>, min: Option<f64>, max: Option<f64>| match n {
            Some(n) if min.is_none_or(|min| n >= min) && max.is_none_or(|max| n <= max) => Ok(()),
            _ => Err(FormProblem::OutOfRange as Problem),
        };
        let known = |options: &[RequestedChoice], value: &str| options.iter().any(|option| option.value == value);
        match (self, value) {
            (RequestedFieldKind::Text { min_length, max_length, .. }, Value::String(text)) => {
                within(Some(text.chars().count() as f64), min_length.map(|n| n as f64), max_length.map(|n| n as f64))
            }
            (RequestedFieldKind::Number { integer, minimum, maximum, .. }, Value::Number(n)) => {
                if *integer && !(n.is_i64() || n.is_u64()) {
                    return Err(FormProblem::WrongType);
                }
                within(n.as_f64(), minimum.as_ref().and_then(Number::as_f64), maximum.as_ref().and_then(Number::as_f64))
            }
            (RequestedFieldKind::Boolean { .. }, Value::Bool(_)) => Ok(()),
            (RequestedFieldKind::Choice { options, .. }, Value::String(choice)) => {
                if known(options, choice) { Ok(()) } else { Err(FormProblem::NotAnOption) }
            }
            (RequestedFieldKind::Choices { options, min_items, max_items, .. }, Value::Array(picked)) => {
                let mut seen = Vec::with_capacity(picked.len());
                for choice in picked {
                    let Some(choice) = choice.as_str() else { return Err(FormProblem::WrongType) };
                    if !known(options, choice) || seen.contains(&choice) {
                        return Err(FormProblem::NotAnOption);
                    }
                    seen.push(choice);
                }
                within(Some(picked.len() as f64), min_items.map(|n| n as f64), max_items.map(|n| n as f64))
            }
            _ => Err(FormProblem::WrongType),
        }
    }
}

#[cfg(test)]
#[path = "requested_form_tests.rs"]
mod tests;
