//! Layering: tables merge by key, steps merge by name, every other value is
//! replaced whole. Each value's origin is recorded by its dotted path.

use std::collections::BTreeMap;

use toml::{Table, Value};

use super::schema::join;
use super::{Origin, SwitchedOff};

const STEP_SECTIONS: [&str; 2] = ["setup", "ensure"];

/// The merged table, the origins of its values, and the steps switched off.
pub type Finished = (Table, BTreeMap<String, Origin>, Vec<SwitchedOff>);

#[derive(Debug, Default)]
pub struct Merger {
    merged: Table,
    origins: BTreeMap<String, Origin>,
}

impl Merger {
    /// Lay `layer` over what is merged so far. A preset's `version` is its
    /// own business and is not carried over.
    pub fn apply(&mut self, layer: &Table, origin: &Origin) {
        for (key, value) in layer {
            if key == "version" && matches!(origin, Origin::Preset { .. }) {
                continue;
            }
            if STEP_SECTIONS.contains(&key.as_str()) {
                if let Value::Array(steps) = value {
                    self.steps(key, steps, origin);
                }
                continue;
            }
            merge_value(&mut self.merged, key, value, key, origin, &mut self.origins);
        }
    }

    fn steps(&mut self, section: &str, steps: &[Value], origin: &Origin) {
        let list = self
            .merged
            .entry(section.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !list.is_array() {
            *list = Value::Array(Vec::new());
        }
        let Value::Array(list) = list else {
            return;
        };
        for step in steps {
            let Value::Table(step) = step else { continue };
            let Some(name) = step.get("name").and_then(Value::as_str) else {
                continue;
            };
            let index = list
                .iter()
                .position(|known| known.get("name").and_then(Value::as_str) == Some(name));
            let index = index.unwrap_or_else(|| {
                list.push(Value::Table(Table::new()));
                list.len() - 1
            });
            let Value::Table(target) = &mut list[index] else {
                continue;
            };
            for (key, value) in step {
                target.insert(key.clone(), value.clone());
                if key != "name" || !self.origins.contains_key(&step_path(section, name, key)) {
                    self.origins
                        .insert(step_path(section, name, key), origin.clone());
                }
            }
        }
    }

    /// The merged table without switched-off steps, the origins of what is
    /// left, and the steps that were switched off. A step left on must have
    /// a `run`.
    pub fn finish(mut self) -> Result<Finished, String> {
        let mut switched_off = Vec::new();
        for section in STEP_SECTIONS {
            let Some(Value::Array(list)) = self.merged.get_mut(section) else {
                continue;
            };
            let mut kept = Vec::new();
            for step in list.drain(..) {
                let Value::Table(mut step) = step else {
                    continue;
                };
                let name = step
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let enabled_path = step_path(section, &name, "enabled");
                let enabled = step.remove("enabled").and_then(|value| value.as_bool());
                let by = self.origins.remove(&enabled_path);
                if enabled == Some(false) {
                    let prefix = format!("{section}.{name}.");
                    self.origins.retain(|path, _| !path.starts_with(&prefix));
                    switched_off.push(SwitchedOff {
                        section: section.to_string(),
                        name,
                        by: by.unwrap_or(Origin::File),
                    });
                    continue;
                }
                if !step.contains_key("run") {
                    let from = self
                        .origins
                        .get(&step_path(section, &name, "name"))
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    return Err(format!(
                        "{section} step {name:?} (from {from}) has no run; give it one or \
                         switch it off with enabled = false"
                    ));
                }
                kept.push(Value::Table(step));
            }
            *list = kept;
        }
        Ok((self.merged, self.origins, switched_off))
    }
}

fn merge_value(
    target: &mut Table,
    key: &str,
    value: &Value,
    path: &str,
    origin: &Origin,
    origins: &mut BTreeMap<String, Origin>,
) {
    match value {
        Value::Table(layer) => {
            let entry = target
                .entry(key.to_string())
                .or_insert_with(|| Value::Table(Table::new()));
            if !entry.is_table() {
                *entry = Value::Table(Table::new());
            }
            let Value::Table(inner) = entry else { return };
            for (child, value) in layer {
                merge_value(inner, child, value, &join(path, child), origin, origins);
            }
        }
        other => {
            // `PATH = "/x/bin:$PATH"` over an earlier PATH extends it.
            if path.starts_with("run.env.")
                && let (Value::String(new), Some(Value::String(old))) = (other, target.get(key))
                && let Some(extended) = extend(new, key, old)
            {
                let mut layers = vec![origin.clone()];
                match origins.get(path) {
                    Some(Origin::Several(earlier)) => layers.extend(earlier.iter().cloned()),
                    Some(earlier) => layers.push(earlier.clone()),
                    None => {}
                }
                target.insert(key.to_string(), Value::String(extended));
                origins.insert(path.to_string(), Origin::Several(layers));
                return;
            }
            target.insert(key.to_string(), other.clone());
            origins.insert(path.to_string(), origin.clone());
        }
    }
}

/// `new` with each reference to its own variable (`$NAME` or `${NAME}`)
/// replaced by `old`, or `None` when it has no such reference.
fn extend(new: &str, name: &str, old: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = new;
    let mut found = false;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let braced = format!("{{{name}}}");
        let plain_end = after
            .strip_prefix(name)
            .filter(|tail| !tail.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'));
        if let Some(tail) = after.strip_prefix(braced.as_str()) {
            out.push_str(old);
            rest = tail;
            found = true;
        } else if let Some(tail) = plain_end {
            out.push_str(old);
            rest = tail;
            found = true;
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    found.then_some(out)
}

/// The origin path of one key of a step: `ensure.<name>.<key>`.
pub fn step_path(section: &str, name: &str, key: &str) -> String {
    format!("{section}.{name}.{key}")
}
