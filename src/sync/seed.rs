//! Seeding a member's repository from the project's GitHub remote.
//!
//! A first sync of a large repository is dominated by sending objects that
//! GitHub already has. When the local repository has a GitHub remote and the
//! member holds nothing yet, the client picks commits the remote is known to
//! have (from the local remote-tracking refs), the runner fetches them on the
//! sandbox (shallow in `head` mode, with history in `full` mode), and the
//! pack then excludes everything reachable from them.
//!
//! Seeding never fails a sync: no remote, no access, a network failure or a
//! timeout leaves the normal pack path in charge, and the reason is reported.
//!
//! A private repository needs a token. It comes from `BOATPARTY_GITHUB_TOKEN`
//! or `gh auth token`, is read only when a seed is attempted, and travels on
//! the runner's stdin. The runner hands it to Git through a one-shot
//! credential helper reading a pipe, so it never appears in an argument
//! list, the remote URL, `.git/config` or any file.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{History, LocalRef, WorktreeInfo, git};
use crate::runner::{EXIT_MISSING, MISSING_MARKER, RunnerHost};

/// Enables `file://` (and absolute path) remotes as seed sources, for tests.
pub const ALLOW_FILE_ENV: &str = "BOATPARTY_SEED_ALLOW_FILE";
/// The token used instead of `gh auth token`.
pub const TOKEN_ENV: &str = "BOATPARTY_GITHUB_TOKEN";
/// Seed only when HEAD's files add up to at least this many bytes.
pub const MIN_BYTES_ENV: &str = "BOATPARTY_SEED_MIN_BYTES";
/// How long the sandbox's fetch may take, in seconds.
pub const TIMEOUT_ENV: &str = "BOATPARTY_SEED_TIMEOUT_SECONDS";

pub const DEFAULT_MIN_BYTES: u64 = 0;
/// A member that already holds the project seeds again only when what it
/// lacks of the seed commits is at least this large (bytes as stored).
pub const WARM_MIN_BYTES_ENV: &str = "BOATPARTY_SEED_WARM_MIN_BYTES";
/// A GitHub fetch costs a few seconds however small; a signed upload moves
/// about 4.5 MB/s from a fast connection, so seeding wins from about here.
pub const DEFAULT_WARM_MIN_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 180;

/// A secret string whose `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where a token for a private repository comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// Fetch anonymously.
    None,
    /// This token (from `BOATPARTY_GITHUB_TOKEN`, or a test).
    Fixed(Secret),
    /// `gh auth token`, run only when a seed is attempted.
    Gh,
}

impl TokenSource {
    /// `BOATPARTY_GITHUB_TOKEN` when set, else `gh auth token`; `None` when
    /// the project does not want a token. Nothing is read or run here.
    pub fn from_env(wanted: bool) -> Self {
        match std::env::var(TOKEN_ENV) {
            _ if !wanted => Self::None,
            Ok(value) if !value.trim().is_empty() => {
                Self::Fixed(Secret::new(value.trim().to_string()))
            }
            _ => Self::Gh,
        }
    }

    /// The token itself: `gh` runs only here, and its answer stays in memory.
    /// `None` when there is no source or no usable token.
    pub fn fetch(&self) -> Option<Secret> {
        match self {
            Self::None => None,
            Self::Fixed(secret) => Some(secret.clone()),
            Self::Gh => gh_token(),
        }
    }
}

/// The project's and environment's seeding settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedConfig {
    /// `[sync] seed` (default true).
    pub enabled: bool,
    /// Accept `file://` remotes (tests only).
    pub allow_file: bool,
    /// Seed only when HEAD's files add up to at least this many bytes.
    pub min_bytes: u64,
    /// Seed a member that already holds something only when the objects it
    /// lacks add up to at least this many bytes.
    pub warm_min_bytes: u64,
    pub timeout_secs: u64,
    pub token: TokenSource,
}

impl SeedConfig {
    /// The settings from `[sync] seed`, `[sync] github_token` and the
    /// environment knobs.
    pub fn from_env(enabled: bool, github_token: bool) -> Self {
        let number = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(default)
        };
        let token = TokenSource::from_env(github_token);
        Self {
            enabled,
            allow_file: std::env::var(ALLOW_FILE_ENV).is_ok_and(|value| value == "1"),
            min_bytes: number(MIN_BYTES_ENV, DEFAULT_MIN_BYTES),
            warm_min_bytes: number(WARM_MIN_BYTES_ENV, DEFAULT_WARM_MIN_BYTES),
            timeout_secs: number(TIMEOUT_ENV, DEFAULT_TIMEOUT_SECONDS).max(1),
            token,
        }
    }
}

/// How a sync's objects were seeded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SeedRoute {
    /// Not seeded.
    #[default]
    None,
    /// Fetched on the sandbox from the GitHub remote.
    Github,
}

impl SeedRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Github => "github",
        }
    }
}

/// What seeding did for one sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedReport {
    pub route: SeedRoute,
    /// Why an attempted seed failed (the pack carried everything instead).
    pub fallback: Option<String>,
    /// Why no seed was attempted, when one could have helped.
    pub skipped: Option<String>,
    /// The commits seeded.
    pub commits: Vec<String>,
    /// How long the seed took, in milliseconds.
    pub millis: u64,
    /// The runner was uploaded first.
    pub installed: bool,
}

impl SeedReport {
    /// How long the seed took, in seconds.
    pub fn seconds(&self) -> f64 {
        self.millis as f64 / 1000.0
    }
}

/// A seed to attempt: the remote's URL and the commits to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPlan {
    pub url: String,
    pub commits: Vec<String>,
}

/// The URL the sandbox fetches from, for a remote URL: any github.com form
/// (https, ssh, scp-like, git) as `https://github.com/<owner>/<repo>.git`,
/// without credentials. With `allow_file`, an absolute path or `file://`
/// URL too. Anything else is not a seed source.
pub fn seed_url(raw: &str, allow_file: bool) -> Option<String> {
    let raw = raw.trim();
    if allow_file {
        if let Some(path) = raw.strip_prefix("file://") {
            return path.starts_with('/').then(|| format!("file://{path}"));
        }
        if raw.starts_with('/') {
            return Some(format!("file://{raw}"));
        }
    }
    let rest = if let Some((scheme, rest)) = raw.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(
            scheme.as_str(),
            "https" | "http" | "ssh" | "git" | "git+ssh"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        // Credentials in the URL are dropped, never forwarded.
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com")
        {
            return None;
        }
        path.to_string()
    } else {
        // scp-like: [user@]github.com:owner/repo
        let (authority, path) = raw.split_once(':')?;
        let host = authority.rsplit('@').next()?;
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path.to_string()
    };
    let path = rest.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("https://github.com/{owner}/{repo}.git"))
}

/// What to seed for this sync, or why nothing is. The checkout's remote is
/// the one its branch tracks, else `origin`; the remote is known to have
/// whatever its remote-tracking refs reach.
///
/// `head` mode seeds one commit: HEAD when it is pushed, else the newest
/// pushed commit on HEAD's first-parent line. `full` mode seeds, for HEAD
/// and every mirrored ref, the pushed commits its unpushed history rests on.
pub fn plan(
    info: &WorktreeInfo,
    history: History,
    refs: &[LocalRef],
    config: &SeedConfig,
) -> Result<SeedPlan, String> {
    let root = &info.root;
    let quiet = |args: &[&str]| -> Option<String> {
        let output = git(root, args, &[], None, true).ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let tracked = info
        .branch
        .as_ref()
        .and_then(|branch| quiet(&["config", "--get", &format!("branch.{branch}.remote")]))
        .filter(|remote| !remote.is_empty() && remote != ".");
    let remote = tracked.unwrap_or_else(|| "origin".to_string());
    let Some(raw) = quiet(&["remote", "get-url", &remote]) else {
        return Err("the repository has no remote to seed from".to_string());
    };
    let Some(url) = seed_url(&raw, config.allow_file) else {
        return Err(format!("remote {remote} is not on github.com"));
    };
    if config.min_bytes > 0 {
        let size = head_bytes(root).unwrap_or(0);
        if size < config.min_bytes {
            return Err(format!(
                "HEAD's files ({size} bytes) are below the seeding threshold ({} bytes)",
                config.min_bytes
            ));
        }
    }
    let remotes = format!("--remotes={remote}");
    let commits = match history {
        History::Head => {
            let local_only = quiet(&["rev-list", "--first-parent", &info.head, "--not", &remotes])
                .ok_or("cannot list unpushed commits")?;
            match local_only.lines().last() {
                None => vec![info.head.clone()],
                Some(oldest) => {
                    let parent = quiet(&["rev-parse", "-q", "--verify", &format!("{oldest}^1")])
                        .filter(|sha| !sha.is_empty())
                        .ok_or_else(|| {
                            format!("no commit of HEAD is known to be on remote {remote}")
                        })?;
                    vec![parent]
                }
            }
        }
        History::Full => full_seeds(root, info, refs, &remotes)?,
    };
    if commits.is_empty() {
        return Err(format!("no commit is known to be on remote {remote}"));
    }
    Ok(SeedPlan { url, commits })
}

/// The pushed commits the unpushed history of HEAD and the mirrored refs
/// rests on, plus those of them that are pushed themselves, reduced to the
/// ones no other covers.
fn full_seeds(
    root: &Path,
    info: &WorktreeInfo,
    refs: &[LocalRef],
    remotes: &str,
) -> Result<Vec<String>, String> {
    let mut wanted = vec![info.head.clone()];
    wanted.extend(refs.iter().map(|entry| entry.sha.clone()));
    // Only commits: an annotated tag travels in the pack.
    let input: String = wanted.iter().map(|sha| format!("{sha}\n")).collect();
    let types = git(
        root,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        &[],
        Some(input.as_bytes()),
        true,
    )
    .map_err(|error| error.message().to_string())?;
    let mut commits: Vec<String> = String::from_utf8_lossy(&types.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(_, kind)| *kind == "commit")
        .map(|(sha, _)| sha.to_string())
        .collect();
    commits.dedup();
    let mut args = vec!["rev-list", "--boundary"];
    args.extend(commits.iter().map(String::as_str));
    args.extend(["--not", remotes]);
    let listed = git(root, &args, &[], None, true).map_err(|error| error.message().to_string())?;
    if !listed.status.success() {
        return Err("cannot list unpushed commits".to_string());
    }
    let text = String::from_utf8_lossy(&listed.stdout).to_string();
    let mut local_only = std::collections::BTreeSet::new();
    let mut seeds = Vec::new();
    for line in text.lines() {
        match line.strip_prefix('-') {
            Some(boundary) => seeds.push(boundary.to_string()),
            None => {
                local_only.insert(line.to_string());
            }
        }
    }
    for sha in &commits {
        if !local_only.contains(sha) && !seeds.contains(sha) {
            seeds.push(sha.clone());
        }
    }
    if seeds.len() > 1 {
        let mut args = vec!["merge-base", "--independent"];
        args.extend(seeds.iter().map(String::as_str));
        if let Ok(output) = git(root, &args, &[], None, true)
            && output.status.success()
        {
            seeds = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect();
        }
    }
    Ok(seeds)
}

/// The total size of HEAD's files.
fn head_bytes(root: &Path) -> Option<u64> {
    let output = git(
        root,
        &["ls-tree", "-r", "-l", "--full-tree", "HEAD"],
        &[],
        None,
        true,
    )
    .ok()?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split('\t').next()?.split_whitespace().nth(3))
            .filter_map(|size| size.parse::<u64>().ok())
            .sum(),
    )
}

/// How many bytes (as stored, compressed) the objects of `commits` take
/// that `known` does not reach: about what a pack would carry for them.
/// Known commits this repository lacks are ignored.
pub fn missing_bytes(root: &Path, commits: &[String], known: &[String]) -> Option<u64> {
    let mut args = vec!["rev-list", "--objects", "--ignore-missing"];
    args.extend(commits.iter().map(String::as_str));
    args.push("--not");
    args.extend(known.iter().map(String::as_str));
    let listed = git(root, &args, &[], None, true).ok()?;
    if !listed.status.success() {
        return None;
    }
    let names: String = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|sha| format!("{sha}\n"))
        .collect();
    if names.is_empty() {
        return Some(0);
    }
    let sizes = git(
        root,
        &["cat-file", "--batch-check=%(objectsize:disk)"],
        &[],
        Some(names.as_bytes()),
        true,
    )
    .ok()?;
    sizes.status.success().then(|| {
        String::from_utf8_lossy(&sizes.stdout)
            .lines()
            .filter_map(|size| size.trim().parse::<u64>().ok())
            .sum()
    })
}

/// The token for this seed, if any. `gh` runs only here, only for
/// github.com, and its answer stays in memory.
pub fn token(source: &TokenSource, url: &str) -> Option<Secret> {
    match source {
        // A file:// URL only exists when tests allow it.
        TokenSource::Gh
            if !url.starts_with("https://github.com/") && !url.starts_with("file://") =>
        {
            None
        }
        source => source.fetch(),
    }
}

fn gh_token() -> Option<Secret> {
    let mut child = Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    if !wait_until(&mut child, Instant::now() + Duration::from_secs(10)) {
        return None;
    }
    let text = reader.join().ok()?;
    let token = text.trim();
    let usable = !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic());
    usable.then(|| Secret::new(token.to_string()))
}

/// Wait for `child` until `deadline`; kill it after. True when it exited 0.
fn wait_until(child: &mut Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The runner's stdin for a seed: one `key value` line each, then an empty
/// line.
fn header(plan: &SeedPlan, token: Option<&Secret>, timeout_secs: u64) -> Vec<u8> {
    let mut text = format!("url {}\ntimeout {timeout_secs}\n", plan.url);
    for sha in &plan.commits {
        text.push_str(&format!("want {sha}\n"));
    }
    if let Some(token) = token.filter(|token| {
        !token.expose().is_empty() && token.expose().bytes().all(|b| b.is_ascii_graphic())
    }) {
        text.push_str(&format!("token {}\n", token.expose()));
    }
    text.push('\n');
    text.into_bytes()
}

/// Ask the runner to fetch the plan's commits on the sandbox. `Ok(installed)`
/// when every commit is there now; `Err(reason)` otherwise.
pub fn fetch(
    host: &dyn RunnerHost,
    project: &str,
    history: History,
    plan: &SeedPlan,
    token: Option<&Secret>,
    timeout_secs: u64,
) -> Result<bool, String> {
    let mut installed = false;
    loop {
        let nonce = crate::system::random_hex(8).map_err(|error| error.message().to_string())?;
        let args: Vec<String> = ["seed", project, &nonce, history.as_str()]
            .map(String::from)
            .to_vec();
        let (status, stdout, stderr) = invoke(
            host,
            &args,
            header(plan, token, timeout_secs),
            Duration::from_secs(timeout_secs + 30),
        )?;
        let stdout = String::from_utf8_lossy(&stdout).to_string();
        let stderr = String::from_utf8_lossy(&stderr).to_string();
        if status == Some(EXIT_MISSING) && stderr.starts_with(MISSING_MARKER) && !installed {
            host.install()
                .map_err(|error| format!("cannot install the runner: {}", error.message()))?;
            installed = true;
            continue;
        }
        let answer = stdout.lines().rev().find(|line| !line.trim().is_empty());
        return match (status, answer) {
            (Some(0), Some("seeded")) => Ok(installed),
            (Some(0), Some(line)) if line.starts_with("failed ") => {
                Err(clean(&line["failed ".len()..]))
            }
            (Some(255), _) => Err("the SSH connection failed".to_string()),
            (status, _) => Err(format!(
                "the runner exited {} without a result{}",
                status.map_or_else(|| "on a signal".to_string(), |code| code.to_string()),
                last_line(&stderr)
                    .map(|line| format!(": {}", clean(line)))
                    .unwrap_or_default()
            )),
        };
    }
}

/// A finished runner process: its exit code, stdout and stderr.
type Invoked = (Option<i32>, Vec<u8>, Vec<u8>);

/// Run the runner with `input` on stdin, collecting its output, killing it
/// after `limit`.
fn invoke(
    host: &dyn RunnerHost,
    args: &[String],
    input: Vec<u8>,
    limit: Duration,
) -> Result<Invoked, String> {
    let mut child = host
        .command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start the runner: {error}"))?;
    let collect = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = collect(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = collect(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input);
    }
    // The input (which may hold the token) is dropped here, not kept.
    drop(input);
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the sandbox's fetch took longer than {}s",
                    limit.as_secs()
                ));
            }
        }
    };
    // A multiplexed ssh client can leave its pipes open through the master;
    // whatever has arrived by now is enough.
    let join = |handle: std::thread::JoinHandle<Vec<u8>>| {
        let until = Instant::now() + Duration::from_millis(1500);
        while !handle.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        if handle.is_finished() {
            handle.join().unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    Ok((status, join(stdout), join(stderr)))
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().rev().find(|line| !line.trim().is_empty())
}

/// One printable line of at most 200 characters.
fn clean(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    match line.char_indices().nth(200) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_remotes_normalize_to_https_without_credentials() {
        let want = Some("https://github.com/amxv/boatparty.git".to_string());
        for raw in [
            "https://github.com/amxv/boatparty.git",
            "https://github.com/amxv/boatparty",
            "https://github.com/amxv/boatparty/",
            "https://user:ghp_secret@github.com/amxv/boatparty.git",
            "http://GitHub.com/amxv/boatparty.git",
            "git@github.com:amxv/boatparty.git",
            "github.com:amxv/boatparty",
            "ssh://git@github.com/amxv/boatparty.git",
            "ssh://git@github.com:22/amxv/boatparty",
            "git://github.com/amxv/boatparty.git",
        ] {
            assert_eq!(seed_url(raw, false), want, "{raw}");
        }
        for raw in [
            "https://gitlab.com/amxv/boatparty.git",
            "https://github.com.evil.example/amxv/boatparty.git",
            "git@example.com:amxv/boatparty.git",
            "https://github.com/amxv",
            "https://github.com/amxv/boatparty/extra",
            "https://github.com/amxv/bo at",
            "https://github.com/../x",
            "/srv/git/repo.git",
            "file:///srv/git/repo.git",
            "ext::sh -c touch% /tmp/x",
            "github-work:amxv/boatparty",
        ] {
            assert_eq!(seed_url(raw, false), None, "{raw}");
        }
        assert_eq!(
            seed_url("/srv/git/repo.git", true).as_deref(),
            Some("file:///srv/git/repo.git")
        );
        assert_eq!(
            seed_url("file:///srv/git/repo.git", true).as_deref(),
            Some("file:///srv/git/repo.git")
        );
        assert_eq!(seed_url("file://relative", true), None);
    }

    #[test]
    fn secrets_do_not_print() {
        let secret = Secret::new("ghp_dont_show");
        assert!(!format!("{secret:?}").contains("ghp_dont_show"));
        let config = SeedConfig {
            enabled: true,
            allow_file: false,
            min_bytes: 0,
            warm_min_bytes: 0,
            timeout_secs: 1,
            token: TokenSource::Fixed(secret),
        };
        assert!(!format!("{config:?}").contains("ghp_dont_show"));
    }

    #[test]
    fn the_header_carries_the_token_only_when_given() {
        let plan = SeedPlan {
            url: "https://github.com/a/b.git".to_string(),
            commits: vec!["a".repeat(40)],
        };
        let without = String::from_utf8(header(&plan, None, 9)).unwrap();
        assert_eq!(
            without,
            format!(
                "url https://github.com/a/b.git\ntimeout 9\nwant {}\n\n",
                "a".repeat(40)
            )
        );
        let with = String::from_utf8(header(&plan, Some(&Secret::new("t0k")), 9)).unwrap();
        assert!(with.ends_with("token t0k\n\n"));
    }

    #[test]
    fn reasons_are_one_short_printable_line() {
        assert_eq!(clean("fatal: a\rb\n"), "fatal: a b");
        assert_eq!(clean(&"x".repeat(300)).len(), 203);
    }
}
