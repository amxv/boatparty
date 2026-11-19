//! Shared helpers for the CLI tests: an isolated home and a fake boat API.

#![allow(dead_code)]

pub mod contract;
#[cfg(unix)]
pub mod e2e;
pub mod runner;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use assert_cmd::Command;

pub const TEST_KEY: &str = "boat_test_key_do_not_print_7c1e";

/// A temporary directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("boatparty-test-{}-{label}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Variables of the environment the tests themselves run in that would leak
/// into what they observe. Tests run inside `boatparty run` too, whose
/// runner exports the developer's Git identity (GIT_AUTHOR_*, GIT_COMMITTER_*
/// and user.name and user.email through GIT_CONFIG_COUNT), `BOATPARTY=1`
/// and, with a token, `BOATPARTY_CREDENTIAL_SOCKET`. Every process a test
/// starts drops them, so results do not depend on where the tests run.
pub fn ambient_vars() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let name = name.to_string_lossy();
            name == "BOATPARTY"
                || name == "BOATPARTY_CREDENTIAL_SOCKET"
                || name == "GIT_CONFIG_COUNT"
                || name == "GIT_CONFIG_PARAMETERS"
                || name.starts_with("GIT_CONFIG_KEY_")
                || name.starts_with("GIT_CONFIG_VALUE_")
                || name.starts_with("GIT_AUTHOR_")
                || name.starts_with("GIT_COMMITTER_")
        })
        .collect()
}

/// A std command for `program` without the ambient variables.
pub fn isolated(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    for name in ambient_vars() {
        command.env_remove(name);
    }
    command
}

/// The binary with an isolated environment rooted at `home`.
pub fn boatparty(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_boatparty"));
    for name in ambient_vars() {
        command.env_remove(name);
    }
    command
        .env("BOATPARTY_HOME", home)
        .env_remove("BOATPARTY_BOAT_API_KEY")
        .env_remove("BOATPARTY_BOAT_API_URL")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME");
    command
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// The body's raw bytes.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Raw bytes sent instead of `body`, when set.
    pub raw: Option<Vec<u8>>,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
            raw: None,
        }
    }

    pub fn bytes(status: u16, bytes: Vec<u8>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
            raw: Some(bytes),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

/// A minimal HTTP/1.1 server on 127.0.0.1 that answers with `handler` and
/// records every request.
pub struct FakeBoat {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeBoat {
    pub fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let recorded = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let handler = Arc::clone(&handler);
                let recorded = Arc::clone(&recorded);
                thread::spawn(move || serve(stream, &*handler, &recorded));
            }
        });
        Self { url, requests }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, handler: &Handler, recorded: &Mutex<Vec<Request>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let full_path = parts.next().unwrap_or_default().to_string();
    let path = full_path
        .strip_prefix("/api/v1")
        .unwrap_or(&full_path)
        .to_string();
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let request = Request {
        method,
        path,
        authorization: headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.clone()),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
        bytes: body,
    };
    let reply = handler(&request);
    recorded.lock().unwrap().push(request);

    let mut stream = stream;
    let body = reply
        .raw
        .clone()
        .unwrap_or_else(|| reply.body.clone().into_bytes());
    let mut response = format!(
        "HTTP/1.1 {} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        body.len()
    );
    for (name, value) in &reply.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut response = response.into_bytes();
    response.extend_from_slice(&body);
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

pub fn fixture(path: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/boat")
            .join(path),
    )
    .unwrap()
}

/// Store `TEST_KEY` in the isolated home as `login` would, without the network.
pub fn store_key(home: &Path) {
    boatparty(home)
        .args(["login", "--key-stdin", "--no-verify"])
        .write_stdin(format!("{TEST_KEY}\n"))
        .assert()
        .success();
}

/// A directory holding fake `ssh`, `ssh-keygen` and `git` executables.
pub fn fake_tools(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["ssh", "ssh-keygen", "git"] {
        let path = bin.join(format!("{tool}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}
