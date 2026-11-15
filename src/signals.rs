//! SIGINT and SIGTERM while a remote command runs.
//!
//! While [`Interrupts`] is alive the signals no longer end the process.
//! The first one asks for a soft cancel (the remote command is stopped and
//! the connection ends cleanly), a second one for a hard cancel. The caller
//! then exits 130 after SIGINT or 143 after SIGTERM.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::error::AppError;
use crate::sync::{CancelProbe, CancelState};

/// Install once and keep until the process exits: removing signal-hook's
/// handlers does not restore the default action.
pub struct Interrupts {
    /// Low byte: the exit status of the first signal; above it: the count.
    seen: Arc<AtomicU32>,
}

impl Interrupts {
    pub fn install() -> Result<Self, AppError> {
        let mut signals = Signals::new([SIGINT, SIGTERM])
            .map_err(|error| AppError::io("cannot handle SIGINT and SIGTERM", error))?;
        let seen = Arc::new(AtomicU32::new(0));
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for signal in signals.forever() {
                let status = if signal == SIGINT { 130 } else { 143 };
                // Only this thread writes, so a plain load and store suffice.
                let value = recorded.load(Ordering::SeqCst);
                let first = if value & 0xff == 0 {
                    status
                } else {
                    value & 0xff
                };
                recorded.store((((value >> 8) + 1) << 8) | first, Ordering::SeqCst);
            }
        });
        Ok(Self { seen })
    }

    /// The exit status the process should end with, once a signal arrived.
    pub fn exit_status(&self) -> Option<u8> {
        let first = self.seen.load(Ordering::SeqCst) & 0xff;
        (first != 0).then(|| u8::try_from(first).unwrap_or(130))
    }

    fn count(&self) -> u32 {
        self.seen.load(Ordering::SeqCst) >> 8
    }
}

impl CancelProbe for Interrupts {
    fn state(&self) -> CancelState {
        match self.count() {
            0 => CancelState::None,
            1 => CancelState::Soft,
            _ => CancelState::Hard,
        }
    }
}
