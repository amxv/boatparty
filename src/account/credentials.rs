//! The stored provider credential.
//!
//! The key lives in `credentials.toml` with mode 0600, or comes from
//! `BOATPARTY_BOAT_API_KEY`, which overrides the file. It can be stored only
//! from standard input (`boatparty login --key-stdin`), never from an
//! argument. [`ApiKey`] never prints its value.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::paths::Paths;
use crate::provider;

pub const KEY_ENV: &str = "BOATPARTY_BOAT_API_KEY";

/// A provider API key. Its `Debug` output is redacted and it has no
/// `Display`, so it cannot be formatted into a message by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Validate and wrap a key. Errors never contain the key.
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        let key = raw.trim();
        provider::check_key_format(key)
            .map_err(|reason| AppError::credentials(format!("not a valid API key: {reason}")))?;
        Ok(Self(key.to_string()))
    }

    /// The secret itself, for the provider's HTTP client only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Where the active key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Environment,
    File(PathBuf),
}

impl KeySource {
    pub fn describe(&self) -> String {
        match self {
            Self::Environment => format!("the {KEY_ENV} environment variable"),
            Self::File(path) => path.display().to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsFile {
    provider: ProviderCredential,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderCredential {
    api_key: String,
}

impl fmt::Debug for ProviderCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderCredential { api_key: <redacted> }")
    }
}

/// The active key: the environment variable when set, else the stored file.
pub fn load(paths: &Paths) -> Result<Option<(ApiKey, KeySource)>, AppError> {
    if let Some(value) = std::env::var_os(KEY_ENV).filter(|value| !value.is_empty()) {
        let value = value
            .into_string()
            .map_err(|_| AppError::credentials(format!("{KEY_ENV} is not valid UTF-8")))?;
        let key = ApiKey::parse(&value)
            .map_err(|error| AppError::credentials(format!("{KEY_ENV}: {}", error.message())))?;
        return Ok(Some((key, KeySource::Environment)));
    }
    let path = paths.credentials_file();
    let Some(key) = read_file(&path)? else {
        return Ok(None);
    };
    Ok(Some((key, KeySource::File(path))))
}

fn read_file(path: &Path) -> Result<Option<ApiKey>, AppError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::io(
                format!("cannot read {}", path.display()),
                error,
            ));
        }
    };
    // The parser's own message can quote the file, so it is not shown.
    let file: CredentialsFile = toml::from_str(&text).map_err(|_| {
        AppError::credentials(format!(
            "cannot parse {}; store the key again with `boatparty login --key-stdin`",
            path.display()
        ))
    })?;
    ApiKey::parse(&file.provider.api_key)
        .map(Some)
        .map_err(|error| AppError::credentials(format!("{}: {}", path.display(), error.message())))
}

/// Store the key with mode 0600, replacing any previous one atomically.
pub fn store(paths: &Paths, key: &ApiKey) -> Result<PathBuf, AppError> {
    let path = paths.credentials_file();
    let dir = path
        .parent()
        .expect("the credentials file always has a parent directory");
    create_private_dir(dir)?;

    let body = toml::to_string(&CredentialsFile {
        provider: ProviderCredential {
            api_key: key.expose().to_string(),
        },
    })
    .map_err(|_| AppError::credentials("cannot encode the credentials file"))?;
    let contents = format!(
        "# boatparty credentials. Written by `boatparty login --key-stdin`.\n\
         # Keep this file private (mode 0600). {KEY_ENV} overrides it.\n{body}"
    );

    let temp = dir.join(format!(".credentials.toml.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temp, &path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(AppError::io(
            format!("cannot write {}", path.display()),
            error,
        ));
    }
    Ok(path)
}

/// The permission bits of a file, if it exists.
pub fn file_mode(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777)
}

fn create_private_dir(dir: &Path) -> Result<(), AppError> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn temp_paths(name: &str) -> (PathBuf, Paths) {
        let root =
            std::env::temp_dir().join(format!("boatparty-cred-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = OsString::from(root.as_os_str());
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        (root, paths)
    }

    #[test]
    fn stored_key_file_has_mode_0600_and_round_trips() {
        let (root, paths) = temp_paths("roundtrip");
        let key = ApiKey::parse("boat_test_secret_123").unwrap();
        let path = store(&paths, &key).unwrap();
        assert_eq!(file_mode(&path), Some(0o600));
        assert_eq!(file_mode(path.parent().unwrap()), Some(0o700));
        assert_eq!(read_file(&path).unwrap(), Some(key.clone()));

        // Storing again replaces the key and keeps the mode.
        let other = ApiKey::parse("boat_other_456").unwrap();
        store(&paths, &other).unwrap();
        assert_eq!(file_mode(&path), Some(0o600));
        assert_eq!(read_file(&path).unwrap(), Some(other));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn debug_output_is_redacted() {
        let key = ApiKey::parse("boat_should_not_appear").unwrap();
        let shown = format!("{key:?}");
        assert!(!shown.contains("should_not_appear"));
        let file = CredentialsFile {
            provider: ProviderCredential {
                api_key: "boat_should_not_appear".into(),
            },
        };
        assert!(!format!("{file:?}").contains("should_not_appear"));
    }

    #[test]
    fn parse_errors_do_not_echo_the_file() {
        let (root, paths) = temp_paths("garbled");
        let path = paths.credentials_file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "api_key = boat_leaky_value_here\n").unwrap();
        let error = read_file(&path).unwrap_err();
        assert!(!error.message().contains("leaky_value"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_key_errors_do_not_echo_the_key() {
        let error = ApiKey::parse("not-a-key secret-part").unwrap_err();
        assert!(!error.message().contains("secret-part"));
    }
}
