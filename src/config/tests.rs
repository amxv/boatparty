//! Layering, step merging and the error paths of the project configuration.

use std::fs;
use std::path::{Path, PathBuf};

use super::presets::{PresetDirs, Source};
use super::*;

/// A scratch directory with a user presets directory and a project root.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("boatparty-config-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("user")).unwrap();
        fs::create_dir_all(root.join("project/.boatparty/presets")).unwrap();
        Self { root }
    }

    fn dirs(&self) -> PresetDirs {
        PresetDirs::new(self.root.join("user"), Some(&self.root.join("project")))
    }

    fn user(&self, name: &str, text: &str) {
        fs::write(self.root.join("user").join(format!("{name}.toml")), text).unwrap();
    }

    fn project(&self, name: &str, text: &str) {
        fs::write(
            self.root
                .join("project/.boatparty/presets")
                .join(format!("{name}.toml")),
            text,
        )
        .unwrap();
    }

    fn resolve(&self, text: &str) -> Result<Resolved, AppError> {
        resolve(text, Path::new("boatparty.toml"), &self.dirs())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file(presets: &[&str], rest: &str) -> String {
    let list: Vec<String> = presets.iter().map(|p| format!("{p:?}")).collect();
    format!(
        "version = 1\n[project]\nname = \"app\"\npresets = [{}]\n{rest}",
        list.join(", ")
    )
}

fn preset_origin(name: &str, source: Source) -> Origin {
    Origin::Preset {
        name: name.to_string(),
        source,
    }
}

#[test]
fn presets_apply_in_the_listed_order_and_the_project_file_wins() {
    let scratch = Scratch::new("order");
    scratch.user(
        "one",
        "[run]\nscript = \"one run\"\n[run.env]\nA = \"1\"\nB = \"1\"\n[sync]\nhistory = \"full\"\n",
    );
    scratch.project(
        "two",
        "[run]\nscript = \"two run\"\n[run.env]\nB = \"2\"\nC = \"2\"\n",
    );
    let resolved = scratch
        .resolve(&file(&["one", "two"], "[run.env]\nC = \"file\"\n"))
        .unwrap();
    let config = &resolved.config;
    assert_eq!(config.run.script.as_deref(), Some("two run"));
    assert_eq!(config.run.env["A"], "1");
    assert_eq!(config.run.env["B"], "2");
    assert_eq!(config.run.env["C"], "file");
    assert_eq!(config.sync.history, "full");
    // Values nobody set keep boatparty's defaults.
    assert!(config.sync.copy_back && config.run.ram_tmp);
    assert_eq!(config.sandbox.remote_path, "/home/user/app");

    let user = scratch.root.join("user/one.toml");
    let project = scratch.root.join("project/.boatparty/presets/two.toml");
    let origins = &resolved.origins;
    assert_eq!(
        origins["run.env.A"],
        preset_origin("one", Source::User(user))
    );
    assert_eq!(
        origins["run.env.B"],
        preset_origin("two", Source::Project(project))
    );
    assert_eq!(origins["run.env.C"], Origin::File);
    assert_eq!(origins["sync.copy_back"], Origin::Default);
    assert_eq!(origins["project.name"], Origin::File);

    // The other order flips the shared keys.
    let flipped = scratch.resolve(&file(&["two", "one"], "")).unwrap();
    assert_eq!(flipped.config.run.script.as_deref(), Some("one run"));
    assert_eq!(flipped.config.run.env["B"], "1");
    assert_eq!(flipped.config.run.env["C"], "2");
    let names: Vec<&str> = flipped.presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["two", "one"]);
}

#[test]
fn a_more_specific_preset_file_replaces_one_of_the_same_name_entirely() {
    let scratch = Scratch::new("shadow");
    let builtin = scratch.resolve(&file(&["rust"], "")).unwrap();
    assert_eq!(builtin.config.run.env["CARGO_HOME"], "/usr/local/cargo");
    assert_eq!(builtin.presets[0].source, Source::BuiltIn);
    assert!(
        builtin
            .config
            .ensure
            .iter()
            .any(|s| s.name == "cargo-fetch")
    );

    scratch.user("rust", "[run.env]\nCARGO_HOME = \"/home/user/cargo\"\n");
    let user = scratch.resolve(&file(&["rust"], "")).unwrap();
    assert_eq!(user.config.run.env["CARGO_HOME"], "/home/user/cargo");
    assert!(
        user.config.ensure.is_empty(),
        "the built-in is not merged under it"
    );

    scratch.project("rust", "[build]\noutput_dirs = [\"out\"]\n");
    let project = scratch.resolve(&file(&["rust"], "")).unwrap();
    assert!(project.config.run.env.is_empty());
    assert_eq!(project.config.build.output_dirs, ["out"]);
    assert!(matches!(project.presets[0].source, Source::Project(_)));
    assert_eq!(scratch.dirs().candidates("rust").unwrap().len(), 3);
}

#[test]
fn steps_merge_by_name_and_keep_their_place() {
    let scratch = Scratch::new("steps");
    scratch.user(
        "base",
        "[[ensure]]\nname = \"deps\"\nkey_files = [\"a.lock\"]\nrun = \"install\"\n\
         [[ensure]]\nname = \"tools\"\nrun = \"tools\"\n\
         [[setup]]\nname = \"apt\"\ncheck = \"dpkg -s x\"\nrun = \"apt-get install x\"\n",
    );
    let resolved = scratch
        .resolve(&file(
            &["base"],
            "[[ensure]]\nname = \"deps\"\nrun = \"install --frozen\"\n\
             [[ensure]]\nname = \"extra\"\ndir = \"web\"\nrun = \"more\"\n",
        ))
        .unwrap();
    let ensure = &resolved.config.ensure;
    let names: Vec<&str> = ensure.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["deps", "tools", "extra"]);
    assert_eq!(ensure[0].run, "install --frozen");
    assert_eq!(
        ensure[0].key_files,
        ["a.lock"],
        "keys the file did not set stay"
    );
    assert_eq!(ensure[2].dir.as_deref(), Some("web"));
    assert_eq!(resolved.config.setup[0].check.as_deref(), Some("dpkg -s x"));
    assert_eq!(resolved.origins["ensure.deps.run"], Origin::File);
    assert!(matches!(
        resolved.origins["ensure.deps.key_files"],
        Origin::Preset { .. }
    ));
    assert!(matches!(
        resolved.origins["ensure.deps.name"],
        Origin::Preset { .. }
    ));
}

#[test]
fn a_step_can_be_switched_off_by_name_and_back_on() {
    let scratch = Scratch::new("off");
    let resolved = scratch
        .resolve(&file(
            &["rust"],
            "[[ensure]]\nname = \"cargo-fetch\"\nenabled = false\n",
        ))
        .unwrap();
    let names: Vec<&str> = resolved
        .config
        .ensure
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["rust-toolchain"]);
    assert_eq!(resolved.switched_off.len(), 1);
    assert_eq!(resolved.switched_off[0].name, "cargo-fetch");
    assert_eq!(resolved.switched_off[0].by, Origin::File);
    assert!(
        !resolved
            .origins
            .keys()
            .any(|path| path.starts_with("ensure.cargo-fetch.")),
        "{:?}",
        resolved.origins
    );
    let text = render::render(&resolved.merged, Some(&resolved.origins));
    assert!(!text.contains("cargo-fetch"), "{text}");
    assert!(!text.contains("enabled"), "{text}");

    scratch.user(
        "off",
        "[[ensure]]\nname = \"cargo-fetch\"\nenabled = false\n",
    );
    let back_on = scratch
        .resolve(&file(
            &["rust", "off"],
            "[[ensure]]\nname = \"cargo-fetch\"\nenabled = true\n",
        ))
        .unwrap();
    assert_eq!(back_on.config.ensure.len(), 2);
    assert!(back_on.switched_off.is_empty());

    let error = scratch
        .resolve(&file(&[], "[[setup]]\nname = \"x\"\ncheck = \"true\"\n"))
        .unwrap_err();
    assert!(error.message().contains("setup step \"x\""), "{error}");
    assert!(error.message().contains("has no run"), "{error}");
}

#[test]
fn unknown_keys_and_bad_values_name_their_path_and_file() {
    let scratch = Scratch::new("errors");
    let error = scratch
        .resolve(&file(&[], "[build]\noutput_dir = [\"target\"]\n"))
        .unwrap_err();
    assert_eq!(error.code(), "config");
    assert!(error.message().starts_with("boatparty.toml: "), "{error}");
    assert!(
        error.message().contains("unknown key build.output_dir"),
        "{error}"
    );

    scratch.user("bad", "[run]\nscripts = \"x\"\n");
    let error = scratch.resolve(&file(&["bad"], "")).unwrap_err();
    assert!(error.message().contains("preset bad (user: "), "{error}");
    assert!(
        error.message().contains("unknown key run.scripts"),
        "{error}"
    );

    let error = scratch.resolve(&file(&["nope"], "")).unwrap_err();
    assert!(
        error.message().contains("no preset named \"nope\""),
        "{error}"
    );
    assert!(error.message().contains("rust, node, pnpm"), "{error}");

    scratch.user("far", "[sandbox]\nremote_path = \"/tmp/x\"\n");
    let error = scratch.resolve(&file(&["far"], "")).unwrap_err();
    assert!(
        error
            .message()
            .contains("sandbox.remote_path (from preset far (user: "),
        "{error}"
    );
    for (rest, needle) in [
        (
            "[sandbox]\nmachine = \"huge\"\n",
            "sandbox.machine (from boatparty.toml)",
        ),
        ("[run.env]\nBOATPARTY_X = \"1\"\n", "reserved"),
        (
            "[[ensure]]\nname = \"x\"\ndir = \"../up\"\nrun = \"y\"\n",
            "ensure.x.dir",
        ),
        ("[commands]\n\"-x\" = \"y\"\n", "commands.-x"),
        (
            "[sandbox]\nremote_path = \"/home/user/.cache/app\"\n",
            "~/.cache",
        ),
    ] {
        let error = scratch.resolve(&file(&[], rest)).unwrap_err();
        assert!(error.message().contains(needle), "{rest}: {error}");
    }
    let error = scratch.resolve(&file(&["go", "go"], "")).unwrap_err();
    assert!(error.message().contains("twice"), "{error}");
}

#[test]
fn every_built_in_preset_is_valid_on_its_own() {
    for (name, text) in presets::BUILT_IN {
        let preset = presets::PresetFile {
            name: name.to_string(),
            source: Source::BuiltIn,
            text: text.to_string(),
        };
        check_preset(&preset).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!preset.summary().is_empty(), "{name} has no summary line");
        assert!(!text.contains('\u{2014}'), "{name} contains an em dash");
        let settings: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect();
        assert!(
            !settings.iter().any(|line| line.contains("/.cache")),
            "{name} puts something in ~/.cache"
        );
    }
    let scratch = Scratch::new("builtins");
    for (script, name) in [
        ("npm run", "node"),
        ("pnpm run", "pnpm"),
        ("bun run", "bun"),
    ] {
        let resolved = scratch.resolve(&file(&[name], "")).unwrap();
        assert_eq!(resolved.config.run.script.as_deref(), Some(script));
    }
}

#[test]
fn the_rendered_configuration_parses_back_to_the_same_table() {
    let scratch = Scratch::new("render");
    let resolved = scratch
        .resolve(&file(
            &["rust", "bun", "python"],
            "[commands]\ntest = \"cargo test\"\n\"check:rust\" = \"x\"\n",
        ))
        .unwrap();
    for origins in [None, Some(&resolved.origins)] {
        let text = render::render(&resolved.merged, origins);
        let parsed: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(parsed, resolved.merged, "{text}");
    }
    let text = render::render(&resolved.merged, Some(&resolved.origins));
    assert!(
        text.contains("CARGO_HOME = \"/usr/local/cargo\"  # preset rust (built-in)"),
        "{text}"
    );
    // Each preset's PATH extends the one before it.
    assert_eq!(
        resolved.config.run.env["PATH"],
        "$PWD/.venv/bin:/home/user/.bun/bin:/usr/local/cargo/bin:$PATH"
    );
    assert!(
        text.contains(
            "# preset python (built-in), extending preset bun (built-in), extending preset rust \
             (built-in)"
        ),
        "{text}"
    );
    assert!(text.contains("history = \"head\"  # default"), "{text}");
}

#[test]
fn an_env_value_naming_its_own_variable_extends_the_earlier_one() {
    use super::merge::Merger;
    let mut merger = Merger::default();
    let layer = |text: &str| toml::from_str::<toml::Table>(text).unwrap();
    merger.apply(
        &layer("[run.env]\nPATH = \"/a:$PATH\"\nX = \"1\"\n"),
        &Origin::Default,
    );
    merger.apply(
        &layer("[run.env]\nPATH = \"/b:${PATH}:/c\"\nX = \"$XY\"\n"),
        &Origin::File,
    );
    let (merged, origins, _) = merger.finish().unwrap();
    let env = merged["run"]["env"].as_table().unwrap();
    assert_eq!(env["PATH"].as_str(), Some("/b:/a:$PATH:/c"));
    assert_eq!(env["X"].as_str(), Some("$XY"), "$XY is another variable");
    assert_eq!(
        origins["run.env.PATH"],
        Origin::Several(vec![Origin::File, Origin::Default])
    );
    assert_eq!(origins["run.env.X"], Origin::File);
}
