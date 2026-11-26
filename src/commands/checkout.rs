//! Serialize work on one local workspace from snapshot through copy-back.
//! The OS releases this independent lock when the client dies.
use std::fs::{File, TryLockError};
use std::io::Write;
use std::path::PathBuf;

use crate::{Context, error::AppError, project::Workspace, runner::content_hash, system};

pub struct Checkout {
    pub waited: bool,
    _lock: File,
    owner: PathBuf,
}

impl Checkout {
    pub fn acquire(ctx: &Context, workspace: &Workspace, label: &str) -> Result<Self, AppError> {
        let dir = ctx.paths.state_dir();
        system::private_directory()
            .create(dir)
            .map_err(|e| AppError::io("creating state directory", e))?;
        let prefix = format!("checkout-{}", content_hash(workspace.key().as_bytes()));
        let owner = dir.join(format!("{prefix}.owner"));
        let file = system::private_file()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("{prefix}.lock")))
            .map_err(|e| AppError::io("opening workspace lock", e))?;
        let waited = match file.try_lock() {
            Ok(()) => false,
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&owner)
                    .unwrap_or_else(|_| "another workspace command".into());
                ctx.out.note(format_args!("waiting for {holder}"));
                file.lock()
                    .map_err(|e| AppError::io("waiting for workspace lock", e))?;
                true
            }
            Err(TryLockError::Error(e)) => return Err(AppError::io("locking workspace", e)),
        };
        let guard = Self {
            _lock: file,
            owner,
            waited,
        };
        // This is a display label only. The lock file is never unlinked.
        let mut note = system::private_file()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&guard.owner)
            .map_err(|e| AppError::io("opening workspace owner record", e))?;
        write!(note, "{label} (pid {})", std::process::id())
            .map_err(|e| AppError::io("writing workspace owner", e))?;
        Ok(guard)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.owner);
    }
}
