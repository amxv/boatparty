//! Start the remote streaming services and issue a short-lived desktop link.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::session::with_session;
use crate::Context;
use crate::error::AppError;
use crate::pool::wait;
use crate::provider::{BusyReason, ErrorKind};
use crate::runner::{EXIT_MISSING, RunnerHost, SshRunner};
use serde_json::json;

pub fn run(ctx: &Context, open: bool) -> Result<u8, AppError> {
    let ended = with_session(ctx, "desktop", |held| {
        let id =
            held.member.id.as_deref().ok_or_else(|| {
                AppError::new("member_pending", "the member has no sandbox id yet")
            })?;
        let provider = super::open_provider(ctx)?;
        crate::pool::health::ensure_tunnel(
            ctx,
            held.project,
            held.member,
            held.target,
            provider.as_ref(),
        )?;
        let host = SshRunner::ssh_only(held.target.clone());
        let args = ["desktop", held.project].map(String::from);
        for attempt in 0..2 {
            let status = host
                .command(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .map_err(|error| AppError::io("cannot start desktop services", error))?;
            if status.success() {
                break;
            }
            if status.code() == Some(EXIT_MISSING) && attempt == 0 {
                host.install()?;
                continue;
            }
            return Err(AppError::new(
                "desktop_failed",
                "the sandbox desktop services did not start",
            ));
        }
        let url = wait::retry(
            Duration::from_secs(180),
            |kind| {
                wait::transient(kind)
                    || matches!(
                        kind,
                        ErrorKind::NotRunning
                            | ErrorKind::Busy {
                                reason: BusyReason::Starting,
                                ..
                            }
                    )
            },
            || provider.desktop_link(id),
        )?;
        if open {
            open_link(&url)?;
        }
        if ctx.out.is_json() {
            ctx.out.document(&json!({"ok":true, "project":held.project, "member":{"name":held.member.name,"id":id}, "url":url,"opened":open}));
        } else if !open {
            ctx.out.text(url);
        }
        Ok(())
    })?;
    ended.value?;
    Ok(ended.interrupted.unwrap_or(0))
}

/// Only an expiring loopback address goes into the platform opener's argv.
/// Deliver the signed link in a no-store redirect after the browser connects.
fn open_link(url: &str) -> Result<(), AppError> {
    let failed = |e| AppError::io("cannot open the desktop in a browser", e);
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(failed)?;
    listener.set_nonblocking(true).map_err(failed)?;
    let ticket = crate::system::uuid_v4()?;
    let address = format!("http://{}/{ticket}", listener.local_addr().map_err(failed)?);
    #[cfg(target_os = "macos")]
    let mut opener = Command::new("open");
    #[cfg(windows)]
    let mut opener = {
        let mut cmd = Command::new("rundll32.exe");
        cmd.arg("url.dll,FileProtocolHandler");
        cmd
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut opener = Command::new("xdg-open");
    let mut child = opener
        .arg(address)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failed)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .map_err(failed)?;
                let mut request = String::new();
                if BufReader::new((&stream).take(2048))
                    .read_line(&mut request)
                    .is_ok()
                    && request
                        .split_whitespace()
                        .take(2)
                        .eq(["GET", &format!("/{ticket}")])
                {
                    write!(stream, "HTTP/1.1 302 Found\r\nLocation: {url}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").map_err(failed)?;
                    return Ok(());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if child
                    .try_wait()
                    .map_err(failed)?
                    .is_some_and(|s| !s.success())
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(failed(e)),
        }
    }
    Err(AppError::new(
        "desktop_failed",
        "the browser did not open the desktop link. Run boatparty desktop to print it.",
    ))
}
