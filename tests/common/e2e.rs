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

pub struct E2e {
    pub root: TempDir,
}

impl E2e {
    /// A project in `full` history mode, as Phase 4 and 5 tests expect.
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
             case \"$cmd\" in *' run app '*) echo ssh-run >> '{events}';; esac\n\
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
            let _ = writeln!(log, "provider {} {} {}", request.method, path_only, body);
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

    pub fn event_lines(&self) -> Vec<String> {
        fs::read_to_string(self.events())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}
