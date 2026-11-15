//! `boatparty limits set`: change limits in `account.toml` in place,
//! keeping every comment, blank line and the order of the file. The edited
//! text is validated as a whole before it replaces the file.

use std::fs;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

use super::config::{AccountConfig, load_or_create};
use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::MachineSize;

/// What a `limits set` changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// `[account]` and `[defaults]`.
    Account,
    /// `[projects.<name>]`.
    Project(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A whole number of at least `min`.
    Count { min: u32 },
    /// A machine size.
    Size,
}

/// The keys `limits set account` changes, with their table.
const ACCOUNT_KEYS: &[(&str, &str, Kind)] = &[
    ("account", "max_running", Kind::Count { min: 1 }),
    ("account", "reserve_starts_per_hour", Kind::Count { min: 0 }),
    ("account", "reserve_starts_per_day", Kind::Count { min: 0 }),
    ("defaults", "machine", Kind::Size),
    ("defaults", "max_sandboxes", Kind::Count { min: 1 }),
    ("defaults", "steal_after_minutes", Kind::Count { min: 0 }),
    ("defaults", "lease_minutes", Kind::Count { min: 1 }),
    ("defaults", "idle_minutes", Kind::Count { min: 1 }),
    ("defaults", "heartbeat_minutes", Kind::Count { min: 1 }),
    ("defaults", "acquire_wait_seconds", Kind::Count { min: 0 }),
];

/// The keys `limits set project <name>` changes.
const PROJECT_KEYS: &[(&str, Kind)] = &[
    ("max_sandboxes", Kind::Count { min: 1 }),
    ("max_running", Kind::Count { min: 1 }),
    ("max_machine", Kind::Size),
    ("starts_per_hour", Kind::Count { min: 1 }),
    ("starts_per_day", Kind::Count { min: 1 }),
    ("idle_minutes", Kind::Count { min: 1 }),
    ("lease_minutes", Kind::Count { min: 1 }),
];

/// One changed key.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The table, for example `account` or `projects.app`.
    pub table: String,
    pub key: String,
    pub old: Option<serde_json::Value>,
    /// `None` when the key was removed (an empty value).
    pub new: Option<serde_json::Value>,
}

/// The keys a target accepts, for help and error messages.
pub fn known_keys(target: Target<'_>) -> Vec<&'static str> {
    match target {
        Target::Account => ACCOUNT_KEYS.iter().map(|(_, key, _)| *key).collect(),
        Target::Project(_) => PROJECT_KEYS.iter().map(|(key, _)| *key).collect(),
    }
}

/// Apply `assignments` (`key=value`; an empty value removes the key) to
/// `account.toml`. Nothing is written unless every assignment is valid and
/// the resulting file passes the same validation as on load.
pub fn set(
    paths: &Paths,
    target: Target<'_>,
    assignments: &[String],
) -> Result<(Vec<Change>, std::path::PathBuf), AppError> {
    if assignments.is_empty() {
        return Err(AppError::new(
            "invalid_limit",
            format!(
                "nothing to set; give one or more KEY=VALUE, with KEY one of: {}",
                known_keys(target).join(", ")
            ),
        ));
    }
    if let Target::Project(name) = target {
        crate::project::validate_name(name).map_err(AppError::config)?;
    }
    let loaded = load_or_create(paths)?;
    let path = loaded.path;
    let text = fs::read_to_string(&path)
        .map_err(|error| AppError::io(format!("cannot read {}", path.display()), error))?;
    let (updated, changes) = apply(&text, target, assignments, &path)?;
    AccountConfig::parse(&updated, &path)?;
    if updated != text {
        write_atomically(&path, &updated)?;
    }
    Ok((changes, path))
}

/// The edit itself, on text: the new text and what changed.
pub fn apply(
    text: &str,
    target: Target<'_>,
    assignments: &[String],
    path: &Path,
) -> Result<(String, Vec<Change>), AppError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|error| AppError::config(format!("{}: {error}", path.display())))?;
    let mut changes = Vec::new();
    for assignment in assignments {
        let Some((key, raw)) = assignment.split_once('=') else {
            return Err(AppError::new(
                "invalid_limit",
                format!("{assignment:?} is not KEY=VALUE"),
            ));
        };
        let key = key.trim();
        let raw = raw.trim();
        let (table_path, kind) = lookup(target, key)?;
        let new = parse_value(key, kind, raw)?;
        let table = table_mut(&mut doc, &table_path, path)?;
        let old = table.get(key).and_then(Item::as_value).map(json_of);
        match &new {
            Some(value) => {
                // Keep a comment written after the old value.
                let decor = table
                    .get(key)
                    .and_then(Item::as_value)
                    .map(|old| old.decor().clone());
                let mut value = value.clone();
                if let Some(decor) = decor {
                    *value.decor_mut() = decor;
                }
                // Replace only the value, so the key keeps the comment above it.
                match table.get_mut(key) {
                    Some(item) => *item = Item::Value(value),
                    None => {
                        table.insert(key, Item::Value(value));
                    }
                }
            }
            None => {
                table.remove(key);
            }
        }
        changes.push(Change {
            table: table_path.join("."),
            key: key.to_string(),
            old,
            new: new.as_ref().map(json_of),
        });
    }
    Ok((doc.to_string(), changes))
}

fn lookup(target: Target<'_>, key: &str) -> Result<(Vec<String>, Kind), AppError> {
    let found = match target {
        Target::Account => ACCOUNT_KEYS
            .iter()
            .find(|(_, name, _)| *name == key)
            .map(|(table, _, kind)| (vec![(*table).to_string()], *kind)),
        Target::Project(project) => PROJECT_KEYS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, kind)| (vec!["projects".to_string(), project.to_string()], *kind)),
    };
    found.ok_or_else(|| {
        let scope = match target {
            Target::Account => "account".to_string(),
            Target::Project(name) => format!("project {name}"),
        };
        AppError::new(
            "invalid_limit",
            format!(
                "{key:?} is not a limit of the {scope}; known keys: {}",
                known_keys(target).join(", ")
            ),
        )
    })
}

fn parse_value(key: &str, kind: Kind, raw: &str) -> Result<Option<Value>, AppError> {
    if raw.is_empty() {
        return Ok(None);
    }
    let raw = raw.trim_matches('"');
    match kind {
        Kind::Count { min } => {
            let value: u32 = raw.parse().map_err(|_| {
                AppError::new(
                    "invalid_limit",
                    format!("{key} must be a whole number, not {raw:?}"),
                )
            })?;
            if value < min {
                return Err(AppError::new(
                    "invalid_limit",
                    format!("{key} must be at least {min}"),
                ));
            }
            Ok(Some(Value::from(i64::from(value))))
        }
        Kind::Size => {
            let size: MachineSize = raw.parse().map_err(|message: String| {
                AppError::new("invalid_limit", format!("{key}: {message}"))
            })?;
            Ok(Some(Value::from(size.as_str())))
        }
    }
}

/// The table at `path`, created (as a header table) when it is missing.
fn table_mut<'a>(
    doc: &'a mut DocumentMut,
    table_path: &[String],
    file: &Path,
) -> Result<&'a mut Table, AppError> {
    let mut table = doc.as_table_mut();
    for (depth, name) in table_path.iter().enumerate() {
        let last = depth + 1 == table_path.len();
        let item = table.entry(name).or_insert_with(|| {
            let mut fresh = Table::new();
            fresh.set_implicit(!last);
            Item::Table(fresh)
        });
        table = item.as_table_mut().ok_or_else(|| {
            AppError::config(format!(
                "{}: {} is not a table; write it as a [{}] section",
                file.display(),
                table_path[..=depth].join("."),
                table_path[..=depth].join(".")
            ))
        })?;
    }
    Ok(table)
}

fn json_of(value: &Value) -> serde_json::Value {
    match value {
        Value::Integer(n) => serde_json::json!(*n.value()),
        Value::String(text) => serde_json::json!(text.value()),
        Value::Boolean(flag) => serde_json::json!(*flag.value()),
        Value::Float(n) => serde_json::json!(*n.value()),
        other => serde_json::json!(other.to_string().trim()),
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<(), AppError> {
    let temporary = path.with_extension(format!("toml.tmp-{}", std::process::id()));
    fs::write(&temporary, text)
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(|error| {
            let _ = fs::remove_file(&temporary);
            AppError::io(format!("cannot write {}", path.display()), error)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::config::DEFAULT_ACCOUNT_TOML;

    fn edit(text: &str, target: Target<'_>, assignments: &[&str]) -> Result<String, AppError> {
        let assignments: Vec<String> = assignments.iter().map(|a| (*a).to_string()).collect();
        let path = Path::new("account.toml");
        let (updated, _) = apply(text, target, &assignments, path)?;
        AccountConfig::parse(&updated, path)?;
        Ok(updated)
    }

    const COMMENTED: &str = "# my notes, keep them\nversion = 1\n\n[defaults]\n\
                             # the lease\nlease_minutes = 4 # short on purpose\n\n\
                             [projects.app] # the app\n# cap it\nmax_sandboxes = 2\n\n\
                             [projects.other]\n";

    #[test]
    fn set_keeps_comments_layout_and_other_tables() {
        let updated = edit(
            COMMENTED,
            Target::Project("app"),
            &["max_sandboxes=3", "starts_per_hour=1"],
        )
        .unwrap();
        assert_eq!(
            updated,
            "# my notes, keep them\nversion = 1\n\n[defaults]\n\
             # the lease\nlease_minutes = 4 # short on purpose\n\n\
             [projects.app] # the app\n# cap it\nmax_sandboxes = 3\nstarts_per_hour = 1\n\n\
             [projects.other]\n"
        );
        let updated = edit(COMMENTED, Target::Account, &["lease_minutes=3"]).unwrap();
        assert!(
            updated.contains("# the lease\nlease_minutes = 3 # short on purpose\n"),
            "{updated}"
        );
        // An empty value removes the key.
        let updated = edit(COMMENTED, Target::Project("app"), &["max_sandboxes="]).unwrap();
        assert!(updated.contains("[projects.app] # the app\n"), "{updated}");
        assert!(!updated.contains("max_sandboxes"), "{updated}");
    }

    #[test]
    fn missing_tables_are_added_and_the_default_file_keeps_its_comments() {
        let updated = edit(
            DEFAULT_ACCOUNT_TOML,
            Target::Account,
            &["max_running=4", "reserve_starts_per_day=10"],
        )
        .unwrap();
        assert!(updated.starts_with("# boatparty account settings"));
        assert!(updated.contains("# max_running = 4\n"), "{updated}");
        assert!(
            updated.contains("reserve_starts_per_day = 10\n"),
            "{updated}"
        );
        let config = AccountConfig::parse(&updated, Path::new("a")).unwrap();
        assert_eq!(config.account.max_running, Some(4));
        assert_eq!(config.account.reserve_starts_per_day, 10);

        let updated = edit("version = 1\n", Target::Project("new"), &["max_running=2"]).unwrap();
        assert!(updated.starts_with("version = 1\n"), "{updated}");
        assert!(
            updated.contains("[projects.new]\nmax_running = 2\n"),
            "{updated}"
        );
        assert!(!updated.contains("[projects]"), "{updated}");
        let updated = edit("version = 1\n", Target::Account, &["max_running=2"]).unwrap();
        assert!(
            updated.contains("[account]\nmax_running = 2\n"),
            "{updated}"
        );
    }

    #[test]
    fn caps_and_bad_values_are_refused() {
        let project = Target::Project("app");
        for bad in [
            "lease_minutes=6",
            "idle_minutes=16",
            "lease_minutes=0",
            "starts_per_day=0",
            "max_machine=huge",
            "max_running=-1",
            "surprise=1",
            "noequals",
        ] {
            assert!(edit(COMMENTED, project, &[bad]).is_err(), "{bad}");
        }
        assert!(edit(COMMENTED, Target::Account, &["lease_minutes=30"]).is_err());
        assert!(edit(COMMENTED, Target::Account, &["idle_minutes=60"]).is_err());
        assert!(edit(COMMENTED, Target::Account, &["max_machine=small"]).is_err());
        let updated = edit(
            COMMENTED,
            project,
            &["lease_minutes=5", "idle_minutes=15", "max_machine=large"],
        )
        .unwrap();
        assert!(updated.contains("max_machine = \"large\""), "{updated}");
    }
}
