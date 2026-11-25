use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::problem::{RESULTS_MAX_BYTES, problem};
use crate::{Code, Problem};

struct Case {
    name: Option<String>,
    depth: usize,
    passed: bool,
}

fn invalid(detail: impl std::fmt::Display) -> Problem {
    problem(Code::InvalidResults, "", 0, format!("the test results are not JUnit XML: {detail}"))
}

fn local_name(element: &BytesStart<'_>) -> String {
    let name = element.local_name();
    let local: &str = name.as_ref();
    local.to_string()
}

fn case_name(element: &BytesStart<'_>) -> Result<Option<String>, Problem> {
    let attribute = element.try_get_attribute("name").map_err(invalid)?;
    match attribute {
        Some(attribute) => attribute.normalized_value(XmlVersion::Implicit1_0).map(|value| Some(value.into_owned())).map_err(invalid),
        None => Ok(None),
    }
}

fn spoils_case(name: &str) -> bool {
    matches!(name, "failure" | "error" | "skipped")
}

/// Every named test case with whether it passed.
pub(crate) fn read(bytes: &[u8]) -> Result<Vec<(String, bool)>, Problem> {
    if bytes.len() > RESULTS_MAX_BYTES {
        return Err(problem(
            Code::TooLarge,
            "",
            0,
            format!("the test results have {} bytes, more than {RESULTS_MAX_BYTES}", bytes.len()),
        ));
    }
    let source = std::str::from_utf8(bytes).map_err(invalid)?;
    let mut reader = Reader::from_str(source);
    let mut cases = Vec::new();
    let mut depth = 0usize;
    let mut roots = 0usize;
    let mut case: Option<Case> = None;
    loop {
        match reader.read_event().map_err(invalid)? {
            Event::Start(element) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
                let name = local_name(&element);
                if name == "testcase" {
                    case = Some(Case { name: case_name(&element)?, depth, passed: true });
                } else if spoils_case(&name) {
                    if let Some(open) = case.as_mut() {
                        open.passed = false;
                    }
                }
            }
            Event::Empty(element) => {
                if depth == 0 {
                    roots += 1;
                }
                let name = local_name(&element);
                if name == "testcase" {
                    if let Some(found) = case_name(&element)? {
                        cases.push((found, true));
                    }
                } else if spoils_case(&name) {
                    if let Some(open) = case.as_mut() {
                        open.passed = false;
                    }
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return Err(invalid("an element closes that never opened"));
                }
                if let Some(open) = case.take() {
                    if open.depth == depth {
                        if let Some(found) = open.name {
                            cases.push((found, open.passed));
                        }
                    } else {
                        case = Some(open);
                    }
                }
                depth -= 1;
            }
            Event::Text(content) => {
                if depth == 0 && !content.trim().is_empty() {
                    return Err(invalid("text outside the root element"));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 || roots != 1 {
        return Err(invalid("the document must have exactly one root element, closed"));
    }
    Ok(cases)
}
