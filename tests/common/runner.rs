//! The runner as a local child against scratch repositories, for tests of
//! the mirror and of commands, without SSH.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use boatparty::runner::RunnerHost;

pub const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/runner/runner.sh");

pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The runner as a local child, with its checkout and state directories
/// overridden. Until `install` is called it can pretend to be missing. With
/// an upload threshold set it offers an upload route that copies packs into
/// the state directory, as the provider's signed upload does on a sandbox.
pub struct LocalRunner {
    pub repo: PathBuf,
    pub state: PathBuf,
    pub missing: Cell<bool>,
    pub installs: Cell<u32>,
    pub upload_threshold: Cell<Option<u64>>,
    pub uploads: Cell<u32>,
    pub fail_uploads: Cell<bool>,
    /// Extra environment for every runner process.
    pub env: RefCell<Vec<(String, String)>>,
    /// The arguments of every runner process started.
    pub spawned: RefCell<Vec<Vec<String>>>,
}

impl LocalRunner {
    pub fn new(root: &Path, name: &str) -> Self {
        Self {
            repo: root.join(name),
            state: root.join(format!("{name}-state")),
            missing: Cell::new(false),
            installs: Cell::new(0),
            upload_threshold: Cell::new(None),
            uploads: Cell::new(0),
            fail_uploads: Cell::new(false),
            env: RefCell::new(Vec::new()),
            spawned: RefCell::new(Vec::new()),
        }
    }
}

impl RunnerHost for LocalRunner {
    fn command(&self, args: &[String]) -> Command {
        if self.missing.get() {
            let mut command = Command::new("sh");
            command.args(["-c", "echo boatparty-runner-missing >&2; exit 96"]);
            return command;
        }
        self.spawned.borrow_mut().push(args.to_vec());
        let mut command = Command::new("bash");
        command
            .arg(SCRIPT)
            .args(args)
            .env("BOATPARTY_RUNNER_REPO", &self.repo)
            .env("BOATPARTY_RUNNER_STATE", &self.state)
            .env("BOATPARTY_RUNNER_SEED_FILE", "1");
        for (name, value) in self.env.borrow().iter() {
            command.env(name, value);
        }
        command
    }

    fn install(&self) -> Result<(), boatparty::error::AppError> {
        self.missing.set(false);
        self.installs.set(self.installs.get() + 1);
        Ok(())
    }

    fn upload_threshold(&self) -> Option<u64> {
        self.upload_threshold.get()
    }

    fn upload(&self, name: &str, file: &Path) -> Result<(), boatparty::error::AppError> {
        if self.fail_uploads.get() {
            return Err(boatparty::error::AppError::new(
                "connection",
                "the signed upload failed: connection failed",
            ));
        }
        self.uploads.set(self.uploads.get() + 1);
        let dir = self.state.join("in");
        fs::create_dir_all(&dir).unwrap();
        fs::copy(file, dir.join(name)).unwrap();
        Ok(())
    }
}

/// A local repository with one commit, `origin/main`, and an ignored file.
pub fn base_repo(root: &Path) -> PathBuf {
    let local = root.join("local");
    fs::create_dir_all(&local).unwrap();
    git(&local, &["init", "-q", "-b", "main"]);
    write(&local.join("a.txt"), "one\n");
    write(&local.join("dir/b.txt"), "two\n");
    write(&local.join(".gitignore"), "ignored/\n");
    git(&local, &["add", "-A"]);
    git(&local, &["commit", "-qm", "base"]);
    git(&local, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    write(&local.join("ignored/big.bin"), "not synced\n");
    fs::canonicalize(local).unwrap()
}
