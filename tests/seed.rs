//! Seeding a member's first sync from the project's GitHub remote, with a
//! local bare repository over file:// standing in for GitHub, and the
//! runner's repository maintenance. The runner runs locally.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use boatparty::runner::RunnerHost;
use boatparty::sync::pack::usable_known;
use boatparty::sync::seed::{Secret, SeedConfig, TokenSource};
use boatparty::sync::{self, History, MirrorOutcome, MirrorRequest, SeedRoute, WorktreeInfo};
use common::TempDir;
use common::runner::{LocalRunner, base_repo, git, write};

const TOKEN: &str = "ghp_seedtesttoken_do_not_leak_8d1f";

#[derive(Default)]
struct Member {
    mode: String,
    known: Vec<String>,
    digest: Option<String>,
}

fn config() -> SeedConfig {
    SeedConfig {
        enabled: true,
        allow_file: true,
        min_bytes: 0,
        warm_min_bytes: u64::MAX,
        timeout_secs: 30,
        token: TokenSource::None,
    }
}

fn load(
    local: &Path,
    host: &LocalRunner,
    member: &mut Member,
    history: History,
    seed: Option<&SeedConfig>,
) -> (MirrorOutcome, String) {
    let info = WorktreeInfo::read(local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let (refs, digest) = sync::mirrored_refs(&info, history).unwrap();
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: member.digest.as_deref() != Some(digest.as_str()),
        known: usable_known(history, &member.mode, &member.known),
        history,
        seed,
    };
    let mut log = Vec::new();
    let outcome = sync::mirror(host, &request, &mut log)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&log)));
    assert_eq!(outcome.tree, snapshot.tree);
    member.mode = history.as_str().to_string();
    member.known = outcome.known.clone();
    member.digest = Some(digest);
    (outcome, String::from_utf8_lossy(&log).to_string())
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
}

/// Bytes that do not compress, so pack sizes reflect what is sent.
fn noise(seed: u64, len: usize) -> String {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

/// A local repository with history and large files, pushed to a bare
/// "GitHub" at `root/github.git` that `origin` points at, with its
/// remote-tracking refs fetched.
fn pushed_repo(root: &Path) -> (PathBuf, PathBuf) {
    let local = base_repo(root);
    for n in 0..4 {
        write(&local.join(format!("big{n}.txt")), &noise(n, 64 * 1024));
        commit(&local, &format!("big {n}"));
    }
    git(&local, &["tag", "v1", "HEAD~1"]);
    git(&local, &["branch", "side", "HEAD~2"]);
    let bare = root.join("github.git");
    git(root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&local, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&local, &["push", "-q", "origin", "main", "side", "v1"]);
    git(&local, &["fetch", "-q", "origin"]);
    git(
        &local,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    );
    (local, bare)
}

fn assert_head_mirror(local: &Path, remote: &Path) {
    assert_eq!(
        git(remote, &["rev-parse", "HEAD"]),
        git(local, &["rev-parse", "HEAD"])
    );
    assert_eq!(porcelain(remote), porcelain(local));
    assert_eq!(git(remote, &["log", "--oneline"]).lines().count(), 1);
    git(remote, &["fsck", "--no-dangling", "--no-progress"]);
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

#[test]
fn a_first_head_sync_seeds_from_the_remote_and_sends_only_local_objects() {
    let root = TempDir::new("seed-head");
    let (local, _) = pushed_repo(root.path());
    // An unpushed commit and working-tree edits on top.
    write(&local.join("local.txt"), "not pushed\n");
    let unpushed = commit(&local, "local only");
    write(&local.join("a.txt"), "edited\n");
    write(&local.join("untracked.txt"), "new\n");

    let plain = LocalRunner::new(root.path(), "plain");
    let (unseeded, _) = load(&local, &plain, &mut Member::default(), History::Head, None);
    assert_eq!(unseeded.seed.route, SeedRoute::None);

    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (seeded, log) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(seeded.seed.route, SeedRoute::Github, "{log}");
    assert_eq!(
        seeded.seed.commits,
        vec![git(&local, &["rev-parse", "HEAD~1"])]
    );
    assert!(
        seeded.pack_bytes * 20 < unseeded.pack_bytes,
        "seeded {} vs unseeded {}",
        seeded.pack_bytes,
        unseeded.pack_bytes
    );
    assert!(!seeded.full_retry);
    assert_head_mirror(&local, &host.repo);
    assert_eq!(git(&host.repo, &["rev-parse", "HEAD"]), unpushed);
    // The known set is what the sandbox holds for sure, and the next sync
    // builds on it without seeding again.
    assert_eq!(
        member.known,
        vec![seeded_wip(&local), git(&local, &["rev-parse", "HEAD"])]
    );
    write(&local.join("a.txt"), "edited again\n");
    let (next, _) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(next.seed.route, SeedRoute::None);
    assert!(!next.full_retry);
    assert_head_mirror(&local, &host.repo);
    // The remote URL never lands in the configuration.
    let config_text = fs::read_to_string(host.repo.join(".git/config")).unwrap();
    assert!(!config_text.contains("github.git"), "{config_text}");
}

fn seeded_wip(local: &Path) -> String {
    let info = WorktreeInfo::read(local).unwrap();
    sync::snapshot(&info).unwrap().wip
}

#[test]
fn a_pushed_head_is_itself_the_seed() {
    let root = TempDir::new("seed-pushed");
    let (local, _) = pushed_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (outcome, log) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(outcome.seed.route, SeedRoute::Github, "{log}");
    assert_eq!(
        outcome.seed.commits,
        vec![git(&local, &["rev-parse", "HEAD"])]
    );
    // Only the snapshot commit is new.
    assert!(outcome.pack_bytes < 1024, "{}", outcome.pack_bytes);
    assert_head_mirror(&local, &host.repo);
}

#[test]
fn a_full_sync_seeds_the_mirrored_refs_with_history() {
    let root = TempDir::new("seed-full");
    let (local, _) = pushed_repo(root.path());
    write(&local.join("local.txt"), "not pushed\n");
    commit(&local, "local only");
    git(&local, &["tag", "local-tag"]);

    let plain = LocalRunner::new(root.path(), "plain");
    let (unseeded, _) = load(&local, &plain, &mut Member::default(), History::Full, None);
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (seeded, log) = load(&local, &host, &mut member, History::Full, Some(&config()));
    assert_eq!(seeded.seed.route, SeedRoute::Github, "{log}");
    assert!(seeded.pack_bytes * 20 < unseeded.pack_bytes);
    let remote = &host.repo;
    for name in ["HEAD", "side", "v1", "local-tag", "origin/main"] {
        assert_eq!(
            git(remote, &["rev-parse", name]),
            git(&local, &["rev-parse", name]),
            "{name}"
        );
    }
    assert_eq!(
        git(remote, &["rev-list", "--count", "HEAD", "side", "v1"]),
        git(&local, &["rev-list", "--count", "HEAD", "side", "v1"])
    );
    assert_eq!(
        git(remote, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    git(remote, &["fsck", "--no-dangling", "--no-progress"]);
    assert_eq!(porcelain(remote), porcelain(&local));
}

#[test]
fn a_switch_from_head_to_full_deepens_through_the_seed() {
    let root = TempDir::new("seed-deepen");
    let (local, _) = pushed_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_head_mirror(&local, &host.repo);
    let (full, log) = load(&local, &host, &mut member, History::Full, Some(&config()));
    assert_eq!(full.seed.route, SeedRoute::Github, "{log}");
    assert_eq!(
        git(&host.repo, &["rev-list", "--count", "HEAD"]),
        git(&local, &["rev-list", "--count", "HEAD"])
    );
    assert_eq!(
        git(&host.repo, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    git(&host.repo, &["fsck", "--no-dangling", "--no-progress"]);
}

#[test]
fn an_unreachable_remote_falls_back_to_the_full_pack() {
    let root = TempDir::new("seed-unreachable");
    let (local, bare) = pushed_repo(root.path());
    fs::remove_dir_all(&bare).unwrap();
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (outcome, log) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(outcome.seed.route, SeedRoute::None);
    let reason = outcome.seed.fallback.clone().expect("a fallback reason");
    assert!(!reason.is_empty());
    assert_eq!(log.matches("boatparty: seeding from").count(), 1, "{log}");
    assert!(outcome.pack_bytes > 100 * 1024, "{}", outcome.pack_bytes);
    assert_head_mirror(&local, &host.repo);
}

#[test]
fn seeding_is_skipped_without_a_github_remote_or_when_disabled() {
    let root = TempDir::new("seed-skip");
    let (local, _) = pushed_repo(root.path());

    let off = SeedConfig {
        enabled: false,
        ..config()
    };
    let host = LocalRunner::new(root.path(), "off");
    let (outcome, _) = load(
        &local,
        &host,
        &mut Member::default(),
        History::Head,
        Some(&off),
    );
    assert_eq!(outcome.seed.route, SeedRoute::None);
    assert!(
        outcome
            .seed
            .skipped
            .as_deref()
            .unwrap()
            .contains("seed = false")
    );
    assert!(outcome.seed.fallback.is_none());
    assert_eq!(
        host.spawned
            .borrow()
            .iter()
            .filter(|a| a[0] == "seed")
            .count(),
        0
    );

    // Production only seeds from github.com.
    let strict = SeedConfig {
        allow_file: false,
        ..config()
    };
    let host = LocalRunner::new(root.path(), "strict");
    let (outcome, _) = load(
        &local,
        &host,
        &mut Member::default(),
        History::Head,
        Some(&strict),
    );
    assert_eq!(outcome.seed.route, SeedRoute::None);
    assert!(
        outcome
            .seed
            .skipped
            .as_deref()
            .unwrap()
            .contains("github.com")
    );

    let threshold = SeedConfig {
        min_bytes: 1 << 30,
        ..config()
    };
    let host = LocalRunner::new(root.path(), "small");
    let (outcome, _) = load(
        &local,
        &host,
        &mut Member::default(),
        History::Head,
        Some(&threshold),
    );
    assert!(
        outcome
            .seed
            .skipped
            .as_deref()
            .unwrap()
            .contains("threshold")
    );

    git(&local, &["remote", "remove", "origin"]);
    let host = LocalRunner::new(root.path(), "none");
    let (outcome, _) = load(
        &local,
        &host,
        &mut Member::default(),
        History::Head,
        Some(&config()),
    );
    assert!(
        outcome
            .seed
            .skipped
            .as_deref()
            .unwrap()
            .contains("no remote")
    );
    assert_head_mirror(&local, &host.repo);
}

/// A `git` in front of the real one that records every argument list and,
/// for a fetch, asks the configured credential helpers for a password the
/// way an HTTPS fetch would, recording only the password's hash.
fn git_spy(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let bin = dir.join("spy-bin");
    fs::create_dir_all(&bin).unwrap();
    let log = dir.join("git-argv.log");
    let hashes = dir.join("password-hashes.log");
    let script = format!(
        "#!/usr/bin/env bash\n\
         printf '%s\\n' \"$*\" >> '{log}'\n\
         configs=()\n\
         args=(\"$@\")\n\
         i=0\n\
         while [ $i -lt ${{#args[@]}} ]; do\n\
           a=\"${{args[$i]}}\"\n\
           if [ \"$a\" = -c ]; then i=$((i+1)); configs+=(-c \"${{args[$i]}}\");\n\
           elif [ \"$a\" = fetch ]; then\n\
             p=$(printf 'protocol=https\\nhost=github.com\\npath=a/b.git\\n\\n' | GIT_TERMINAL_PROMPT=0 '{real}' -C / \"${{configs[@]}}\" credential fill 2>/dev/null | sed -n 's/^password=//p')\n\
             printf '%s' \"$p\" | '{real}' -C / hash-object --stdin >> '{hashes}'\n\
             break\n\
           fi\n\
           i=$((i+1))\n\
         done\n\
         exec '{real}' \"$@\"\n",
        log = log.display(),
        hashes = hashes.display(),
    );
    let path = bin.join("git");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn hash(text: &str) -> String {
    let mut child = common::isolated("git")
        .args(["-C", "/", "hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
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

#[test]
fn the_token_reaches_the_credential_helper_and_nothing_else() {
    let root = TempDir::new("seed-token");
    let (local, _) = pushed_repo(root.path());
    let spy = git_spy(root.path());
    let home = root.path().join("sandbox-home");
    fs::create_dir_all(&home).unwrap();
    // A sandbox whose Git would store credentials: the seed must switch it off.
    fs::write(home.join(".gitconfig"), "[credential]\n\thelper = store\n").unwrap();
    let host = LocalRunner::new(root.path(), "remote");
    host.env.borrow_mut().extend([
        (
            "PATH".to_string(),
            format!("{}:{}", spy.display(), std::env::var("PATH").unwrap()),
        ),
        ("HOME".to_string(), home.display().to_string()),
    ]);
    let config = SeedConfig {
        token: TokenSource::Fixed(Secret::new(TOKEN)),
        ..config()
    };
    let mut member = Member::default();
    let (outcome, log) = load(&local, &host, &mut member, History::Head, Some(&config));
    assert_eq!(outcome.seed.route, SeedRoute::Github, "{log}");
    assert_head_mirror(&local, &host.repo);

    // The helper handed Git the token, once.
    let hashes = fs::read_to_string(root.path().join("password-hashes.log")).unwrap();
    assert_eq!(
        hashes.lines().collect::<Vec<_>>(),
        vec![hash(TOKEN).as_str()]
    );
    // No argument list carried it: not the runner's, not any git's.
    for args in host.spawned.borrow().iter() {
        assert!(!args.join(" ").contains(TOKEN), "{args:?}");
    }
    let argv = fs::read_to_string(root.path().join("git-argv.log")).unwrap();
    assert!(argv.contains("fetch"), "{argv}");
    assert!(!argv.contains(TOKEN));
    // Nothing on the "sandbox" holds it: repository, state, home.
    assert_no_token_under(&host.repo);
    assert_no_token_under(&host.state);
    assert_no_token_under(&home);
    assert!(!home.join(".git-credentials").exists());
    assert!(!log.contains(TOKEN));
    assert!(!format!("{outcome:?}").contains(TOKEN));
}

fn pack_count(repo: &Path) -> usize {
    fs::read_dir(repo.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
        .count()
}

#[test]
fn maintenance_bounds_the_pack_count_and_keeps_a_shallow_repository_whole() {
    let root = TempDir::new("seed-maintain");
    let (local, _) = pushed_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    host.env
        .borrow_mut()
        .push(("BOATPARTY_RUNNER_MAX_PACKS".to_string(), "4".to_string()));
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head, Some(&config()));
    for n in 0..12 {
        write(
            &local.join("a.txt"),
            &format!("edit {n}\n{}\n", noise(100 + n, 2048)),
        );
        if n % 3 == 0 {
            commit(&local, &format!("step {n}"));
        }
        load(&local, &host, &mut member, History::Head, Some(&config()));
    }
    // The detached maintenance catches up shortly after the last sync.
    let deadline = Instant::now() + Duration::from_secs(30);
    while (pack_count(&host.repo) > 4 || !host.state.join("maintained").exists())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(host.state.join("maintained").exists());
    // Run once more in the foreground, so nothing is still rewriting packs.
    let status = host
        .command(&["maintain".to_string(), "app".to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(pack_count(&host.repo) <= 4, "{}", pack_count(&host.repo));
    assert_head_mirror(&local, &host.repo);
    git(&host.repo, &["fsck", "--no-progress"]);
    assert_eq!(
        git(&host.repo, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    // Syncing on top of the maintained repository still works incrementally.
    write(&local.join("a.txt"), "after maintenance\n");
    let (next, _) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert!(!next.full_retry);
    assert_head_mirror(&local, &host.repo);
}

#[test]
fn the_daily_prune_drops_old_snapshots_and_keeps_what_the_client_counts_on() {
    let root = TempDir::new("seed-prune");
    let (local, _) = pushed_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    // Syncs never start a background prune here; the test runs it.
    host.env.borrow_mut().extend([
        (
            "BOATPARTY_RUNNER_PRUNE_MINUTES".to_string(),
            "100000".to_string(),
        ),
        ("BOATPARTY_RUNNER_MAX_PACKS".to_string(), "1000".to_string()),
    ]);
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head, Some(&config()));
    for n in 0..8 {
        write(
            &local.join("a.txt"),
            &format!("edit {n}\n{}\n", noise(300 + n, 4096)),
        );
        if n % 2 == 0 {
            commit(&local, &format!("step {n}"));
        }
        load(&local, &host, &mut member, History::Head, Some(&config()));
    }
    let all = |repo: &Path| {
        git(
            repo,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
        )
        .lines()
        .count()
    };
    let reachable = |repo: &Path| {
        git(repo, &["rev-list", "--objects", "--all"])
            .lines()
            .count()
    };
    let before = all(&host.repo);
    assert!(before > reachable(&host.repo), "nothing to prune");
    // The first sync started the clock; two days later a prune is due. No
    // grace period here.
    let stamp = host.state.join("pruned-app");
    assert!(stamp.exists());
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 86_400);
    fs::File::options()
        .write(true)
        .open(&stamp)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    let status = host
        .command(&["maintain".to_string(), "app".to_string()])
        .env("BOATPARTY_RUNNER_PRUNE_GRACE", "now")
        .env("BOATPARTY_RUNNER_PRUNE_MINUTES", "1440")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        fs::metadata(&stamp).unwrap().modified().unwrap() > two_days_ago,
        "the prune did not run"
    );
    let after = all(&host.repo);
    assert!(after < before, "{after} objects, {before} before");
    assert_eq!(after, reachable(&host.repo));
    git(&host.repo, &["fsck", "--no-progress"]);
    // Every remaining shallow boundary names a commit that is still there.
    let shallow = fs::read_to_string(host.repo.join(".git/shallow")).unwrap();
    for sha in shallow.lines() {
        git(
            &host.repo,
            &["cat-file", "-e", &format!("{sha}^{{commit}}")],
        );
    }
    assert_head_mirror(&local, &host.repo);
    // The next sync is still thin: what the client knows is still there.
    write(&local.join("a.txt"), "after the prune\n");
    let (next, _) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert!(!next.full_retry);
    assert_head_mirror(&local, &host.repo);
}

#[test]
fn a_warm_member_is_seeded_again_only_when_a_large_pull_would_travel() {
    let root = TempDir::new("seed-warm");
    let (local, bare) = pushed_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (first, log) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(first.seed.route, SeedRoute::Github, "{log}");

    // Someone else pushes large commits; this checkout pulls them.
    let other = root.path().join("other");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "-b",
            "main",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["config", "user.name", "Other"]);
    git(&other, &["config", "user.email", "other@example.com"]);
    let push_large = |n: u64| {
        write(
            &other.join(format!("pulled{n}.txt")),
            &noise(100 + n, 256 * 1024),
        );
        commit(&other, &format!("pulled {n}"));
        git(&other, &["push", "-q", "origin", "main"]);
        git(&local, &["pull", "-q", "--ff-only"]);
    };

    // Below the threshold: the pack carries the pull, as before.
    push_large(1);
    let (small, log) = load(&local, &host, &mut member, History::Head, Some(&config()));
    assert_eq!(small.seed.route, SeedRoute::None, "{log}");
    assert!(small.pack_bytes > 100 * 1024, "{}", small.pack_bytes);
    assert_head_mirror(&local, &host.repo);

    // At the threshold: the sandbox fetches the pull from GitHub and the
    // pack carries only the snapshot.
    let warm = SeedConfig {
        warm_min_bytes: 64 * 1024,
        ..config()
    };
    push_large(2);
    write(&local.join("a.txt"), "local edit\n");
    let (seeded, log) = load(&local, &host, &mut member, History::Head, Some(&warm));
    assert_eq!(seeded.seed.route, SeedRoute::Github, "{log}");
    assert_eq!(
        seeded.seed.commits,
        vec![git(&local, &["rev-parse", "HEAD"])]
    );
    assert!(seeded.pack_bytes < 8 * 1024, "{}", seeded.pack_bytes);
    assert!(!seeded.full_retry);
    assert_head_mirror(&local, &host.repo);

    // HEAD did not move: nothing is looked at, nothing is seeded.
    write(&local.join("a.txt"), "another local edit\n");
    let (next, _) = load(&local, &host, &mut member, History::Head, Some(&warm));
    assert_eq!(next.seed.route, SeedRoute::None);
    assert!(!next.full_retry);
    assert_head_mirror(&local, &host.repo);
}
