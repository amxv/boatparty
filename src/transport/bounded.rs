//! Short SSH operations with a wall-clock limit, including pipe I/O.
use crate::error::AppError;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) fn capture(
    command: &mut Command,
    input: Option<Vec<u8>>,
    step: &str,
    limit: Duration,
) -> Result<(i32, String), AppError> {
    capture_stream(command, input, step, limit, false)
}

/// Capture one of the two output streams while discarding the other.
pub(super) fn capture_stream(
    command: &mut Command,
    input: Option<Vec<u8>>,
    step: &str,
    limit: Duration,
    stdout: bool,
) -> Result<(i32, String), AppError> {
    let end = Instant::now() + limit;
    let timeout = || {
        AppError::new(
            "step_timeout",
            format!("{step} timed out after {}s", limit.as_secs()),
        )
    };
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if stdout {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .spawn()
        .map_err(|e| AppError::io(step, e))?;
    let mut output: Box<dyn Read + Send> = if stdout {
        Box::new(child.stdout.take().expect("piped"))
    } else {
        Box::new(child.stderr.take().expect("piped"))
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .by_ref()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .and_then(|_| std::io::copy(&mut output, &mut std::io::sink()));
        let _ = tx.send(result.map(|_| bytes));
    });
    let (written, writer) = mpsc::channel();
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = written.send(stdin.write_all(&bytes));
        });
    } else {
        let _ = written.send(Ok(()));
    }
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| AppError::io(step, e))? {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(timeout());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let bytes = rx
        .recv_timeout(end.saturating_duration_since(Instant::now()))
        .map_err(|_| timeout())?
        .map_err(|e| AppError::io(step, e))?;
    let write = writer
        .recv_timeout(end.saturating_duration_since(Instant::now()))
        .map_err(|_| timeout())?;
    if status.success() {
        write.map_err(|e| AppError::io(step, e))?;
    }
    Ok((
        status.code().unwrap_or(255),
        String::from_utf8_lossy(&bytes).trim().into(),
    ))
}
