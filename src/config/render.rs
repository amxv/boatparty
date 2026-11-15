//! TOML text for `config show`: the sections in schema order, each value
//! optionally followed by a comment naming where it came from. The output
//! is itself a valid boatparty.toml.

use std::collections::BTreeMap;
use std::fmt::Write;

use toml::{Table, Value};

use super::Origin;
use super::merge::step_path;
use super::schema::{Field, Kind, TOP, join};

/// Render `table`. With `origins`, every value carries a `# <origin>`
/// comment.
pub fn render(table: &Table, origins: Option<&BTreeMap<String, Origin>>) -> String {
    let mut out = String::new();
    let comment = |path: &str| -> String {
        match origins.and_then(|origins| origins.get(path)) {
            Some(origin) => format!("  # {origin}"),
            None => String::new(),
        }
    };
    for field in TOP {
        let Some(value) = table.get(field.key) else {
            continue;
        };
        match field.kind {
            Kind::Section(fields) => {
                let Value::Table(section) = value else {
                    continue;
                };
                section_text(&mut out, field.key, section, fields, &comment);
            }
            Kind::StrMap => {
                let Value::Table(map) = value else { continue };
                if map.is_empty() {
                    continue;
                }
                blank(&mut out);
                let _ = writeln!(out, "[{}]", field.key);
                for (key, item) in map {
                    let _ = writeln!(
                        out,
                        "{} = {}{}",
                        key_text(key),
                        value_text(item),
                        comment(&join(field.key, key))
                    );
                }
            }
            Kind::Steps(fields) => {
                let Value::Array(steps) = value else { continue };
                for step in steps {
                    let Value::Table(step) = step else { continue };
                    let name = step.get("name").and_then(Value::as_str).unwrap_or_default();
                    blank(&mut out);
                    let _ = writeln!(out, "[[{}]]", field.key);
                    for key in ordered_keys(step, fields) {
                        let _ = writeln!(
                            out,
                            "{} = {}{}",
                            key_text(&key),
                            value_text(&step[&key]),
                            comment(&step_path(field.key, name, &key))
                        );
                    }
                }
            }
            _ => {
                let _ = writeln!(
                    out,
                    "{} = {}{}",
                    field.key,
                    value_text(value),
                    comment(field.key)
                );
            }
        }
    }
    out
}

fn section_text(
    out: &mut String,
    name: &str,
    section: &Table,
    fields: &[Field],
    comment: &dyn Fn(&str) -> String,
) {
    let plain: Vec<String> = ordered_keys(section, fields)
        .into_iter()
        .filter(|key| !section[key].is_table())
        .collect();
    let maps: Vec<String> = ordered_keys(section, fields)
        .into_iter()
        .filter(|key| section[key].is_table())
        .collect();
    if !plain.is_empty() || maps.is_empty() {
        blank(out);
        let _ = writeln!(out, "[{name}]");
        for key in &plain {
            let _ = writeln!(
                out,
                "{} = {}{}",
                key_text(key),
                value_text(&section[key]),
                comment(&join(name, key))
            );
        }
    }
    for key in maps {
        let Value::Table(map) = &section[&key] else {
            continue;
        };
        if map.is_empty() {
            continue;
        }
        let path = join(name, &key);
        blank(out);
        let _ = writeln!(out, "[{path}]");
        for (item, value) in map {
            let _ = writeln!(
                out,
                "{} = {}{}",
                key_text(item),
                value_text(value),
                comment(&join(&path, item))
            );
        }
    }
}

/// The keys of `table` in schema order, then any others.
fn ordered_keys(table: &Table, fields: &[Field]) -> Vec<String> {
    let mut keys: Vec<String> = fields
        .iter()
        .filter(|field| table.contains_key(field.key))
        .map(|field| field.key.to_string())
        .collect();
    for key in table.keys() {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys
}

fn blank(out: &mut String) {
    if !out.is_empty() {
        out.push('\n');
    }
}

fn key_text(key: &str) -> String {
    if !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        key.to_string()
    } else {
        Value::String(key.to_string()).to_string()
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) if text.contains('\n') => multiline(text),
        other => other.to_string(),
    }
}

/// A multi-line string as a literal block when it can be one, else a basic
/// string with escapes.
fn multiline(text: &str) -> String {
    let literal_ok = !text.contains("'''")
        && !text.ends_with('\'')
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t');
    if literal_ok {
        format!("'''\n{text}'''")
    } else {
        Value::String(text.to_string()).to_string()
    }
}
