//! An end-to-end harness: a Git repository with `boatparty init` run, a
//! fake `ssh` that runs remote commands locally (with /home/user/ mapped to
//! a scratch directory), a fake boat API (including signed uploads) and a
//! shared event log for ssh runs and provider calls.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::runner::base_repo;
use super::{FakeBoat, Reply, TempDir, boatparty, store_key};

pub const ID: &str = "bx_23456789";
/// Part of every signed upload URL the fake hands out; it must never show up
/// in output, state or logs.
pub const URL_SECRET: &str = "s3cr3t-upload-token";
/// The access token in every gated URL `expose` gets from the fake; it may
/// appear on stdout and nowhere else.
pub const HOST_TOKEN: &str = "h0st-t0ken-for-tests";

pub struct E2e {
    pub root: TempDir,
}

impl E2e {
    /// A project in `full` history mode, as the sync and run tests expect.
    pub fn new(label: &str) -> Self {
        Self::with_history(label, Some("full"))
    }

    /// A project with this `[sync] history` (`None`: the default).
    pub fn with_history(label: &str, history: Option<&str>) -> Self {
        let root = TempDir::new(label);
        let setup = Self { root };
        let local = base_repo(setup.root.path());
        fs::rename(&local, setup.repo()).unwrap();
        fs::create_dir_all(setup.remote()).unwrap();
        store_key(&setup.home());
        setup.tools();
        boatparty(&setup.home())
            .current_dir(setup.repo())
            .args(["init", "--name", "app"])
            .assert()
            .success();
        // A lease shorter than the idle length, so the idle deadline (300 s)
        // is told apart from a heartbeat (240 s).
        let account = setup.home().join("config/account.toml");
        let text = fs::read_to_string(&account).unwrap();
        fs::write(
            &account,
            text.replace("lease_minutes = 5", "lease_minutes = 4"),
        )
        .unwrap();
        if let Some(history) = history {
            let file = setup.repo().join("boatparty.toml");
            let mut text = fs::read_to_string(&file).unwrap();
            text.push_str(&format!("\n[sync]\nhistory = \"{history}\"\n"));
            fs::write(&file, text).unwrap();
        }
        setup
    }

    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub fn repo(&self) -> PathBuf {
        self.root.path().join("app")
    }

    pub fn remote(&self) -> PathBuf {
        self.root.path().join("remote")
    }

    pub fn events(&self) -> PathBuf {
        self.root.path().join("events.log")
    }

    fn tools(&self) {
        let bin = self.root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let ssh = format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
               case \"$a\" in check) exit \"${{FAKE_SSH_CHECK:-1}}\";; exit) exit 0;; esac\n\
             done\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             cmd=$(printf '%s' \"$last\" | sed -e 's#/home/user/#{remote}/#g')\n\
             case \"$cmd\" in *' run app '*|*' attached-run app '*) echo ssh-run >> '{events}';; *' settle app '*) echo ssh-settle >> '{events}';; \
               *' serve app '*) for a in \"$@\"; do [ \"$a\" = \"$last\" ] || printf '%s ' \"$a\"; done | sed 's/^/ssh-serve /' >> '{events}'; echo >> '{events}';; esac\n\
             exec sh -c \"$cmd\"\n",
            remote = self.remote().display(),
            events = self.events().display(),
        );
        let keygen = "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -f ]; then f=\"$2\"; fi; shift; done\n\
                      printf 'private\\n' > \"$f\"\nprintf 'ssh-ed25519 AAAAfakekey boatparty\\n' > \"$f.pub\"\n";
        for (name, body) in [("ssh", ssh.as_str()), ("ssh-keygen", keygen)] {
            let path = bin.join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    pub fn cmd(&self, boat: &FakeBoat) -> assert_cmd::Command {
        let path = format!(
            "{}:{}",
            self.root.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = boatparty(&self.home());
        command
            .current_dir(self.repo())
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("BOATPARTY_RUNNER_REPO", self.remote().join("app"))
            .env("BOATPARTY_RUNNER_STATE", self.remote().join(".boatparty"))
            .env("BOATPARTY_RUNNER_JOBS", self.root.path().join("jobs"))
            .env("PATH", path)
            .env_remove("FAKE_SSH_CHECK")
            .env_remove("BOATPARTY_SIGNED_URL_MIN_BYTES");
        command
    }

    /// `cmd` as a std command, for a client a test spawns and kills.
    pub fn spawn_cmd(&self, boat: &FakeBoat) -> std::process::Command {
        let path = format!(
            "{}:{}",
            self.root.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = super::isolated(env!("CARGO_BIN_EXE_boatparty"));
        command
            .current_dir(self.repo())
            .env("BOATPARTY_HOME", self.home())
            .env_remove("BOATPARTY_BOAT_API_KEY")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("BOATPARTY_RUNNER_REPO", self.remote().join("app"))
            .env("BOATPARTY_RUNNER_STATE", self.remote().join(".boatparty"))
            .env("BOATPARTY_RUNNER_JOBS", self.root.path().join("jobs"))
            .env("PATH", path)
            .env_remove("FAKE_SSH_CHECK")
            .env_remove("BOATPARTY_SIGNED_URL_MIN_BYTES");
        command
    }

    /// The fake boat API, whose signed uploads succeed.
    pub fn boat(&self) -> FakeBoat {
        self.boat_with_uploads(200)
    }

    /// The fake boat API. Signed upload and download URLs point back at the
    /// fake, whose `PUT` and `GET` answer `put_status` and, on success, write
    /// or read the file in the mapped /home/user/.
    pub fn boat_with_uploads(&self, put_status: u16) -> FakeBoat {
        let events = self.events();
        let remote = self.remote();
        let base: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let base_in = Arc::clone(&base);
        let boat = FakeBoat::start(move |request| {
            let mut log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&events)
                .unwrap();
            let body = if request.method == "PUT" {
                format!("<{} bytes>", request.bytes.len())
            } else {
                request.body.split_whitespace().collect::<String>()
            };
            let path_only = request.path.split('?').next().unwrap_or_default();
            // One write per line: requests are served on concurrent threads,
            // and `writeln!` on a file may split a line into several writes.
            let line = format!("provider {} {} {}\n", request.method, path_only, body);
            let _ = log.write_all(line.as_bytes());
            let sandbox = json!({
                "id": ID, "name": "bp-app-1", "state": "idle", "hydrated": true,
                "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
                "type": "default", "archiveAfter": "2026-11-15T12:15:00Z",
            });
            let upload_prefix = format!("/upload/{URL_SECRET}");
            let download_prefix = format!("/download/{URL_SECRET}");
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/sandboxes") => Reply::json(
                    202,
                    &json!({"ok": true, "type": "sandbox.created", "status": "provisioning",
                            "ttlSeconds": 900, "sandbox": sandbox})
                    .to_string(),
                ),
                ("POST", path) if path.ends_with("/sshkey") => Reply::json(
                    200,
                    &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                            "machineIp": "198.51.100.7", "sshUser": "user",
                            "sshEndpoint": "203.0.113.10:22001",
                            "hostKey": "ssh-ed25519 AAAAhostkey"})
                    .to_string(),
                ),
                ("POST", path) if path == format!("/sandboxes/{ID}/files/upload-url") => {
                    let asked: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                    let target = asked["path"].as_str().unwrap().to_string();
                    let host = base_in.lock().unwrap().clone();
                    Reply::json(
                        200,
                        &json!({"ok": true, "type": "file.upload_url", "method": "PUT",
                                "path": target, "expiresAt": "2026-11-15T12:15:00Z",
                                "url": format!("{host}{upload_prefix}{target}?sig={URL_SECRET}")})
                        .to_string(),
                    )
                }
                ("POST", path) if path == format!("/sandboxes/{ID}/files/download-url") => {
                    let asked: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                    let target = asked["path"].as_str().unwrap().to_string();
                    let host = base_in.lock().unwrap().clone();
                    Reply::json(
                        200,
                        &json!({"ok": true, "type": "file.download_url", "method": "GET",
                                "path": target, "expiresAt": "2026-11-15T12:15:00Z",
                                "url": format!("{host}{download_prefix}{target}?sig={URL_SECRET}")})
                        .to_string(),
                    )
                }
                ("POST", path) if path == format!("/sandboxes/{ID}/host") => {
                    let asked: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                    let port = asked["port"].as_u64().unwrap();
                    let public = asked["public"].as_bool().unwrap_or(false);
                    let url = if public {
                        format!("https://app-{port}.on.boat.example")
                    } else {
                        format!("https://app-{port}.on.boat.example?_token={HOST_TOKEN}")
                    };
                    Reply::json(
                        200,
                        &json!({"ok": true, "type": "port.hosted", "success": true,
                                "port": port, "url": url, "isProtected": !public,
                                "access": if public { "public" } else { "private" },
                                "localhost": asked["localhost"]})
                        .to_string(),
                    )
                }
                ("GET", path) if path.starts_with(&download_prefix) => {
                    if put_status != 200 {
                        return Reply::json(put_status, "{}");
                    }
                    let target = path_only.strip_prefix(&download_prefix).unwrap();
                    let mapped = remote.join(target.strip_prefix("/home/user/").unwrap());
                    match fs::read(&mapped) {
                        Ok(bytes) => Reply::bytes(200, bytes),
                        Err(_) => Reply::json(404, "{}"),
                    }
                }
                ("PUT", path) if path.starts_with(&upload_prefix) => {
                    if put_status != 200 {
                        return Reply::json(
                            put_status,
                            &format!("{{\"error\":\"refused {}\"}}", request.path),
                        );
                    }
                    let target = path_only.strip_prefix(&upload_prefix).unwrap();
                    let mapped = remote.join(target.strip_prefix("/home/user/").unwrap());
                    fs::create_dir_all(mapped.parent().unwrap()).unwrap();
                    fs::write(&mapped, &request.bytes).unwrap();
                    Reply::json(200, r#"{"success":true}"#)
                }
                ("GET" | "PATCH", path) if path == format!("/sandboxes/{ID}") => Reply::json(
                    200,
                    &json!({"ok": true, "type": "sandbox.info", "sandbox": sandbox}).to_string(),
                ),
                _ => Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#),
            }
        });
        // The fake's own origin, for the URLs it signs.
        *base.lock().unwrap() = boat.url().trim_end_matches("/api/v1").to_string();
        boat
    }

    /// How many TTL changes the fake has seen.
    pub fn patches(&self) -> usize {
        self.event_lines()
            .iter()
            .filter(|line| line.starts_with("provider PATCH"))
            .count()
    }

    /// Wait (up to 10 s) until more than `before` TTL changes were seen: the
    /// detached idle helper's, after a command that ended.
    pub fn wait_patches(&self, before: usize) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if self.patches() > before {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// `start`, then wait for the idle helper it starts to set the idle
    /// deadline, so its provider calls do not mix with the next command's.
    pub fn start(&self, boat: &FakeBoat) {
        let before = self.patches();
        self.cmd(boat).arg("start").assert().success();
        assert!(self.wait_patches(before), "start set no idle deadline");
    }

    pub fn event_lines(&self) -> Vec<String> {
        fs::read_to_string(self.events())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}
