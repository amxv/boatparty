//! Output of streaming commands (`run`).
//!
//! Text mode: the remote stdout is written to stdout byte for byte, the
//! remote stderr (minus the runner's frames) to stderr, and the tool's own
//! notes to stderr on a line of their own.
//!
//! `--json` mode: stdout carries newline-delimited JSON frames, in order:
//! `{"type":"stdout","data":...}`, `{"type":"stderr","data":...}` and
//! `{"type":"event","event":...}` as they happen, then exactly one
//! `{"type":"result",...}`. Output that is not valid UTF-8 travels as
//! `"base64"` instead of `"data"`. The tool's own notes stay on stderr.

use std::io::Write;

use serde_json::{Value, json};

use crate::error::AppError;
use crate::sync::Sink;

pub struct Stream {
    json: bool,
    stderr_at_line_start: bool,
    out_carry: Vec<u8>,
    err_carry: Vec<u8>,
    events: Vec<(String, std::time::Instant)>,
}

impl Stream {
    pub fn new(json: bool) -> Self {
        Self {
            json,
            stderr_at_line_start: true,
            out_carry: Vec::new(),
            err_carry: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn is_json(&self) -> bool {
        self.json
    }

    /// A message from the tool itself, on stderr, on a line of its own.
    pub fn note(&mut self, message: impl std::fmt::Display) {
        let mut stderr = std::io::stderr().lock();
        let gap = if self.stderr_at_line_start { "" } else { "\n" };
        let _ = writeln!(stderr, "{gap}boatparty: {message}");
        self.stderr_at_line_start = true;
    }

    pub fn ready(&mut self, urls: Vec<String>) {
        self.events
            .push(("ready".into(), std::time::Instant::now()));
        if self.json {
            frame(&json!({"type":"event","event":"ready","urls":urls}));
        }
    }

    /// Events seen so far, with when they arrived.
    pub fn take_events(&mut self) -> Vec<(String, std::time::Instant)> {
        std::mem::take(&mut self.events)
    }

    /// The final frame (JSON mode only).
    pub fn result(&mut self, mut value: Value) {
        self.flush_carries();
        if self.json {
            value["type"] = json!("result");
            frame(&value);
        }
    }

    /// Report a failure: a note on stderr and, in JSON mode, the result frame.
    pub fn error(&mut self, error: &AppError) {
        self.note(format_args!("error: {error}"));
        if self.json {
            self.flush_carries();
            frame(&json!({
                "type": "result",
                "ok": false,
                "error": { "code": error.code(), "message": error.message() },
            }));
        }
    }

    fn flush_carries(&mut self) {
        for (kind, carry) in [
            ("stdout", &mut self.out_carry),
            ("stderr", &mut self.err_carry),
        ] {
            if !carry.is_empty() {
                frame(&json!({"type": kind, "base64": base64(carry)}));
                carry.clear();
            }
        }
    }
}

impl Sink for Stream {
    fn stdout(&mut self, bytes: &[u8]) {
        if self.json {
            data_frame("stdout", &mut self.out_carry, bytes);
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(bytes);
        let _ = stdout.flush();
    }

    fn stderr(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.json {
            data_frame("stderr", &mut self.err_carry, bytes);
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(bytes);
        let _ = stderr.flush();
        self.stderr_at_line_start = bytes.ends_with(b"\n");
    }

    fn event(&mut self, name: &str) {
        self.events
            .push((name.to_string(), std::time::Instant::now()));
        if self.json {
            frame(&json!({"type": "event", "event": name}));
        }
    }
}

fn frame(value: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

/// Frame `bytes` (after what an earlier chunk left unfinished) as text,
/// holding back an incomplete UTF-8 sequence at the end for the next chunk.
fn data_frame(kind: &str, carry: &mut Vec<u8>, bytes: &[u8]) {
    carry.extend_from_slice(bytes);
    let (text, rest) = split_utf8(carry);
    match text {
        Some(text) if !text.is_empty() => frame(&json!({"type": kind, "data": text})),
        Some(_) => {}
        None => frame(&json!({"type": kind, "base64": base64(&carry[..carry.len() - rest])})),
    }
    let keep = carry.split_off(carry.len() - rest);
    *carry = keep;
}

/// The longest valid UTF-8 prefix and the length of the incomplete tail
/// after it. `None` when the bytes contain an invalid sequence.
fn split_utf8(bytes: &[u8]) -> (Option<String>, usize) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (Some(text.to_string()), 0),
        Err(error) if error.error_len().is_none() => {
            let valid = error.valid_up_to();
            let text = std::str::from_utf8(&bytes[..valid]).unwrap_or_default();
            (Some(text.to_string()), bytes.len() - valid)
        }
        Err(_) => (None, 0),
    }
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe]), "//4=");
    }

    #[test]
    fn a_split_utf8_sequence_is_held_back_and_invalid_bytes_are_flagged() {
        let euro = "€".as_bytes();
        assert_eq!(split_utf8(&euro[..2]), (Some(String::new()), 2));
        assert_eq!(split_utf8(b"ok\xe2\x82"), (Some("ok".into()), 2));
        assert_eq!(split_utf8("a€".as_bytes()), (Some("a€".into()), 0));
        assert_eq!(split_utf8(b"a\xffb"), (None, 0));
    }
}
