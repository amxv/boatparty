//! Output conventions shared by every command.
//!
//! - Results go to stdout: text, or with `--json` exactly one JSON document.
//! - The tool's own messages go to stderr, prefixed `boatparty: `.
//! - A failure prints `{"ok": false, "error": {"code", "message"}}` on stdout
//!   under `--json`, and always one `boatparty: error: ...` line on stderr.

use std::fmt::Display;
use std::io::Write;

use serde_json::{Value, json};

use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub struct Output {
    json: bool,
}

impl Output {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    pub fn is_json(&self) -> bool {
        self.json
    }

    /// A message from the tool itself, on stderr.
    pub fn note(&self, message: impl Display) {
        let _ = writeln!(std::io::stderr().lock(), "boatparty: {message}");
    }

    /// A command's text result, on stdout.
    pub fn text(&self, text: impl Display) {
        let _ = writeln!(std::io::stdout().lock(), "{text}");
    }

    /// A command's JSON result, on stdout.
    pub fn document(&self, value: &Value) {
        let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
        let _ = writeln!(std::io::stdout().lock(), "{rendered}");
    }

    pub fn error(&self, error: &AppError) {
        if self.json {
            self.document(&error_document(error));
        }
        self.note(format_args!("error: {error}"));
    }
}

pub fn error_document(error: &AppError) -> Value {
    json!({
        "ok": false,
        "error": { "code": error.code(), "message": error.message() },
    })
}
