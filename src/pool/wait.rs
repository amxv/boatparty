//! Bounded waiting and retrying around provider calls.

use std::time::{Duration, Instant};

use crate::provider::{BusyReason, ErrorKind, ProviderError};

/// Overridable for tests so polling loops finish quickly.
pub const POLL_ENV: &str = "BOATPARTY_POLL_MS";

/// The pause between two polls of the provider.
pub fn poll_interval() -> Duration {
    std::env::var(POLL_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::from_millis(1000), Duration::from_millis)
}

/// The delay before retry number `attempt` (0-based): the provider's own
/// delay when it gave one, else a short exponential backoff, at most 10 s.
pub fn retry_delay(error: &ProviderError, attempt: u32) -> Duration {
    let base = poll_interval();
    let backoff = base.saturating_mul(1 << attempt.min(4));
    error
        .kind()
        .retry_after()
        .unwrap_or(backoff)
        .min(Duration::from_secs(10))
        .max(base.min(Duration::from_millis(100)))
}

/// Retry `op` while `retryable` says so and the deadline allows.
pub fn retry<T>(
    timeout: Duration,
    retryable: impl Fn(&ErrorKind) -> bool,
    mut op: impl FnMut() -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let deadline = Instant::now() + timeout;
    let mut attempt = 0;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(error) if retryable(error.kind()) => {
                let delay = retry_delay(&error, attempt);
                if Instant::now() + delay > deadline {
                    return Err(error);
                }
                std::thread::sleep(delay);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Failures that say nothing about the request itself.
pub fn transient(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::Transport | ErrorKind::Server | ErrorKind::RequestRateLimited { .. }
    )
}

/// Answers from the SSH key endpoint that clear by themselves in the first
/// seconds of a start: restoring, securing, host key not yet available.
pub fn ssh_not_ready(kind: &ErrorKind) -> bool {
    transient(kind)
        || matches!(kind, ErrorKind::NotRunning)
        || matches!(
            kind,
            ErrorKind::Busy {
                reason: BusyReason::Restoring
                    | BusyReason::Securing
                    | BusyReason::HostKeyUnavailable
                    | BusyReason::Starting
                    | BusyReason::Resuming
                    | BusyReason::MachineRetryable,
                ..
            }
        )
}

/// A deadline with a description, for loops that poll a state.
pub struct Deadline {
    end: Instant,
}

impl Deadline {
    pub fn after(timeout: Duration) -> Self {
        Self {
            end: Instant::now() + timeout,
        }
    }

    pub fn passed(&self) -> bool {
        Instant::now() >= self.end
    }

    /// Sleep one poll interval, or report that the deadline passed.
    pub fn tick(&self) -> bool {
        if self.passed() {
            return false;
        }
        std::thread::sleep(poll_interval());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_stops_on_a_permanent_error_and_retries_transient_ones() {
        let mut calls = 0;
        let result: Result<(), ProviderError> = retry(Duration::from_secs(5), transient, || {
            calls += 1;
            Err(ProviderError::new(ErrorKind::NotFound))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);

        let mut calls = 0;
        let result = retry(Duration::from_secs(30), ssh_not_ready, || {
            calls += 1;
            if calls < 3 {
                Err(ProviderError::new(ErrorKind::Busy {
                    reason: BusyReason::HostKeyUnavailable,
                    retry_after: Some(Duration::from_millis(1)),
                }))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.unwrap(), 3);
    }

    #[test]
    fn ssh_readiness_covers_restoring_securing_and_host_key() {
        for reason in [
            BusyReason::Restoring,
            BusyReason::Securing,
            BusyReason::HostKeyUnavailable,
        ] {
            assert!(ssh_not_ready(&ErrorKind::Busy {
                reason,
                retry_after: None
            }));
        }
        assert!(!ssh_not_ready(&ErrorKind::Unauthorized));
        assert!(!ssh_not_ready(&ErrorKind::NotFound));
    }
}
