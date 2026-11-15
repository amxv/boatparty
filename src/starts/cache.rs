//! `limits_cache` in `state.json`: the provider's start figures as of one
//! fetch, so admission needs no provider call while the figures are fresh.

use serde::{Deserialize, Serialize};

use crate::provider::{AccountLimits, StartWindow, WindowUsage};
use crate::runs::parse_rfc3339;
use crate::state::State;

/// One window's figures at fetch time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedWindow {
    pub limit: u64,
    pub used: u64,
    pub remaining: u64,
}

/// A window the provider reported full, and when it frees (ms).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFull {
    pub window: String,
    #[serde(default)]
    pub frees_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsCache {
    /// When the request for these figures was sent (ms). A start whose
    /// answer arrived before this is counted in them.
    pub fetched_at: u64,
    pub unlimited: bool,
    #[serde(default)]
    pub minute: Option<CachedWindow>,
    #[serde(default)]
    pub hour: Option<CachedWindow>,
    #[serde(default)]
    pub day: Option<CachedWindow>,
    /// Sandboxes running across the account.
    #[serde(default)]
    pub running: u64,
    /// The provider's limit on sandboxes running at once.
    #[serde(default)]
    pub max_running: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_full: Option<CachedFull>,
    /// The provider bills starts past a full window instead of refusing them.
    #[serde(default)]
    pub extra_starts: bool,
    /// What the next start costs, in thousandths of a dollar, when the
    /// provider said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_start_mills: Option<u64>,
}

fn cached(window: Option<WindowUsage>) -> Option<CachedWindow> {
    window.map(|window| CachedWindow {
        limit: window.limit,
        used: window.used,
        remaining: window.remaining,
    })
}

impl LimitsCache {
    pub fn from_limits(limits: &AccountLimits, fetched_at: u64) -> Self {
        Self {
            fetched_at,
            unlimited: limits.starts.unlimited
                && limits.starts.minute.is_none()
                && limits.starts.hour.is_none()
                && limits.starts.day.is_none(),
            minute: cached(limits.starts.minute),
            hour: cached(limits.starts.hour),
            day: cached(limits.starts.day),
            running: limits.running_sandboxes,
            max_running: limits.max_running_sandboxes,
            window_full: limits.window_full.as_ref().map(|full| CachedFull {
                window: full.window.as_str().to_string(),
                frees_at: full.frees_at.as_deref().and_then(parse_rfc3339),
            }),
            extra_starts: limits.extra_starts,
            next_start_mills: limits
                .next_start_dollars
                .filter(|dollars| dollars.is_finite() && *dollars >= 0.0)
                .map(|dollars| (dollars * 1000.0).round() as u64),
        }
    }

    /// The cache in `state`, if there is a readable one.
    pub fn read(state: &State) -> Option<Self> {
        serde_json::from_value(state.limits_cache.clone()?).ok()
    }

    pub fn write(&self, state: &mut State) {
        state.limits_cache = serde_json::to_value(self).ok();
    }

    pub fn window(&self, window: StartWindow) -> Option<CachedWindow> {
        match window {
            StartWindow::Minute => self.minute,
            StartWindow::Hour => self.hour,
            StartWindow::Day => self.day,
        }
    }

    /// When the provider said `window` frees up, if it is the full one.
    pub fn frees_at(&self, window: StartWindow) -> Option<u64> {
        self.window_full
            .as_ref()
            .filter(|full| full.window == window.as_str())
            .and_then(|full| full.frees_at)
    }
}
