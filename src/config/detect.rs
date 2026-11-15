//! Stack detection for `boatparty init`: marker files at the workspace root
//! choose the presets and a guessed test command. It runs once, when the
//! file is written; nothing is detected at run time.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::project::PROJECT_FILE_VERSION;

/// What `init` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detected {
    /// Built-in preset names, in the order they apply.
    pub presets: Vec<String>,
    /// `[commands]` entries, in order.
    pub commands: Vec<(String, String)>,
    /// The marker files that decided it.
    pub markers: Vec<String>,
}

pub fn detect(root: &Path) -> Detected {
    let has = |name: &str| root.join(name).is_file();
    let mut found = Detected::default();
    let add = |found: &mut Detected, preset: &str, markers: &[&str], test: Option<String>| {
        found.presets.push(preset.to_string());
        found
            .markers
            .extend(markers.iter().filter(|m| has(m)).map(|m| m.to_string()));
        if let Some(test) = test {
            let name = if found.commands.is_empty() {
                "test".to_string()
            } else {
                format!("test-{preset}")
            };
            found.commands.push((name, test));
        }
    };

    if has("Cargo.toml") {
        let test = if has("Cargo.lock") {
            "cargo test --locked"
        } else {
            "cargo test"
        };
        add(
            &mut found,
            "rust",
            &["Cargo.toml", "Cargo.lock"],
            Some(test.to_string()),
        );
    }
    if has("package.json") {
        let package = fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        let manager = package
            .get("packageManager")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let has_test = package
            .get("scripts")
            .and_then(|scripts| scripts.get("test"))
            .is_some();
        let (preset, markers, test): (&str, &[&str], &str) =
            if has("pnpm-lock.yaml") || manager.starts_with("pnpm@") {
                ("pnpm", &["package.json", "pnpm-lock.yaml"], "pnpm test")
            } else if has("bun.lock") || has("bun.lockb") || manager.starts_with("bun@") {
                (
                    "bun",
                    &["package.json", "bun.lock", "bun.lockb"],
                    "bun run test",
                )
            } else {
                ("node", &["package.json", "package-lock.json"], "npm test")
            };
        add(
            &mut found,
            preset,
            markers,
            has_test.then(|| test.to_string()),
        );
    }
    let python = ["pyproject.toml", "requirements.txt", "setup.py", "uv.lock"];
    if python.iter().any(|marker| has(marker)) {
        add(
            &mut found,
            "python",
            &python,
            Some(".venv/bin/python -m pytest".to_string()),
        );
    }
    if has("go.mod") {
        add(
            &mut found,
            "go",
            &["go.mod", "go.sum"],
            Some("go test ./...".to_string()),
        );
    }
    found
}

/// The `boatparty.toml` that `init` writes.
pub fn render_project_file(name: &str, detected: &Detected) -> String {
    let quoted = |text: &str| toml::Value::String(text.to_string()).to_string();
    let mut out = format!(
        "# boatparty project configuration. Check this file in.\n\
         # The project name names this project's sandbox pool and its limits in\n\
         # the per-user account.toml, so keep it stable.\n\
         # `boatparty config show --resolved --origin` prints the effective\n\
         # configuration and where each value comes from.\n\
         version = {PROJECT_FILE_VERSION}\n\
         \n\
         [project]\n\
         name = {}\n",
        quoted(name)
    );
    let built_in: Vec<&str> = super::presets::BUILT_IN
        .iter()
        .map(|(name, _)| *name)
        .collect();
    if detected.presets.is_empty() {
        out.push_str(&format!(
            "# No stack was detected. Built-in presets: {}.\n\
             # `boatparty preset show <name>` prints one.\n",
            built_in.join(", ")
        ));
    } else {
        out.push_str(&format!(
            "# Chosen by `boatparty init` from {}.\n\
             # Presets apply in this order and this file overrides them;\n\
             # `boatparty preset show <name>` prints one.\n",
            detected.markers.join(", ")
        ));
    }
    let list: Vec<String> = detected.presets.iter().map(|p| quoted(p)).collect();
    out.push_str(&format!("presets = [{}]\n", list.join(", ")));
    if !detected.commands.is_empty() {
        out.push_str(
            "\n[commands]\n\
             # `boatparty cheers <name> [args...]` runs one of these lines in the\n\
             # sandbox's checkout with the arguments appended.\n",
        );
        for (name, line) in &detected.commands {
            out.push_str(&format!("{name} = {}\n", quoted(line)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("boatparty-detect-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (name, text) in files {
            fs::write(root.join(name), text).unwrap();
        }
        root
    }

    #[test]
    fn package_managers_are_told_apart() {
        let pnpm = dir(
            "pnpm",
            &[
                ("package.json", r#"{"scripts":{"test":"vitest"}}"#),
                ("pnpm-lock.yaml", ""),
            ],
        );
        let found = detect(&pnpm);
        assert_eq!(found.presets, ["pnpm"]);
        assert_eq!(
            found.commands,
            [("test".to_string(), "pnpm test".to_string())]
        );
        let bun = dir(
            "bun",
            &[(
                "package.json",
                r#"{"packageManager":"bun@1.3.1","scripts":{"test":"x"}}"#,
            )],
        );
        assert_eq!(detect(&bun).presets, ["bun"]);
        assert_eq!(detect(&bun).commands[0].1, "bun run test");
        let untested = dir("untested", &[("package.json", "{}")]);
        let found = detect(&untested);
        assert_eq!(found.presets, ["node"]);
        assert!(found.commands.is_empty());
        for root in [pnpm, bun, untested] {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn several_stacks_get_one_test_each_and_nothing_found_writes_an_empty_list() {
        let both = dir("both", &[("Cargo.toml", ""), ("go.mod", "")]);
        let found = detect(&both);
        assert_eq!(found.presets, ["rust", "go"]);
        assert_eq!(found.commands[0], ("test".into(), "cargo test".into()));
        assert_eq!(
            found.commands[1],
            ("test-go".into(), "go test ./...".into())
        );
        let text = render_project_file("app", &found);
        assert!(text.contains("presets = [\"rust\", \"go\"]"), "{text}");
        assert!(text.contains("test-go = \"go test ./...\""), "{text}");
        let none = dir("none", &[]);
        let text = render_project_file("app", &detect(&none));
        assert!(text.contains("presets = []\n"), "{text}");
        assert!(!text.contains("[commands]"), "{text}");
        for root in [both, none] {
            let _ = fs::remove_dir_all(root);
        }
    }
}
