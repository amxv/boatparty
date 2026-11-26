//! The shape of `boatparty.toml` and of preset files, checked strictly: an
//! unknown key or a value of the wrong type is an error that names its path
//! (for example `sync.histroy` or `ensure[2].kye_files`).

use toml::{Table, Value};

/// What a key holds.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Int,
    Bool,
    Str,
    StrList,
    /// A table of string values with free-form keys (`[run.env]`, `[commands]`).
    StrMap,
    Section(&'static [Field]),
    /// An array of tables merged by `name` (`[[setup]]`, `[[ensure]]`).
    Steps(&'static [Field]),
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub kind: Kind,
}

const fn field(key: &'static str, kind: Kind) -> Field {
    Field { key, kind }
}

pub const PROJECT: &[Field] = &[field("name", Kind::Str), field("presets", Kind::StrList)];

pub const SANDBOX: &[Field] = &[
    field("machine", Kind::Str),
    field("desktop", Kind::Str),
    field("remote_path", Kind::Str),
    field("environment", Kind::Str),
];

pub const SYNC: &[Field] = &[
    field("history", Kind::Str),
    field("copy_back", Kind::Bool),
    field("seed", Kind::Bool),
    field("github_token", Kind::Bool),
];

pub const RUN: &[Field] = &[
    field("ram_tmp", Kind::Bool),
    field("env", Kind::StrMap),
    field("script", Kind::Str),
    field("git_identity", Kind::Bool),
    field("github_token", Kind::Bool),
];

pub const SETUP: &[Field] = &[
    field("name", Kind::Str),
    field("check", Kind::Str),
    field("run", Kind::Str),
    field("enabled", Kind::Bool),
];

pub const ENSURE: &[Field] = &[
    field("name", Kind::Str),
    field("dir", Kind::Str),
    field("key_files", Kind::StrList),
    field("key_command", Kind::Str),
    field("check", Kind::Str),
    field("run", Kind::Str),
    field("enabled", Kind::Bool),
];

pub const BUILD: &[Field] = &[
    field("output_dirs", Kind::StrList),
    field("floor_mtimes", Kind::Bool),
    field("cache_dirs", Kind::StrList),
    field("cache_max_age_days", Kind::Int),
    field("min_free_gb", Kind::Int),
];

pub const BASE: &[Field] = &[
    field("warm", Kind::Str),
    field("stale_inputs", Kind::StrList),
    field("ref", Kind::Str),
];

/// Every top-level key, in the order the configuration is shown.
pub const TOP: &[Field] = &[
    field("version", Kind::Int),
    field("project", Kind::Section(PROJECT)),
    field("sandbox", Kind::Section(SANDBOX)),
    field("sync", Kind::Section(SYNC)),
    field("run", Kind::Section(RUN)),
    field("setup", Kind::Steps(SETUP)),
    field("ensure", Kind::Steps(ENSURE)),
    field("build", Kind::Section(BUILD)),
    field("base", Kind::Section(BASE)),
    field("commands", Kind::StrMap),
];

/// Which file is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Project,
    Preset,
}

/// Check one parsed file against the schema. A project file must carry
/// `version` and `[project]`; a preset must not carry `[project]`.
pub fn check(table: &Table, kind: FileKind) -> Result<(), String> {
    check_fields(table, TOP, "")?;
    match table.get("version") {
        Some(Value::Integer(1)) => {}
        Some(other) => {
            return Err(format!(
                "version {other} is not supported (expected version = 1)"
            ));
        }
        None if kind == FileKind::Project => {
            return Err("missing key version (expected version = 1)".to_string());
        }
        None => {}
    }
    match kind {
        FileKind::Project => {
            let name = table
                .get("project")
                .and_then(Value::as_table)
                .and_then(|project| project.get("name"));
            if name.is_none() {
                return Err("missing key project.name".to_string());
            }
        }
        FileKind::Preset => {
            if table.contains_key("project") {
                return Err(
                    "[project] is not allowed in a preset; a preset has the shape of \
                     boatparty.toml without [project]"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

fn check_fields(table: &Table, fields: &[Field], prefix: &str) -> Result<(), String> {
    for (key, value) in table {
        let path = join(prefix, key);
        let Some(field) = fields.iter().find(|field| field.key == key) else {
            let known: Vec<&str> = fields.iter().map(|field| field.key).collect();
            let place = if prefix.is_empty() {
                "at the top level".to_string()
            } else {
                format!("in {prefix}")
            };
            return Err(format!(
                "unknown key {path} (known keys {place}: {})",
                known.join(", ")
            ));
        };
        check_value(value, field.kind, &path)?;
    }
    Ok(())
}

fn check_value(value: &Value, kind: Kind, path: &str) -> Result<(), String> {
    let wrong = |expected: &str| {
        Err(format!(
            "{path} must be {expected}, not {}",
            describe(value)
        ))
    };
    match kind {
        Kind::Int => match value {
            Value::Integer(_) => Ok(()),
            _ => wrong("an integer"),
        },
        Kind::Bool => match value {
            Value::Boolean(_) => Ok(()),
            _ => wrong("true or false"),
        },
        Kind::Str => match value {
            Value::String(_) => Ok(()),
            _ => wrong("a string"),
        },
        Kind::StrList => match value {
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !item.is_str() {
                        return Err(format!(
                            "{path}[{}] must be a string, not {}",
                            index + 1,
                            describe(item)
                        ));
                    }
                }
                Ok(())
            }
            _ => wrong("a list of strings"),
        },
        Kind::StrMap => match value {
            Value::Table(table) => {
                for (key, item) in table {
                    if !item.is_str() {
                        return Err(format!(
                            "{} must be a string, not {}",
                            join(path, key),
                            describe(item)
                        ));
                    }
                }
                Ok(())
            }
            _ => wrong("a table"),
        },
        Kind::Section(fields) => match value {
            Value::Table(table) => check_fields(table, fields, path),
            _ => wrong(&format!("a table ([{path}])")),
        },
        Kind::Steps(fields) => match value {
            Value::Array(items) => {
                let mut names: Vec<&str> = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    let at = format!("{path}[{}]", index + 1);
                    let Value::Table(step) = item else {
                        return Err(format!(
                            "{at} must be a table ([[{path}]]), not {}",
                            describe(item)
                        ));
                    };
                    check_fields(step, fields, &at)?;
                    let Some(name) = step.get("name").and_then(Value::as_str) else {
                        return Err(format!(
                            "{at} has no name; every step needs one so presets can be merged \
                             and switched off by name"
                        ));
                    };
                    if names.contains(&name) {
                        return Err(format!(
                            "{at}: two {path} steps in one file are named {name:?}"
                        ));
                    }
                    names.push(name);
                }
                Ok(())
            }
            _ => wrong(&format!("an array of tables ([[{path}]])")),
        },
    }
}

/// `a.b`, or `b` at the top.
pub fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Float(_) => "a number",
        Value::Boolean(_) => "a boolean",
        Value::Datetime(_) => "a date",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(text: &str, kind: FileKind) -> Result<(), String> {
        check(&toml::from_str::<Table>(text).unwrap(), kind)
    }

    #[test]
    fn unknown_keys_are_errors_that_name_their_path() {
        let base = "version = 1\n[project]\nname = \"a\"\n";
        for (extra, path) in [
            ("[sync]\nhistroy = \"full\"\n", "unknown key sync.histroy"),
            (
                "[run.env]\nA = \"1\"\n[run]\nram = true\n",
                "unknown key run.ram",
            ),
            (
                "[[ensure]]\nname = \"x\"\nrun = \"y\"\nkye_files = []\n",
                "unknown key ensure[1].kye_files",
            ),
            (
                "[later]\nx = 1\n",
                "unknown key later (known keys at the top level",
            ),
        ] {
            let error = checked(&format!("{base}{extra}"), FileKind::Project).unwrap_err();
            assert!(error.contains(path), "{error}");
        }
        let error = checked(
            "version = 1\n[project]\nname = \"a\"\nextra = 1\n",
            FileKind::Project,
        )
        .unwrap_err();
        assert!(error.contains("unknown key project.extra"), "{error}");
    }

    #[test]
    fn types_and_required_keys_are_checked() {
        let error = checked(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nseed = \"no\"\n",
            FileKind::Project,
        )
        .unwrap_err();
        assert!(error.contains("sync.seed must be true or false"), "{error}");
        let error = checked(
            "version = 1\n[project]\nname = \"a\"\n[run.env]\nN = 1\n",
            FileKind::Project,
        )
        .unwrap_err();
        assert!(error.contains("run.env.N must be a string"), "{error}");
        let error = checked("[project]\nname = \"a\"\n", FileKind::Project).unwrap_err();
        assert!(error.contains("version"), "{error}");
        let error =
            checked("version = 2\n[project]\nname = \"a\"\n", FileKind::Project).unwrap_err();
        assert!(error.contains("version 2"), "{error}");
        let error = checked("[project]\nname = \"a\"\n", FileKind::Preset).unwrap_err();
        assert!(error.contains("not allowed in a preset"), "{error}");
        let error = checked("[[setup]]\nrun = \"x\"\n", FileKind::Preset).unwrap_err();
        assert!(error.contains("setup[1] has no name"), "{error}");
        let error = checked(
            "[[setup]]\nname = \"x\"\nrun = \"a\"\n[[setup]]\nname = \"x\"\nrun = \"b\"\n",
            FileKind::Preset,
        )
        .unwrap_err();
        assert!(error.contains("named \"x\""), "{error}");
        assert!(checked("[build]\noutput_dirs = [\"target\"]\n", FileKind::Preset).is_ok());
    }
}
