//! Build-time credentials for one command: the developer's Git identity
//! and, when the project asks for it, a GitHub token.
//!
//! Both travel at the head of the runner's stdin in `run` mode, never in an
//! argument list, and are never written to the sandbox. The runner exports
//! the identity to the command's environment and serves the token to Git's
//! credential helper over a socket that lives exactly as long as the
//! command (see `src/runner/script/credentials.sh`). On this side the token
//! is read only when a command is about to run with it, held in memory and
//! never recorded in state, runs or output.

use std::path::Path;
use std::process::{Command, Stdio};

use super::seed::{Secret, TokenSource};

/// What one command gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunCredentials {
    /// `user.name` of the local repository's effective configuration.
    pub git_name: Option<String>,
    /// `user.email` of the local repository's effective configuration.
    pub git_email: Option<String>,
    /// The token for github.com.
    pub github_token: Option<Secret>,
}

impl RunCredentials {
    /// Nothing: the command runs with the sandbox's own settings.
    pub fn none() -> Self {
        Self::default()
    }

    /// The identity of the repository at `root` (when `identity`) and the
    /// token from `token` (fetched now, only when it is a real source).
    pub fn gather(root: &Path, identity: bool, token: &TokenSource) -> Self {
        let (git_name, git_email) = if identity {
            (
                config_value(root, "user.name"),
                config_value(root, "user.email"),
            )
        } else {
            (None, None)
        };
        Self {
            git_name,
            git_email,
            github_token: token.fetch().filter(|token| usable(token.expose())),
        }
    }

    /// Whether an identity is sent.
    pub fn has_identity(&self) -> bool {
        self.git_name.is_some() || self.git_email.is_some()
    }

    /// The block at the head of `run`'s stdin: `key value` lines, then an
    /// empty line. Values holding a control character are not sent.
    pub fn header(&self) -> Vec<u8> {
        let mut text = String::new();
        let mut line = |key: &str, value: Option<&str>| {
            if let Some(value) = value.filter(|value| printable(value)) {
                text.push_str(&format!("{key} {value}\n"));
            }
        };
        line("git-name", self.git_name.as_deref());
        line("git-email", self.git_email.as_deref());
        line(
            "github-token",
            self.github_token
                .as_ref()
                .map(Secret::expose)
                .filter(|token| usable(token)),
        );
        text.push('\n');
        text.into_bytes()
    }
}

/// One value of the repository's configuration files (local, worktree,
/// global, system and their includes), if set and not empty.
///
/// Configuration injected through the environment (`GIT_CONFIG_COUNT`,
/// `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_VALUE_<n>`, `GIT_CONFIG_PARAMETERS`) is
/// ignored: it outranks even the repository's own file, and boatparty's
/// runner sets exactly these for every command. Honoring it would make a
/// boatparty run inside a boatparty command report the outer developer's
/// identity instead of the repository's.
fn config_value(root: &Path, key: &str) -> Option<String> {
    let mut command = Command::new("git");
    command
        .args(["config", "--get", key])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for name in injected_config_vars() {
        command.env_remove(name);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty() && printable(&value)).then_some(value)
}

/// The names of this process's environment variables that inject Git
/// configuration.
fn injected_config_vars() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let name = name.to_string_lossy();
            name == "GIT_CONFIG_COUNT"
                || name == "GIT_CONFIG_PARAMETERS"
                || name.starts_with("GIT_CONFIG_KEY_")
                || name.starts_with("GIT_CONFIG_VALUE_")
        })
        .collect()
}

fn printable(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

/// A token is one word of visible ASCII.
fn usable(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_holds_what_is_set_and_nothing_unsafe() {
        assert_eq!(RunCredentials::none().header(), b"\n");
        let full = RunCredentials {
            git_name: Some("Ada Lovelace".into()),
            git_email: Some("ada@example.com".into()),
            github_token: Some(Secret::new("gho_t0k")),
        };
        assert_eq!(
            String::from_utf8(full.header()).unwrap(),
            "git-name Ada Lovelace\ngit-email ada@example.com\ngithub-token gho_t0k\n\n"
        );
        let odd = RunCredentials {
            git_name: Some("two\nlines".into()),
            git_email: None,
            github_token: Some(Secret::new("has space")),
        };
        assert_eq!(odd.header(), b"\n");
        assert!(!format!("{full:?}").contains("gho_t0k"));
    }
}
