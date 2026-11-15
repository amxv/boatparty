//! `docs/json.md`, the `--json` contract, as tests read it: each shape is a
//! `<!-- shape: NAME -->` line followed by a ```json example.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

/// The shapes `docs/json.md` documents, by name.
pub fn documented() -> BTreeMap<String, Value> {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/json.md"))
        .expect("docs/json.md exists");
    let mut shapes = BTreeMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .trim()
            .strip_prefix("<!-- shape: ")
            .and_then(|rest| rest.strip_suffix(" -->"))
        else {
            continue;
        };
        assert_eq!(lines.next().map(str::trim), Some("```json"), "{name}");
        let body: Vec<&str> = lines
            .by_ref()
            .take_while(|line| line.trim() != "```")
            .collect();
        let value: Value = serde_json::from_str(&body.join("\n"))
            .unwrap_or_else(|error| panic!("shape {name} is not JSON: {error}"));
        assert!(
            shapes.insert(name.to_string(), value).is_none(),
            "shape {name} is documented twice"
        );
    }
    shapes
}

/// Whether `actual` has the documented shape `doc`; differences go to `errors`.
pub fn conform(doc: &Value, actual: &Value, path: &str, top: bool, errors: &mut Vec<String>) {
    match (doc, actual) {
        (_, Value::Null) => {}
        (Value::Object(doc), Value::Object(actual)) => {
            if doc.contains_key("*") {
                return;
            }
            for key in actual.keys() {
                if !doc.contains_key(key) {
                    errors.push(format!("{path}.{key} is not documented"));
                }
            }
            if top {
                for key in doc.keys() {
                    if !actual.contains_key(key) {
                        errors.push(format!("{path}.{key} is documented but missing"));
                    }
                }
            }
            for (key, value) in actual {
                if let Some(expected) = doc.get(key) {
                    conform(expected, value, &format!("{path}.{key}"), false, errors);
                }
            }
        }
        (Value::Object(doc), other) if !doc.is_empty() => {
            errors.push(format!("{path} is {other}, documented as an object"));
        }
        (Value::Array(doc), Value::Array(actual)) => {
            if let Some(first) = doc.first() {
                for item in actual {
                    conform(first, item, &format!("{path}[]"), false, errors);
                }
            }
        }
        (Value::Array(_), other) => {
            errors.push(format!("{path} is {other}, documented as an array"));
        }
        _ => {}
    }
}

/// Panic unless `actual` has the documented shape `shape`.
pub fn assert_shape(shape: &str, actual: &Value) {
    let shapes = documented();
    let doc = shapes
        .get(shape)
        .unwrap_or_else(|| panic!("docs/json.md has no shape {shape}"));
    let mut errors = Vec::new();
    conform(doc, actual, shape, true, &mut errors);
    assert!(errors.is_empty(), "{shape}: {errors:?}\n{actual:#}");
}
