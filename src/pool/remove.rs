//! Deleting a member's sandbox at the provider. Deletion is asynchronous
//! there, so this waits for the deletion operation to complete.

use std::time::Duration;

use super::wait::{self, Deadline};
use crate::error::AppError;
use crate::output::Output;
use crate::paths::Paths;
use crate::provider::{Deletion, DeletionStatus, ErrorKind, Provider};
use crate::transport::ssh::SshTarget;

const DELETE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Delete sandbox `id` (the member `name`) and wait until the provider's
/// deletion operation completes. `None` when it was already gone. The SSH
/// master is closed and the SSH files are removed either way.
pub fn delete_sandbox(
    paths: &Paths,
    provider: &dyn Provider,
    out: Output,
    name: &str,
    id: &str,
) -> Result<Option<Deletion>, AppError> {
    let target = SshTarget::for_sandbox(paths, id);
    target.close_master();
    let deletion = match wait::retry(Duration::from_secs(60), wait::transient, || {
        provider.delete(id)
    }) {
        Ok(accepted) => {
            out.note(format_args!("deleting {name} ({id})"));
            let deadline = Deadline::after(DELETE_TIMEOUT);
            let mut current = accepted;
            while current.status != DeletionStatus::Completed {
                if !deadline.tick() {
                    return Err(AppError::new(
                        "deletion_timeout",
                        format!(
                            "the provider accepted the deletion of {name} ({id}) but it is still \
                             {} after {} minutes",
                            current.status.as_str(),
                            DELETE_TIMEOUT.as_secs() / 60
                        ),
                    )
                    .with_hint("run the command again later to finish"));
                }
                current = wait::retry(Duration::from_secs(60), wait::transient, || {
                    provider.deletion(&current.operation_id)
                })?;
            }
            Some(current)
        }
        Err(error) if matches!(error.kind(), ErrorKind::NotFound) => {
            out.note(format_args!(
                "{name} ({id}) was already gone at the provider"
            ));
            None
        }
        Err(error) => return Err(error.into()),
    };
    target.remove_files();
    Ok(deletion)
}
