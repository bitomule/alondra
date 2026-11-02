use super::*;
use serde_json::json;

fn form() -> RequestedForm {
    RequestedForm::from_schema(&json!({
        "type": "object",
        "properties": {
            "remember": {"type": "boolean", "title": "Remember me", "default": false},
            "minutes": {"type": "integer", "minimum": 1, "maximum": 60},
            "email": {"type": "string", "format": "email", "maxLength": 40},
            "color": {"type": "string", "oneOf": [{"const": "#f00", "title": "Red"}, {"const": "#0f0", "title": "Green"}]},
            "size": {"type": "string", "enum": ["s", "m"], "enumNames": ["Small", "Medium"]},
            "tags": {"type": "array", "maxItems": 2, "items": {"anyOf": [{"const": "a", "title": "A"}, {"const": "b", "title": "B"}, {"const": "c", "title": "C"}]}},
        },
        "required": ["remember", "minutes"],
    })).unwrap()
}

fn field<'a>(form: &'a RequestedForm, name: &str) -> &'a RequestedField {
    form.fields.iter().find(|field| field.name == name).unwrap()
}

#[test]
fn every_kind_of_the_mcp_schema_becomes_a_typed_field() {
    let form = form();
    assert_eq!(field(&form, "remember"), &RequestedField {
        name: "remember".into(), title: Some("Remember me".into()), description: None, required: true,
        kind: RequestedFieldKind::Boolean { default: Some(false) },
    });
    assert_eq!(field(&form, "minutes").kind, RequestedFieldKind::Number {
        integer: true, minimum: Some(1.into()), maximum: Some(60.into()), default: None });
    assert_eq!(field(&form, "email").kind, RequestedFieldKind::Text {
        format: Some(TextFormat::Email), min_length: None, max_length: Some(40), default: None });
    let titled = |pairs: &[(&str, &str)]| pairs.iter().map(|(value, title)| RequestedChoice { value: (*value).into(), title: Some((*title).into()) }).collect::<Vec<_>>();
    assert_eq!(field(&form, "color").kind, RequestedFieldKind::Choice { options: titled(&[("#f00", "Red"), ("#0f0", "Green")]), default: None });
    assert_eq!(field(&form, "size").kind, RequestedFieldKind::Choice { options: titled(&[("s", "Small"), ("m", "Medium")]), default: None });
    assert_eq!(field(&form, "tags").kind, RequestedFieldKind::Choices {
        options: titled(&[("a", "A"), ("b", "B"), ("c", "C")]), min_items: None, max_items: Some(2), default: vec![] });
    assert!(!field(&form, "email").required);
    let date = RequestedForm::from_schema(&json!({"type": "object", "properties": {"day": {"type": "string", "format": "date-time"}}})).unwrap();
    assert!(matches!(date.fields[0].kind, RequestedFieldKind::Text { format: Some(TextFormat::DateTime), .. }));
}

#[test]
fn a_schema_outside_the_flat_mcp_subset_has_no_form() {
    for schema in [
        json!(null),
        json!({"type": "array"}),
        json!({"type": "object"}),
        json!({"type": "object", "properties": {"address": {"type": "object", "properties": {}}}}),
        json!({"type": "object", "properties": {"when": {}}}),
        json!({"type": "object", "properties": {"n": {"type": "string", "enum": [1, 2]}}}),
    ] {
        assert_eq!(RequestedForm::from_schema(&schema), None, "{schema}");
    }
}

#[test]
fn typed_values_fill_the_form_and_the_problem_names_the_field() {
    let form = form();
    let content = |value: serde_json::Value| value.as_object().unwrap().clone();
    assert_eq!(form.check(&content(json!({"remember": true, "minutes": 5, "color": "#0f0", "tags": ["a", "c"]}))), Ok(()));
    for (values, problem) in [
        (json!({"minutes": 5}), FormProblem::Missing("remember".into())),
        (json!({"remember": "vale", "minutes": 5}), FormProblem::WrongType("remember".into())),
        (json!({"remember": true, "minutes": "5 min"}), FormProblem::WrongType("minutes".into())),
        (json!({"remember": true, "minutes": 2.5}), FormProblem::WrongType("minutes".into())),
        (json!({"remember": true, "minutes": 61}), FormProblem::OutOfRange("minutes".into())),
        (json!({"remember": true, "minutes": 5, "color": "Red"}), FormProblem::NotAnOption("color".into())),
        (json!({"remember": true, "minutes": 5, "tags": ["a", "a"]}), FormProblem::NotAnOption("tags".into())),
        (json!({"remember": true, "minutes": 5, "tags": ["a", "b", "c"]}), FormProblem::OutOfRange("tags".into())),
        (json!({"remember": true, "minutes": 5, "email": "x".repeat(41)}), FormProblem::OutOfRange("email".into())),
        (json!({"remember": true, "minutes": 5, "nickname": "mo"}), FormProblem::UnknownField("nickname".into())),
    ] {
        assert_eq!(form.check(&content(values.clone())), Err(problem), "{values}");
    }
}

#[test]
fn a_form_travels_as_its_own_json() {
    let form = form();
    let json = serde_json::to_value(&form).unwrap();
    let remember = json["fields"].as_array().unwrap().iter().find(|field| field["name"] == "remember").unwrap();
    assert_eq!(remember, &json!({"name": "remember", "title": "Remember me", "required": true, "kind": {"kind": "boolean", "default": false}}));
    assert_eq!(serde_json::from_value::<RequestedForm>(json).unwrap(), form);
}
