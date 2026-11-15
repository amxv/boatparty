//! Large transfers through boat's signed file URLs.
//!
//! boat signs a short-lived URL that the sandbox serves itself; a `PUT` of
//! the raw bytes writes the file and a `GET` reads it. Both are several
//! times faster than SSH (docs/boat-facts.md, question 2). The URL is a secret until it
//! expires, so it never leaves this module: it is not returned, logged,
//! formatted into an error or stored, and transport errors are reduced to a
//! fixed description that cannot contain it.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::http::{Http, Method};
use super::sandboxes::checked_id;
use crate::provider::{ErrorKind, ProviderError};

/// How long a signed URL stays valid. The upload starts right away.
const URL_TTL_SECONDS: u64 = 900;
const USER_AGENT: &str = concat!("boatparty/", env!("CARGO_PKG_VERSION"));

#[derive(Deserialize)]
struct UrlBody {
    url: String,
}

/// Put the local `file` on the sandbox at `remote_path` (under /home/user
/// or /tmp, boat's rule for signed URLs).
pub fn upload_file(
    http: &Http,
    id: &str,
    remote_path: &str,
    file: &Path,
) -> Result<(), ProviderError> {
    let id = checked_id(id)?;
    let body = json!({"path": remote_path, "ttlSeconds": URL_TTL_SECONDS});
    let answer = http.send(
        Method::Post,
        &format!("/sandboxes/{id}/files/upload-url"),
        Some(&body),
        None,
    )?;
    let signed: UrlBody = answer.json()?;
    put(&signed.url, file)
}

/// Read `remote_path` on the sandbox (under /home/user or /tmp) into the
/// local `file`, which is created or replaced.
pub fn download_file(
    http: &Http,
    id: &str,
    remote_path: &str,
    file: &Path,
) -> Result<u64, ProviderError> {
    let id = checked_id(id)?;
    let body = json!({"path": remote_path, "ttlSeconds": URL_TTL_SECONDS});
    let answer = http.send(
        Method::Post,
        &format!("/sandboxes/{id}/files/download-url"),
        Some(&body),
        None,
    )?;
    let signed: UrlBody = answer.json()?;
    get(&signed.url, file)
}

fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        // A large file may take minutes; only a stalled connection fails.
        .timeout_global(None)
        .timeout_per_write(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .max_redirects(0)
        .user_agent(USER_AGENT)
        .build();
    ureq::Agent::new_with_config(config)
}

fn check_url(url: &str) -> Result<(), ProviderError> {
    if secure_url(url) {
        Ok(())
    } else {
        Err(ProviderError::new(ErrorKind::InvalidResponse)
            .with_message("the signed file URL does not use https"))
    }
}

/// `GET` the signed URL into the file.
fn get(url: &str, file: &Path) -> Result<u64, ProviderError> {
    check_url(url)?;
    let transfer = |error: &ureq::Error| {
        ProviderError::new(ErrorKind::Transport)
            .with_message(format!("the signed download failed: {}", describe(error)))
    };
    let mut response = agent().get(url).call().map_err(|error| transfer(&error))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(refused("download", status));
    }
    let mut out = File::create(file).map_err(|error| local_error(file, &error))?;
    let mut reader = response.body_mut().with_config().reader();
    std::io::copy(&mut reader, &mut out).map_err(|error| {
        ProviderError::new(ErrorKind::Transport).with_message(format!(
            "the signed download failed: I/O error ({})",
            error.kind()
        ))
    })
}

fn local_error(file: &Path, error: &std::io::Error) -> ProviderError {
    ProviderError::new(ErrorKind::Transport).with_message(format!(
        "cannot use {}: {}",
        file.display(),
        error.kind()
    ))
}

/// A non-success answer. Its body is not read: nothing from it reaches a
/// message.
fn refused(what: &str, status: u16) -> ProviderError {
    ProviderError::new(if status >= 500 {
        ErrorKind::Server
    } else {
        ErrorKind::Unknown
    })
    .with_status(status)
    .with_message(format!("the signed {what} was refused with HTTP {status}"))
}

/// `PUT` the file to the signed URL.
fn put(url: &str, file: &Path) -> Result<(), ProviderError> {
    check_url(url)?;
    let body = File::open(file).map_err(|error| local_error(file, &error))?;
    let response = agent()
        .put(url)
        .header("Content-Type", "application/octet-stream")
        .send(body)
        .map_err(|error| {
            ProviderError::new(ErrorKind::Transport)
                .with_message(format!("the signed upload failed: {}", describe(&error)))
        })?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(refused("upload", status))
}

/// https, or plain http to a loopback address (local fakes in tests).
fn secure_url(url: &str) -> bool {
    url.starts_with("https://")
        || ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|prefix| url.starts_with(prefix))
}

/// A fixed description of a transport error. ureq's own text can quote the
/// URL, so it is never used.
fn describe(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(status) => format!("HTTP {status}"),
        ureq::Error::Io(io) => format!("I/O error ({})", io.kind()),
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::Tls(_) => "TLS error".to_string(),
        ureq::Error::BadUri(_) => "the URL was not understood".to_string(),
        _ => "transfer error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_urls_are_used() {
        assert!(secure_url("https://box-node-1.on.boat.dev/u/x?sig=1"));
        assert!(secure_url("http://127.0.0.1:4100/upload"));
        assert!(!secure_url("http://box-node-1.on.boat.dev/u/x"));
        assert!(!secure_url("http://127.0.0.1.evil.example/u"));
    }

    #[test]
    fn transport_errors_never_quote_the_url() {
        let secret = "https://box.example/upload/SECRET?sig=SECRET";
        let error = ureq::Error::BadUri(secret.to_string());
        assert!(!describe(&error).contains("SECRET"));
        assert_eq!(describe(&ureq::Error::StatusCode(403)), "HTTP 403");
    }
}
