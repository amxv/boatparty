//! Reading and writing `state.json` under an OS advisory lock.
//!
//! The lock is an exclusive `flock` on `state.lock`, held only for one
//! read-modify-write. Callers never make a network call or run a child
//! process inside [`Store::update`]. The OS drops the lock when its holder
//! dies, so there is no stale-lock heuristic. Writes go to a temporary file
//! that is renamed over `state.json`, so a lock-free [`Store::load`] always
//! sees a complete document.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{STATE_VERSION, State};
use crate::error::AppError;
use crate::paths::Paths;

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    file: PathBuf,
    lock: PathBuf,
}

impl Store {
    pub fn new(paths: &Paths) -> Self {
        Self {
            dir: paths.state_dir().to_path_buf(),
            file: paths.state_file(),
            lock: paths.state_lock_file(),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.file
    }

    /// The current state without taking the lock (a consistent snapshot,
    /// possibly stale by the time it is used).
    pub fn load(&self) -> Result<State, AppError> {
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(State::default());
            }
            Err(error) => {
                return Err(AppError::io(
                    format!("cannot read {}", self.file.display()),
                    error,
                ));
            }
        };
        let state: State = serde_json::from_str(&text).map_err(|error| {
            AppError::config(format!("{} is not valid: {error}", self.file.display()))
        })?;
        if state.version != STATE_VERSION {
            return Err(AppError::config(format!(
                "{}: state version {} is not supported (expected {STATE_VERSION})",
                self.file.display(),
                state.version
            )));
        }
        Ok(state)
    }

    /// Run `mutate` on the current state under the lock and persist the
    /// result. Nothing is written when `mutate` fails.
    pub fn update<R>(
        &self,
        mutate: impl FnOnce(&mut State) -> Result<R, AppError>,
    ) -> Result<R, AppError> {
        let _guard = self.lock()?;
        let mut state = self.load()?;
        let result = mutate(&mut state)?;
        self.write(&state)?;
        Ok(result)
    }

    fn ensure_dir(&self) -> Result<(), AppError> {
        if self.dir.is_dir() {
            return Ok(());
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(|error| AppError::io(format!("cannot create {}", self.dir.display()), error))
    }

    fn lock(&self) -> Result<File, AppError> {
        self.ensure_dir()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock)
            .map_err(|error| AppError::io(format!("cannot open {}", self.lock.display()), error))?;
        file.lock()
            .map_err(|error| AppError::io(format!("cannot lock {}", self.lock.display()), error))?;
        // Dropping the file releases the lock.
        Ok(file)
    }

    fn write(&self, state: &State) -> Result<(), AppError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut text = serde_json::to_string_pretty(state)
            .map_err(|error| AppError::config(format!("cannot encode the state: {error}")))?;
        text.push('\n');
        let temp = self.dir.join(format!(
            ".state.json.{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &self.file)
        })();
        result.map_err(|error| {
            let _ = fs::remove_file(&temp);
            AppError::io(format!("cannot write {}", self.file.display()), error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Member, Role, SyncState};
    use std::ffi::OsString;

    fn temp_store(name: &str) -> (PathBuf, Store) {
        let root = std::env::temp_dir().join(format!(
            "boatparty-state-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = OsString::from(root.as_os_str());
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        (root, Store::new(&paths))
    }

    fn member(name: String) -> Member {
        Member {
            id: None,
            create_key: name.clone(),
            name,
            role: Role::Pool,
            affinity: None,
            leases: Vec::new(),
            bringing_up: None,
            sync: SyncState::default(),
            created_at: 0,
            last_used_at: 0,
            last_run: None,
            broken: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn concurrent_writers_lose_no_update() {
        let (root, store) = temp_store("concurrent");
        const WRITERS: usize = 24;
        const ROUNDS: usize = 5;
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let store = store.clone();
                scope.spawn(move || {
                    for round in 0..ROUNDS {
                        store
                            .update(|state| {
                                let project = state.project_mut("app");
                                let name = project.next_member_name("app");
                                project.members.push(member(name));
                                state.starts.push(crate::state::StartRecord {
                                    at: (writer * ROUNDS + round) as u64,
                                    project: "app".into(),
                                    id: None,
                                    pid: None,
                                    done_at: None,
                                });
                                Ok(())
                            })
                            .unwrap();
                    }
                });
            }
        });
        let state = store.load().unwrap();
        let members = &state.projects["app"].members;
        assert_eq!(members.len(), WRITERS * ROUNDS);
        assert_eq!(state.starts.len(), WRITERS * ROUNDS);
        // Every writer saw the previous write: names are unique and dense.
        let mut names: Vec<&str> = members.iter().map(|member| member.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), WRITERS * ROUNDS);
        // No temporary file is left behind.
        let leftovers = fs::read_dir(store.dir.clone())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_mutation_writes_nothing() {
        let (root, store) = temp_store("failed");
        store
            .update(|state| {
                state.project_mut("app");
                Ok(())
            })
            .unwrap();
        let before = fs::read_to_string(store.path()).unwrap();
        let result: Result<(), AppError> = store.update(|state| {
            state.project_mut("other");
            Err(AppError::config("no"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(store.path()).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_file_is_the_empty_state_and_other_versions_are_refused() {
        let (root, store) = temp_store("versions");
        assert_eq!(store.load().unwrap(), State::default());
        fs::create_dir_all(&store.dir).unwrap();
        fs::write(store.path(), r#"{"version": 9}"#).unwrap();
        assert!(store.load().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
