//! Build credentials on the runner: the Git identity reaches commits a
//! command makes, and a GitHub token reaches Git (in steps and the command)
//! through the credential helper only, then disappears: no file, argument
//! list or environment holds it, during or after the command. The runner
//! runs locally against scratch repositories.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use boatparty::project::ProjectConfig;
use boatparty::runner::RunnerSpec;
use boatparty::sync::seed::Secret;
use boatparty::sync::{
    self, History, MirrorRequest, RunCredentials, RunEnd, Sink, TokenSource, WorktreeInfo,
};
use common::TempDir;
use common::runner::{LocalRunner, base_repo, git};

const TOKEN: &str = "gho_credtest_never_print_7f3a";

#[derive(Default)]
struct Collect {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Sink for Collect {
    fn stdout(&mut self, bytes: &[u8]) {
        self.stdout.extend_from_slice(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
    }
}

/// A scratch member whose runner has its own HOME, TMPDIR and RAM
/// directory, all under one root, and no system Git configuration.
struct Member {
    root: TempDir,
    local: PathBuf,
    host: LocalRunner,
}

impl Member {
    fn new(label: &str, config: &str) -> Self {
        let root = TempDir::new(label);
        let local = base_repo(root.path());
        let host = LocalRunner::new(root.path(), "remote");
        let text = format!("version = 1\n[project]\nname = \"app\"\n{config}");
        let project = ProjectConfig::parse(&text, Path::new("boatparty.toml")).unwrap();
        *host.spec.borrow_mut() = Some(RunnerSpec::new(project.config()));
        for dir in ["home", "tmp", "shm"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
            let name = match dir {
                "home" => "HOME",
                "tmp" => "TMPDIR",
                _ => "BOATPARTY_RUNNER_SHM",
            };
            host.env
                .borrow_mut()
                .push((name.into(), root.path().join(dir).display().to_string()));
        }
        // No system configuration, and no desktop askpass prompt for a
        // credential the helper does not answer.
        for (name, value) in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_ASKPASS", "false"),
            ("SSH_ASKPASS", "false"),
        ] {
            host.env.borrow_mut().push((name.into(), value.into()));
        }
        Self { root, local, host }
    }

    fn set_env(&self, name: &str, value: &str) {
        self.host.env.borrow_mut().push((name.into(), value.into()));
    }

    fn cheers(&self, command: &str, credentials: &RunCredentials) -> (i32, String, String) {
        let info = WorktreeInfo::read(&self.local).unwrap();
        let snapshot = sync::snapshot(&info).unwrap();
        let refs = sync::local_refs(&info).unwrap();
        let request = MirrorRequest {
            project: "app",
            info: &info,
            snapshot: &snapshot,
            refs: &refs,
            send_refs: true,
            known: &[],
            history: History::Full,
            seed: None,
        };
        let mut printed = Collect::default();
        let outcome = sync::run_command_with(
            &self.host,
            &request,
            command,
            credentials,
            &mut printed,
            &sync::NeverCancel,
        )
        .unwrap();
        let stdout = String::from_utf8_lossy(&printed.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&printed.stderr).into_owned();
        match outcome.end {
            RunEnd::Done { exit_code, .. } => (exit_code, stdout, stderr),
            other => panic!("no result: {other:?}; stderr {stderr}"),
        }
    }
}

fn with_token() -> RunCredentials {
    RunCredentials {
        github_token: Some(Secret::new(TOKEN)),
        ..RunCredentials::none()
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn assert_no_token_under(dir: &Path) {
    for file in files_under(dir) {
        let bytes = fs::read(&file).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TOKEN),
            "the token is in {}",
            file.display()
        );
    }
}

/// The SHA-1 Git gives `text` as a blob.
fn git_hash(text: &str) -> String {
    let mut child = std::process::Command::new("git")
        .args(["-C", "/", "hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    String::from_utf8(child.wait_with_output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

/// A shell line that asks Git for github.com's credential and prints only
/// the hash of the password it got.
fn hashed_fill(host: &str) -> String {
    format!(
        "printf 'protocol=https\\nhost={host}\\n\\n' | GIT_TERMINAL_PROMPT=0 git credential fill \
         2>/dev/null | sed -n 's/^password=//p' | git hash-object --stdin"
    )
}

#[test]
fn the_local_identity_is_read_from_the_repository_and_reaches_remote_commits() {
    let member = Member::new("cred-identity", "");
    git(&member.local, &["config", "user.name", "Ada Lovelace"]);
    git(&member.local, &["config", "user.email", "ada@example.com"]);
    let on = RunCredentials::gather(&member.local, true, &TokenSource::None);
    assert_eq!(on.git_name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(on.git_email.as_deref(), Some("ada@example.com"));
    assert!(on.github_token.is_none());
    let off = RunCredentials::gather(&member.local, false, &TokenSource::None);
    assert!(!off.has_identity());

    let command = "git commit --allow-empty -qm remote; \
                   git log -1 --format='%an|%ae|%cn|%ce'; git config user.name || true";
    let (code, stdout, stderr) = member.cheers(command, &on);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout, "Ada Lovelace|ada@example.com|Ada Lovelace|ada@example.com\nAda Lovelace\n",
        "{stderr}"
    );
    // Nothing about it was written to the checkout's configuration.
    let config = fs::read_to_string(member.host.repo.join(".git/config")).unwrap();
    assert!(!config.contains("Ada"), "{config}");

    // Switched off: the command sees no identity of the developer's.
    let (_, stdout, _) = member.cheers(command, &off);
    assert!(!stdout.contains("Ada"), "{stdout}");

    // [run.env] still wins over the identity.
    let member = Member::new(
        "cred-identity-env",
        "[run.env]\nGIT_AUTHOR_NAME = \"Bot\"\n",
    );
    let (code, stdout, stderr) = member.cheers(
        "git commit --allow-empty -qm remote; git log -1 --format='%an|%cn'",
        &on,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "Bot|Ada Lovelace\n");
}

/// A stand-in for github.com over plain HTTP: asks for Basic credentials and
/// records the ones it is given.
fn fake_github() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut auth = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.trim().split_once(": ")
                    && name.eq_ignore_ascii_case("authorization")
                {
                    auth = Some(value.to_string());
                }
            }
            let reply = match auth {
                Some(value) => {
                    record.lock().unwrap().push(value);
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
                None => {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                }
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn git_ls_remote_gets_the_token_through_the_helper_and_only_for_its_host() {
    let member = Member::new("cred-ls-remote", "");
    let (url, seen) = fake_github();
    member.set_env("BOATPARTY_RUNNER_CREDENTIAL_URL", &url);
    let command = format!(
        "GIT_TERMINAL_PROMPT=0 git ls-remote {url}/amxv/private.git 2>/dev/null; \
         echo ls-remote $?; {other}",
        other = hashed_fill("github.com"),
    );
    let (code, stdout, stderr) = member.cheers(&command, &with_token());
    assert_eq!(code, 0, "{stderr}");
    let seen = seen.lock().unwrap().clone();
    let expected = format!(
        "Basic {}",
        base64(format!("x-access-token:{TOKEN}").as_bytes())
    );
    assert!(!seen.is_empty(), "{stdout}{stderr}");
    assert!(
        seen.iter().all(|auth| *auth == expected),
        "{stdout}{stderr}"
    );
    assert!(stdout.starts_with("ls-remote "), "{stdout}");
    // github.com is not the helper's URL here, so it gets nothing.
    assert!(stdout.ends_with(&format!("{}\n", git_hash(""))), "{stdout}");
    assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
}

#[test]
fn the_token_reaches_steps_and_the_command_and_leaks_nowhere() {
    let member = Member::new(
        "cred-leaks",
        &format!(
            "[[ensure]]\nname = \"private-deps\"\nrun = \"{} >&2\"\n",
            hashed_fill("github.com")
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ),
    );
    let scan = "if [ -d /proc/self ]; then \
                  for f in /proc/[0-9]*/environ /proc/[0-9]*/cmdline; do \
                    tr '\\000' '\\n' < \"$f\" 2>/dev/null; done; fi; \
                ps axww -o args; env";
    let command = format!(
        "{github}; {other}; echo socket=$BOATPARTY_CREDENTIAL_SOCKET; {scan}",
        github = hashed_fill("github.com"),
        other = hashed_fill("example.com"),
    );
    let (code, stdout, stderr) = member.cheers(&command, &with_token());
    assert_eq!(code, 0, "{stderr}");
    let token_hash = git_hash(&format!("{TOKEN}\n"));
    let mut lines = stdout.lines();
    // The command got the token for github.com, and nothing for another host.
    assert_eq!(lines.next(), Some(token_hash.as_str()), "{stdout}");
    assert_eq!(lines.next(), Some(git_hash("").as_str()), "{stdout}");
    // So did the ensure step, before the command.
    assert!(stderr.contains(&token_hash), "{stderr}");
    // No environment or argument list held it during the command.
    assert!(!stdout.contains(TOKEN));
    assert!(!stderr.contains(TOKEN));
    let socket = stdout
        .lines()
        .find_map(|line| line.strip_prefix("socket="))
        .unwrap()
        .to_string();
    assert!(!socket.is_empty(), "{stdout}");

    // Afterwards the server and its socket are gone, and no file under the
    // member's checkout, state, HOME, TMPDIR or RAM directory holds it.
    assert!(!Path::new(&socket).exists());
    let ps = std::process::Command::new("ps")
        .args(["axww", "-o", "args"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&ps.stdout).contains(&socket));
    assert_no_token_under(member.root.path());
    assert!(
        fs::read_dir(member.root.path().join("shm"))
            .unwrap()
            .next()
            .is_none()
    );
    for args in member.host.spawned.borrow().iter() {
        assert!(!args.join(" ").contains(TOKEN));
    }

    // Without a token the command has no helper and no socket.
    let (code, stdout, _) = member.cheers(
        &format!(
            "{}; echo socket=${{BOATPARTY_CREDENTIAL_SOCKET:-none}}",
            hashed_fill("github.com")
        ),
        &RunCredentials::none(),
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, format!("{}\nsocket=none\n", git_hash("")));
}

#[test]
fn a_store_helper_on_the_member_never_receives_the_token() {
    let member = Member::new("cred-store", "");
    let home = member.root.path().join("home");
    fs::write(
        home.join(".gitconfig"),
        "[credential]\n\thelper = store\n[credential \"https://github.com\"]\n\thelper = store\n",
    )
    .unwrap();
    // A successful fill makes Git "approve" (store) the credential with every
    // helper still configured.
    let command = "printf 'protocol=https\\nhost=github.com\\n\\n' | git credential fill \
                   | git credential approve; echo done";
    let (code, stdout, stderr) = member.cheers(command, &with_token());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "done\n");
    assert!(!home.join(".git-credentials").exists());
    assert_no_token_under(member.root.path());
}
