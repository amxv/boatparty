//! A command fed this process's input (`boatparty pipe`): no sync, the
//! project's credentials and environment, and the input framed on the
//! runner's stdin (see the runner's "Piped input"), so the end of the input
//! and the death of this process stay distinguishable.

use std::io::Read;

use super::credentials::RunCredentials;
use super::exchange::{self, CancelProbe, PackInput, Sink, Spec};
use super::mirror::RunEnd;
use crate::error::AppError;
use crate::runner::{NO_SPEC, RunnerHost, Trailer};

/// How a piped command went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeOutcome {
    pub end: RunEnd,
    /// Input bytes sent to the command.
    pub input_bytes: u64,
    /// The runner (or the project's spec) was uploaded first.
    pub installed: bool,
    /// A cancel ended the command.
    pub cancelled: bool,
}

/// Run `command` (one shell line) in the checkout with the input `input`
/// makes. `input` is called once per attempt; an attempt that finds the
/// runner missing has read nothing from it, so the retry after installing
/// sends the input whole.
pub fn pipe_command(
    host: &dyn RunnerHost,
    project: &str,
    command: &str,
    credentials: &RunCredentials,
    input: &mut dyn FnMut() -> Box<dyn Read + Send>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<PipeOutcome, AppError> {
    let mut installed = false;
    let root = std::env::temp_dir();
    loop {
        let nonce = crate::system::random_hex(8)?;
        let spec = host.spec_hash().unwrap_or_else(|| NO_SPEC.to_string());
        let args: Vec<String> = ["pipe", project, &nonce, &spec, command]
            .map(String::from)
            .to_vec();
        let exchange = exchange::run(
            host,
            Spec {
                args,
                nonce: &nonce,
                header: credentials.header(),
                pack: PackInput::None,
                root: &root,
                hold_stdin: true,
                input: Some(input()),
            },
            sink,
            cancel,
        )?;
        if exchange.runner_missing && !exchange.cancelled {
            if installed {
                return Err(AppError::new(
                    "run_failed",
                    "the runner or the project's spec is still missing on the sandbox after uploading them",
                ));
            }
            host.install()?;
            installed = true;
            continue;
        }
        let end = match exchange.trailer {
            Some(Trailer::Done {
                exit_code,
                tree,
                patch_bytes,
            }) => RunEnd::Done {
                exit_code,
                tree,
                patch_bytes,
            },
            _ => RunEnd::Lost {
                status: exchange.status,
            },
        };
        return Ok(PipeOutcome {
            end,
            input_bytes: exchange.pack_bytes,
            installed,
            cancelled: exchange.cancelled,
        });
    }
}
