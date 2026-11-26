//! The `RunnerSpec`: a project's resolved settings in the form the runner
//! reads.
//!
//! It is a bash file of plain assignments (no commands), generated from the
//! effective configuration, uploaded to the sandbox as
//! `/home/user/.boatparty/specs/<content-hash>.sh` and named in every `sync`
//! and `run` invocation. The runner sources it; a spec that is not on the
//! sandbox takes the same exit-96 path as a missing runner.
//!
//! What the runner does with each value:
//! - `bp_env`: `[run.env]` as name, value pairs, exported in this (sorted)
//!   order before steps and the command, each value expanded by bash in the
//!   checkout as inside double quotes; then `BOATPARTY=1`.
//! - `bp_setup_*`: `[[setup]]`, run after a start of the machine (first
//!   command of each boot) unless the step's check succeeds.
//! - `bp_ensure_*`: `[[ensure]]`, run before a command when the key changed
//!   or the check fails. `bp_ensure_keys` holds each step's key files, one
//!   per line.
//! - `bp_ram_tmp`: a private RAM `TMPDIR` for the command.
//! - `bp_floor`, `bp_output_dirs`, `bp_cache_dirs`: mtime flooring of build
//!   outputs after a command, and the cache directories it skips (and drops
//!   when a restore left them unreadable).

use crate::config::Config;
use crate::transport::ssh::shell_quote;

use super::{REMOTE_STATE_DIR, content_hash};

/// The runner argument that names no spec.
pub const NO_SPEC: &str = "-";

/// A generated spec and its content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSpec {
    text: String,
    hash: String,
    /// The project's checkout on the sandbox (`[sandbox] remote_path`).
    /// Not part of the text: every invocation names it (see
    /// [`super::remote_invocation_in`]), including those without a spec.
    checkout: Option<String>,
}

impl RunnerSpec {
    /// The spec for a project's effective configuration.
    pub fn new(config: &Config) -> Self {
        let mut out = String::from(
            "# boatparty RunnerSpec, generated from the resolved boatparty.toml.\n\
             # Data only; the runner sources it. Do not edit.\n",
        );
        scalar(&mut out, "bp_ram_tmp", config.run.ram_tmp);
        scalar(&mut out, "bp_floor", config.build.floor_mtimes);
        array(&mut out, "bp_output_dirs", &config.build.output_dirs);
        array(&mut out, "bp_cache_dirs", &config.build.cache_dirs);
        scalar(
            &mut out,
            "bp_desktop_keep",
            config.sandbox.desktop == "keep",
        );
        out.push_str(&format!(
            "bp_cache_days={}\nbp_min_free_gb={}\n",
            config.build.cache_max_age_days, config.build.min_free_gb
        ));
        let env: Vec<String> = config
            .run
            .env
            .iter()
            .flat_map(|(name, value)| [name.clone(), value.clone()])
            .collect();
        array(&mut out, "bp_env", &env);

        let setup = &config.setup;
        array(&mut out, "bp_setup_names", setup.iter().map(|s| &s.name));
        array(
            &mut out,
            "bp_setup_checks",
            setup.iter().map(|s| s.check.clone().unwrap_or_default()),
        );
        array(&mut out, "bp_setup_runs", setup.iter().map(|s| &s.run));

        let ensure = &config.ensure;
        array(&mut out, "bp_ensure_names", ensure.iter().map(|s| &s.name));
        array(
            &mut out,
            "bp_ensure_dirs",
            ensure.iter().map(|s| s.dir.clone().unwrap_or_default()),
        );
        array(
            &mut out,
            "bp_ensure_keys",
            ensure.iter().map(|s| s.key_files.join("\n")),
        );
        array(
            &mut out,
            "bp_ensure_key_commands",
            ensure
                .iter()
                .map(|s| s.key_command.clone().unwrap_or_default()),
        );
        array(
            &mut out,
            "bp_ensure_checks",
            ensure.iter().map(|s| s.check.clone().unwrap_or_default()),
        );
        array(&mut out, "bp_ensure_runs", ensure.iter().map(|s| &s.run));
        let mut spec = Self::from_text(out);
        let checkout = config.sandbox.remote_path.trim_end_matches('/');
        spec.checkout = (!checkout.is_empty()).then(|| checkout.to_string());
        spec
    }

    /// A spec with this exact text and no checkout (tests).
    pub fn from_text(text: String) -> Self {
        let hash = content_hash(text.as_bytes());
        Self {
            text,
            hash,
            checkout: None,
        }
    }

    /// The project's checkout on the sandbox, when known.
    pub fn checkout(&self) -> Option<&str> {
        self.checkout.as_deref()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The spec's name: 16 hex digits of its content hash.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Where this spec lives on a sandbox.
    pub fn remote_path(&self) -> String {
        format!("{REMOTE_STATE_DIR}/specs/{}.sh", self.hash)
    }
}

fn scalar(out: &mut String, name: &str, on: bool) {
    out.push_str(&format!("{name}={}\n", u8::from(on)));
}

fn array<I, S>(out: &mut String, name: &str, items: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let quoted: Vec<String> = items.into_iter().map(|item| quote(item.as_ref())).collect();
    out.push_str(&format!("{name}=({})\n", quoted.join(" ")));
}

/// Always single-quoted, so an empty item stays an item.
fn quote(value: &str) -> String {
    let quoted = shell_quote(value);
    if quoted.starts_with('\'') {
        quoted
    } else {
        format!("'{quoted}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectConfig;
    use std::path::Path;

    fn spec(text: &str) -> RunnerSpec {
        let project = ProjectConfig::parse(text, Path::new("boatparty.toml")).unwrap();
        RunnerSpec::new(project.config())
    }

    #[test]
    fn a_project_without_settings_gets_the_defaults() {
        let spec = spec("version = 1\n[project]\nname = \"app\"\n");
        let text = spec.text();
        assert!(text.contains("bp_ram_tmp=1\n"), "{text}");
        assert!(text.contains("bp_floor=1\n"));
        assert!(text.contains("bp_output_dirs=()\n"));
        assert!(text.contains("bp_env=()\n"));
        assert!(text.contains("bp_ensure_names=()\n"));
        assert_eq!(spec.hash().len(), 16);
        assert_eq!(
            spec.remote_path(),
            format!("/home/user/.boatparty/specs/{}.sh", spec.hash())
        );
    }

    #[test]
    fn steps_env_and_build_settings_are_written_as_quoted_arrays() {
        let spec = spec(
            "version = 1\n[project]\nname = \"app\"\npresets = [\"rust\"]\n\
             [run]\nram_tmp = false\n[run.env]\nGREETING = \"it's $HOME\"\n\
             [[ensure]]\nname = \"deps\"\ndir = \"web\"\nkey_files = [\"a.lock\", \"b.lock\"]\n\
             run = \"install\"\n",
        );
        let text = spec.text();
        assert!(text.contains("bp_ram_tmp=0\n"), "{text}");
        assert!(text.contains("bp_output_dirs=('target')\n"), "{text}");
        assert!(text.contains("bp_cache_dirs=('incremental')\n"), "{text}");
        assert!(text.contains(r#"'GREETING' 'it'\''s $HOME'"#), "{text}");
        assert!(text.contains("'CARGO_HOME' '/usr/local/cargo'"), "{text}");
        assert!(
            text.contains("bp_ensure_names=('rust-toolchain' 'cargo-fetch' 'deps')\n"),
            "{text}"
        );
        assert!(text.contains("bp_ensure_dirs=('' '' 'web')\n"), "{text}");
        assert!(text.contains("'a.lock\nb.lock')\n"), "{text}");
        // The same configuration always gives the same name.
        assert_eq!(spec.hash(), RunnerSpec::from_text(text.to_string()).hash());
    }
}
