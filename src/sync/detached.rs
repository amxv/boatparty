//! A run attachment carries framed copies of two append-only files on stdout.
//! Discard an incomplete transport frame on disconnect. The stderr protocol
//! parser and both committed byte offsets live across SSH connections.
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::exchange::{self, CancelProbe, CancelState, Exchange, PackInput, Sink, Spec};
use crate::error::AppError;
use crate::runner::{Piece, RunnerHost, TrailerFilter};

struct Replay<'a> {
    sink: &'a mut dyn Sink,
    offsets: [u64; 2],
    frame: Vec<u8>,
    stderr: TrailerFilter,
    diagnostic: Vec<u8>,
    armed: bool,
    ended: bool,
    attached: bool,
    invalid: bool,
}

impl Sink for Replay<'_> {
    fn stdout(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        self.frame.extend_from_slice(bytes);
        let mut consumed = 0;
        while self.frame.len() >= consumed + 5 {
            let header = &self.frame[consumed..consumed + 5];
            let kind = usize::from(header[0]);
            let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            if kind > 3 || len > 65536 || (kind >= 2 && len != 0) || self.ended {
                self.invalid = true;
                return;
            }
            let end = consumed + 5 + len;
            if self.frame.len() < end {
                break;
            }
            let payload = &self.frame[consumed + 5..end];
            match kind {
                0 => self.sink.stdout(payload),
                1 => {
                    for piece in self.stderr.push_pieces(payload) {
                        match piece {
                            Piece::Bytes(bytes) => self.sink.stderr(&bytes),
                            Piece::Event(name) => self.sink.event(&name),
                        }
                    }
                }
                2 => self.ended = true,
                _ => self.attached = true,
            }
            if kind < 2 {
                self.offsets[kind] += len as u64;
            }
            consumed = end;
        }
        self.frame.drain(..consumed);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        // Before permission, all diagnostics come from SSH or the sync.
        if !self.armed {
            let space = 8192usize.saturating_sub(self.diagnostic.len());
            self.diagnostic
                .extend_from_slice(&bytes[..space.min(bytes.len())]);
        }
        self.sink.stderr(bytes);
    }

    fn event(&mut self, name: &str) {
        if name == "armed" {
            self.armed = true;
        } else {
            self.sink.event(name);
        }
    }
}

pub(super) fn run(
    host: &dyn RunnerHost,
    spec: Spec<'_>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
    reconnect: &dyn Fn() -> Result<bool, AppError>,
) -> Result<Exchange, AppError> {
    let nonce = spec.nonce;
    let project = spec.args[1].clone();
    let root = spec.root;
    let mut replay = Replay {
        sink,
        offsets: [0, 0],
        frame: Vec::new(),
        stderr: TrailerFilter::new(nonce),
        diagnostic: Vec::new(),
        armed: false,
        ended: false,
        attached: false,
        invalid: false,
    };
    let mut exchange = exchange::run(host, spec, &mut replay, cancel)?;
    if !replay.armed {
        if exchange.status == Some(255) && crate::transport::ssh::mux_failure(&replay.diagnostic) {
            replay.sink.event("mux-refused");
        }
        return Ok(exchange);
    }

    let lost = || {
        AppError::new(
            "run_failed",
            "command lost: the sandbox could not provide the remaining output and exit status; the command was not restarted",
        )
    };
    let mut delay = Duration::from_millis(200);
    let window = Duration::from_secs(600);
    let mut deadline = Instant::now() + window;
    let mut resumed = false;
    let pack_bytes = exchange.pack_bytes;
    loop {
        if replay.invalid {
            return Err(lost());
        }
        if replay.ended {
            if !replay.stderr.complete() && !exchange.cancelled {
                return Err(lost());
            }
            exchange.trailer = replay.stderr.result();
            exchange.pack_bytes = pack_bytes;
            if resumed {
                replay.sink.event("reattached");
            }
            return Ok(exchange);
        }
        if cancel.state() != CancelState::None || exchange.cancelled {
            // The attachment might have died before its in-band cancel arrived.
            let _ = host
                .command(&["cancel-run".into(), project.clone(), nonce.into()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            exchange.cancelled = true;
            return Ok(exchange);
        }
        if exchange.status == Some(98) || Instant::now() >= deadline {
            return Err(lost());
        }
        if !resumed {
            replay
                .sink
                .note("SSH disconnected; reconnecting to the running command");
        }
        resumed = true;
        replay.frame.clear();
        let before = replay.offsets;
        replay.attached = false;
        if reconnect()? {
            exchange = exchange::run(
                host,
                Spec {
                    args: vec![
                        "reattach".into(),
                        project.clone(),
                        nonce.into(),
                        before[0].to_string(),
                        before[1].to_string(),
                    ],
                    nonce,
                    root,
                    header: vec![],
                    pack: PackInput::None,
                    hold_stdin: true,
                    input: None,
                },
                &mut replay,
                cancel,
            )?;
            if exchange.runner_missing {
                // A restored home may lack this script even though /tmp has the job.
                host.install()?;
            }
            if replay.attached {
                deadline = Instant::now() + window;
                delay = Duration::from_millis(200);
            }
            if replay.ended || exchange.status == Some(98) {
                continue;
            }
        }
        let until = Instant::now() + delay;
        while Instant::now() < until && cancel.state() == CancelState::None {
            std::thread::sleep(Duration::from_millis(100));
        }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}
