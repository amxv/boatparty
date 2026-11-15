//! One invocation of the runner: write a header and an optional pack into its
//! stdin, pump its stdout and stderr, and pick out the trailer and events.
//!
//! Both output pipes are read by threads that hand chunks to the calling
//! thread, which does all the writing and watches the child. That way the
//! caller never blocks on a pipe forever: a multiplexed `ssh` client that dies
//! leaves its pipes open through the control master, so once the child has
//! exited the pipes get a short grace period and are then abandoned.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::pack::{self, Revs, TempPack};
use crate::error::AppError;
use crate::runner::{
    EXIT_MISSING, EXIT_NEEDFULL, MISSING_MARKER, Piece, RunnerHost, Trailer, TrailerFilter,
};

/// How long a child that exited may keep its pipes open before they are abandoned.
const PIPE_GRACE: Duration = Duration::from_millis(1500);
/// How long the runner may keep the connection open after its trailer (for
/// example because a background process it left holds the output open).
const AFTER_TRAILER: Duration = Duration::from_secs(5);
/// How long a cancelled command gets to stop and report before the
/// connection is cut.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
const TICK: Duration = Duration::from_millis(100);

/// Where an exchange's output goes.
pub trait Sink {
    /// Bytes the remote command wrote to stdout.
    fn stdout(&mut self, bytes: &[u8]);
    /// Bytes the runner or the command wrote to stderr, minus the frames.
    fn stderr(&mut self, bytes: &[u8]);
    /// An event frame, such as `started`.
    fn event(&mut self, _name: &str) {}
}

/// What a cancel request looks like to an exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelState {
    None,
    /// Close the runner's stdin so it stops the command and reports.
    Soft,
    /// Cut the connection now.
    Hard,
}

/// Asked between chunks whether the caller wants the exchange to end.
pub trait CancelProbe {
    fn state(&self) -> CancelState;
}

/// A probe that never cancels.
pub struct NeverCancel;

impl CancelProbe for NeverCancel {
    fn state(&self) -> CancelState {
        CancelState::None
    }
}

/// One runner invocation to perform.
pub struct Spec<'a> {
    pub args: Vec<String>,
    pub nonce: &'a str,
    /// Written to the runner's stdin before the pack.
    pub header: Vec<u8>,
    /// What follows the header on stdin.
    pub pack: PackInput,
    pub root: &'a Path,
    /// Keep stdin open after the pack until the runner finishes (or a cancel
    /// closes it). The runner treats its end as "stop the command".
    pub hold_stdin: bool,
}

/// The pack that follows the header on the runner's stdin.
#[derive(Debug)]
pub enum PackInput {
    /// No pack.
    None,
    /// Stream `git pack-objects` for these revs as it builds the pack.
    Stream(Revs),
    /// A pack already built in memory.
    Bytes(Vec<u8>),
    /// A pack already built on disk.
    File(TempPack),
}

#[derive(Debug)]
pub struct Exchange {
    /// The child's exit status; `None` when it was killed by a signal.
    pub status: Option<i32>,
    pub trailer: Option<Trailer>,
    pub runner_missing: bool,
    pub pack_bytes: u64,
    /// A cancel closed stdin or cut the connection.
    pub cancelled: bool,
}

enum Chunk {
    Out(Vec<u8>),
    Err(Vec<u8>),
    OutEnd,
    ErrEnd,
}

pub fn run(
    host: &dyn RunnerHost,
    spec: Spec<'_>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<Exchange, AppError> {
    let mut pack = match &spec.pack {
        PackInput::Stream(revs) => Some(pack::spawn(spec.root, revs)?),
        _ => None,
    };
    let mut source: Option<Box<dyn Read + Send>> = match spec.pack {
        PackInput::None | PackInput::Stream(_) => None,
        PackInput::Bytes(bytes) => Some(Box::new(io::Cursor::new(bytes))),
        PackInput::File(file) => Some(Box::new(OpenPack::open(file)?)),
    };
    if let Some(child) = pack.as_mut() {
        source = child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn Read + Send>);
    }
    let mut command = host.command(&spec.args);
    // Its own process group: a terminal's Ctrl-C reaches boatparty, which
    // cancels through stdin, rather than killing the connection outright.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Some(mut pack) = pack {
                let _ = pack.kill();
                let _ = pack.wait();
            }
            return Err(AppError::io("cannot start the runner", error));
        }
    };
    let (close_tx, close_rx) = mpsc::channel::<()>();
    let writer = spawn_writer(
        child.stdin.take().expect("stdin is piped"),
        spec.header,
        source,
        spec.hold_stdin.then_some(close_rx),
    );
    let (tx, rx) = mpsc::channel();
    pump(
        child.stdout.take().expect("stdout is piped"),
        tx.clone(),
        Chunk::Out,
        Chunk::OutEnd,
    );
    pump(
        child.stderr.take().expect("stderr is piped"),
        tx,
        Chunk::Err,
        Chunk::ErrEnd,
    );

    let mut filter = TrailerFilter::new(spec.nonce);
    let mut sniffer = MissingSniffer::default();
    let watched = watch(
        &mut child,
        &rx,
        &mut filter,
        &mut sniffer,
        sink,
        cancel,
        &close_tx,
    );
    let mut tail = sniffer.pass(&filter.end());
    tail.extend(sniffer.finish());
    if !tail.is_empty() {
        sink.stderr(&tail);
    }
    let _ = close_tx.send(());
    let status = reap(&mut child, watched.killed);
    let pack_bytes = writer.join().unwrap_or(0);
    let trailer = filter.result();
    if let Some(mut child) = pack {
        let pack_status = pack::finish(&mut child);
        // A runner that stopped reading early (needfull, missing runner,
        // cancel) makes pack-objects fail on a closed pipe; only a pack the
        // runner accepted must have been built successfully.
        if let (Err(error), Some(Trailer::Done { .. })) = (pack_status, &trailer) {
            return Err(error);
        }
    }
    Ok(Exchange {
        runner_missing: status == Some(EXIT_MISSING) && sniffer.matched,
        status: if trailer == Some(Trailer::NeedFull) {
            Some(EXIT_NEEDFULL)
        } else {
            status
        },
        trailer,
        pack_bytes,
        cancelled: watched.cancelled,
    })
}

struct Watch {
    killed: bool,
    cancelled: bool,
}

/// Move output to the sink until both pipes end, or until they can no
/// longer be trusted to end.
fn watch(
    child: &mut Child,
    rx: &Receiver<Chunk>,
    filter: &mut TrailerFilter,
    sniffer: &mut MissingSniffer,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
    close: &Sender<()>,
) -> Watch {
    let mut open = 2;
    let mut exited_at: Option<Instant> = None;
    let mut trailer_at: Option<Instant> = None;
    let mut cancel_at: Option<Instant> = None;
    let mut result = Watch {
        killed: false,
        cancelled: false,
    };
    while open > 0 {
        match rx.recv_timeout(TICK) {
            Ok(Chunk::Out(bytes)) => sink.stdout(&bytes),
            Ok(Chunk::Err(bytes)) => {
                for piece in filter.push_pieces(&bytes) {
                    match piece {
                        Piece::Bytes(bytes) => {
                            let printable = sniffer.pass(&bytes);
                            if !printable.is_empty() {
                                sink.stderr(&printable);
                            }
                        }
                        Piece::Event(name) => sink.event(&name),
                    }
                }
                if trailer_at.is_none() && filter.complete() {
                    trailer_at = Some(Instant::now());
                }
            }
            Ok(Chunk::OutEnd | Chunk::ErrEnd) => open -= 1,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        match cancel.state() {
            CancelState::None => {}
            CancelState::Soft if cancel_at.is_none() => {
                result.cancelled = true;
                cancel_at = Some(Instant::now());
                let _ = close.send(());
            }
            CancelState::Soft => {}
            CancelState::Hard => {
                result.cancelled = true;
                result.killed = true;
                let _ = child.kill();
                return result;
            }
        }
        if exited_at.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited_at = Some(Instant::now());
        }
        let overdue =
            |at: Option<Instant>, limit: Duration| at.is_some_and(|at| at.elapsed() > limit);
        if overdue(exited_at, PIPE_GRACE) {
            break;
        }
        if overdue(trailer_at, AFTER_TRAILER) || overdue(cancel_at, CANCEL_GRACE) {
            result.killed = true;
            let _ = child.kill();
            break;
        }
    }
    result
}

/// Wait for the child, killing it if it does not exit soon after its pipes
/// closed.
fn reap(child: &mut Child, killed: bool) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if killed { None } else { status.code() },
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn pump<R: Read + Send + 'static>(
    mut pipe: R,
    tx: Sender<Chunk>,
    wrap: fn(Vec<u8>) -> Chunk,
    end: Chunk,
) {
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if tx.send(wrap(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(end);
    });
}

/// A built pack file, kept (and so not deleted) while it is read.
struct OpenPack {
    file: std::fs::File,
    _pack: TempPack,
}

impl OpenPack {
    fn open(pack: TempPack) -> Result<Self, AppError> {
        let file = std::fs::File::open(pack.path()).map_err(|error| {
            AppError::io(format!("cannot open {}", pack.path().display()), error)
        })?;
        Ok(Self { file, _pack: pack })
    }
}

impl Read for OpenPack {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

/// Write the header, then stream the pack, into the runner; then, when
/// asked to hold stdin, keep it open until told to close it. Returns the pack
/// bytes written. A runner that stops reading ends the copy quietly; its
/// trailer says what happened.
fn spawn_writer(
    mut stdin: ChildStdin,
    header: Vec<u8>,
    pack: Option<Box<dyn Read + Send>>,
    hold: Option<Receiver<()>>,
) -> JoinHandle<u64> {
    std::thread::spawn(move || {
        let mut written = 0u64;
        if stdin.write_all(&header).is_ok()
            && let Some(mut pack) = pack
        {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let read = match pack.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if stdin.write_all(&buffer[..read]).is_err() {
                    break;
                }
                written += read as u64;
            }
        }
        let _ = stdin.flush();
        if let Some(hold) = hold {
            let _ = hold.recv();
        }
        drop(stdin);
        written
    })
}

/// Holds back the start of the runner's stderr while it could still be the
/// missing-runner marker, so that marker is never shown.
#[derive(Debug, Default)]
pub(crate) struct MissingSniffer {
    held: Vec<u8>,
    decided: bool,
    matched: bool,
}

impl MissingSniffer {
    pub(crate) fn pass(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.decided {
            return bytes.to_vec();
        }
        self.held.extend_from_slice(bytes);
        let marker = format!("{MISSING_MARKER}\n").into_bytes();
        let n = self.held.len().min(marker.len());
        if self.held[..n] != marker[..n] {
            self.decided = true;
            return std::mem::take(&mut self.held);
        }
        if self.held.len() >= marker.len() {
            self.decided = true;
            self.matched = true;
            let rest = self.held.split_off(marker.len());
            self.held.clear();
            return rest;
        }
        Vec::new()
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        self.decided = true;
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::MissingSniffer;

    #[test]
    fn the_missing_marker_is_swallowed_and_other_output_is_not() {
        let mut sniffer = MissingSniffer::default();
        let mut out = sniffer.pass(b"boatparty-run");
        out.extend(sniffer.pass(b"ner-missing\n"));
        out.extend(sniffer.finish());
        assert!(out.is_empty());
        assert!(sniffer.matched);

        let mut sniffer = MissingSniffer::default();
        let mut out = sniffer.pass(b"boat");
        out.extend(sniffer.pass(b"s ahoy\n"));
        out.extend(sniffer.finish());
        assert_eq!(out, b"boats ahoy\n");
        assert!(!sniffer.matched);
    }
}
