//! The Git mirror end to end, with the runner run locally against scratch
//! repositories instead of over SSH.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use boatparty::runner::{RunnerHost, Trailer, TrailerFilter};
use boatparty::sync::{self, History, MirrorOutcome, MirrorRequest, WorktreeInfo};
use common::runner::{LocalRunner, base_repo, git, write};
use common::{TempDir, boatparty};

/// What the client keeps between syncs: the member's sync record.
#[derive(Default)]
struct Member {
    known: Vec<String>,
    digest: Option<String>,
}

fn load(local: &Path, host: &LocalRunner, member: &mut Member) -> (MirrorOutcome, String) {
    let info = WorktreeInfo::read(local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let refs = sync::local_refs(&info).unwrap();
    let digest = sync::refs_digest(&refs);
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: member.digest.as_deref() != Some(digest.as_str()),
        known: &member.known,
        history: History::Full,
        seed: None,
    };
    let mut log = Vec::new();
    let outcome = sync::mirror(host, &request, &mut log).unwrap();
    assert_eq!(outcome.tree, snapshot.tree);
    member.known = outcome.known.clone();
    member.digest = Some(digest);
    (outcome, String::from_utf8(log).unwrap())
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
}

#[test]
fn first_sync_mirrors_the_worktree_head_branch_and_default_remote_branch() {
    let root = TempDir::new("sync-first");
    let local = base_repo(root.path());
    write(&local.join("untracked.txt"), "new\n");
    fs::remove_file(local.join("a.txt")).unwrap();
    write(&local.join("dir/b.txt"), "two\nedited\n");
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();

    let (outcome, log) = load(&local, &host, &mut member);
    assert!(log.is_empty(), "unexpected runner output: {log}");
    assert!(outcome.pack_bytes > 0);
    assert!(!outcome.full_retry);
    let remote = &host.repo;
    assert_eq!(
        fs::read_to_string(remote.join("dir/b.txt")).unwrap(),
        "two\nedited\n"
    );
    assert_eq!(
        fs::read_to_string(remote.join("untracked.txt")).unwrap(),
        "new\n"
    );
    assert!(!remote.join("a.txt").exists());
    assert!(!remote.join("ignored").exists());
    assert_eq!(
        git(remote, &["rev-parse", "HEAD"]),
        git(&local, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(remote, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(
        git(remote, &["rev-parse", "refs/remotes/origin/main"]),
        git(&local, &["rev-parse", "refs/remotes/origin/main"])
    );
    assert_eq!(porcelain(remote), porcelain(&local));
    assert!(!porcelain(remote).is_empty());
    assert_eq!(
        git(remote, &["rev-parse", "refs/boatparty/wip^{tree}"]),
        outcome.tree
    );
    let head = git(&local, &["rev-parse", "HEAD"]);
    assert!(member.known.contains(&head));
}

#[test]
fn incremental_sync_writes_only_changed_paths_and_mirrors_edits_adds_and_deletes() {
    let root = TempDir::new("sync-incremental");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let (first, _) = load(&local, &host, &mut member);
    let remote = host.repo.clone();

    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let untouched = remote.join(".gitignore");
    fs::File::options()
        .write(true)
        .open(&untouched)
        .unwrap()
        .set_modified(old)
        .unwrap();
    write(&local.join("a.txt"), "one\nedited\n");
    write(&local.join("new.txt"), "untracked\n");
    fs::remove_file(local.join("dir/b.txt")).unwrap();
    write(&local.join("dir/c.txt"), "three\n");

    let (second, _) = load(&local, &host, &mut member);
    assert!(second.pack_bytes > 0);
    assert!(
        second.pack_bytes < first.pack_bytes,
        "the second pack should be thin"
    );
    assert_eq!(
        fs::read_to_string(remote.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    assert_eq!(
        fs::read_to_string(remote.join("new.txt")).unwrap(),
        "untracked\n"
    );
    assert!(!remote.join("dir/b.txt").exists());
    assert_eq!(
        fs::read_to_string(remote.join("dir/c.txt")).unwrap(),
        "three\n"
    );
    assert_eq!(
        fs::metadata(&untouched).unwrap().modified().unwrap(),
        old,
        "an unchanged file was rewritten"
    );
    assert_eq!(porcelain(&remote), porcelain(&local));

    // Nothing changed: the snapshot commit is reused and no pack is sent.
    let (third, _) = load(&local, &host, &mut member);
    assert_eq!(third.pack_bytes, 0);
    assert_eq!(third.tree, second.tree);
}

#[test]
fn a_thin_pack_whose_base_the_sandbox_lacks_answers_needfull_and_the_client_recovers() {
    let root = TempDir::new("sync-needfull");
    let local = base_repo(root.path());
    write(&local.join("a.txt"), "changed\n");
    let info = WorktreeInfo::read(&local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();

    // The raw runner: a pack that assumes HEAD, sent to an empty sandbox.
    let mut pack_objects = Command::new("git")
        .args(["pack-objects", "--stdout", "--revs", "--thin", "-q"])
        .current_dir(&local)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        pack_objects.stdin.as_mut().unwrap(),
        format!("{}\n^{}\n", snapshot.wip, info.head).as_bytes(),
    )
    .unwrap();
    let pack = pack_objects.wait_with_output().unwrap().stdout;
    let empty = LocalRunner::new(root.path(), "empty");
    let mut child = empty
        .command(&[
            "sync".into(),
            "app".into(),
            "nthin".into(),
            "1".into(),
            snapshot.wip.clone(),
            info.head.clone(),
            "main".into(),
            "keep".into(),
            "full".into(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = std::io::Write::write_all(&mut stdin, b"\n");
    let _ = std::io::Write::write_all(&mut stdin, &pack);
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(97));
    let mut filter = TrailerFilter::new("nthin");
    filter.push(&output.stderr);
    assert_eq!(filter.result(), Some(Trailer::NeedFull));

    // Through the client: it believes the sandbox has HEAD, is told
    // otherwise, and sends a full pack once.
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member {
        known: vec![info.head.clone()],
        digest: None,
    };
    let (outcome, _) = load(&local, &host, &mut member);
    assert!(outcome.full_retry);
    assert_eq!(
        fs::read_to_string(host.repo.join("a.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(porcelain(&host.repo), porcelain(&local));
}

#[test]
fn other_local_branches_and_tags_are_mirrored_with_their_history_and_pruned() {
    let root = TempDir::new("sync-refs");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member);

    git(&local, &["checkout", "-q", "-b", "side"]);
    write(&local.join("side.txt"), "side only\n");
    git(&local, &["add", "side.txt"]);
    git(&local, &["commit", "-qm", "side work"]);
    let side = git(&local, &["rev-parse", "HEAD"]);
    git(&local, &["tag", "-a", "v9.9.9", "-m", "release", "HEAD"]);
    git(&local, &["tag", "doomed", "HEAD"]);
    git(&local, &["checkout", "-q", "main"]);

    let info = WorktreeInfo::read(&local).unwrap();
    let names: Vec<String> = sync::local_refs(&info)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        names,
        [
            "refs/heads/side",
            "refs/tags/doomed",
            "refs/tags/v9.9.9",
            "refs/remotes/origin/main"
        ]
    );
    load(&local, &host, &mut member);
    let remote = &host.repo;
    assert_eq!(git(remote, &["rev-parse", "refs/heads/side"]), side);
    assert_eq!(git(remote, &["rev-parse", "v9.9.9^{commit}"]), side);
    assert_eq!(git(remote, &["cat-file", "-t", "v9.9.9"]), "tag");
    assert_eq!(git(remote, &["show", "v9.9.9:side.txt"]), "side only");
    assert_eq!(git(remote, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert!(!remote.join("side.txt").exists());

    // A deleted local tag disappears; unchanged refs are not re-sent.
    git(&local, &["tag", "-d", "doomed"]);
    load(&local, &host, &mut member);
    assert_eq!(
        git(remote, &["tag", "--list"]),
        "v9.9.9",
        "the deleted tag is still mirrored"
    );
    let digest = member.digest.clone();
    load(&local, &host, &mut member);
    assert_eq!(member.digest, digest);

    // Switching branches moves HEAD and keeps the other branch.
    git(&local, &["checkout", "-q", "side"]);
    load(&local, &host, &mut member);
    assert_eq!(git(remote, &["symbolic-ref", "--short", "HEAD"]), "side");
    assert_eq!(
        git(remote, &["rev-parse", "refs/heads/main"]),
        git(&local, &["rev-parse", "refs/heads/main"])
    );
    assert_eq!(
        fs::read_to_string(remote.join("side.txt")).unwrap(),
        "side only\n"
    );
    assert_eq!(porcelain(remote), porcelain(&local));
}

/// Every file under `dir` (except `.git/objects`) with its bytes and mtime.
fn tree_listing(dir: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(dir).unwrap().to_path_buf();
            if relative.starts_with(".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                let meta = fs::metadata(&path).unwrap();
                out.push((relative, fs::read(&path).unwrap(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_real_index_and_working_tree_are_never_touched() {
    let root = TempDir::new("sync-untouched");
    let local = base_repo(root.path());
    write(&local.join("a.txt"), "unstaged edit\n");
    write(&local.join("staged.txt"), "staged\n");
    git(&local, &["add", "staged.txt"]);
    write(&local.join("loose.txt"), "untracked\n");
    // Let `git status` refresh the index once, so the comparison below sees
    // only what the syncs might do.
    let status = porcelain(&local);
    let index = local.join(".git/index");
    let index_bytes = fs::read(&index).unwrap();
    let index_mtime = fs::metadata(&index).unwrap().modified().unwrap();
    let files = tree_listing(&local);

    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member);
    load(&local, &host, &mut member);

    assert_eq!(tree_listing(&local), files);
    assert_eq!(fs::read(&index).unwrap(), index_bytes);
    assert_eq!(
        fs::metadata(&index).unwrap().modified().unwrap(),
        index_mtime
    );
    assert_eq!(porcelain(&local), status);
    assert_eq!(
        git(&local, &["diff", "--cached", "--name-only"]),
        "staged.txt"
    );
}

#[test]
fn the_snapshot_leaves_the_real_index_byte_for_byte_unchanged() {
    let root = TempDir::new("sync-index");
    let local = base_repo(root.path());
    write(&local.join("a.txt"), "edit\n");
    write(&local.join("loose.txt"), "untracked\n");
    let index = local.join(".git/index");
    let before = fs::read(&index).unwrap();
    let before_mtime = fs::metadata(&index).unwrap().modified().unwrap();
    let info = WorktreeInfo::read(&local).unwrap();
    sync::snapshot(&info).unwrap();
    write(&local.join("ignored/later.txt"), "x\n");
    sync::snapshot(&info).unwrap();
    assert_eq!(fs::read(&index).unwrap(), before);
    assert_eq!(
        fs::metadata(&index).unwrap().modified().unwrap(),
        before_mtime
    );
}

#[test]
fn ignored_files_never_leave_the_machine() {
    let root = TempDir::new("sync-ignored");
    let local = base_repo(root.path());
    write(&local.join("secret.env"), "TOKEN=first-secret\n");
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member);
    let remote = host.repo.clone();
    assert!(remote.join("secret.env").exists());

    // The ignored directory's content was never sent.
    let ignored_blob = git(&local, &["hash-object", "ignored/big.bin"]);
    let probe = Command::new("git")
        .args(["cat-file", "-e", &ignored_blob])
        .current_dir(&remote)
        .status()
        .unwrap();
    assert!(
        !probe.success(),
        "an ignored file's content reached the sandbox"
    );

    // A file ignored after it was synced stops being synced.
    write(&local.join(".gitignore"), "ignored/\nsecret.env\n");
    write(&local.join("secret.env"), "TOKEN=second-secret\n");
    let (outcome, _) = load(&local, &host, &mut member);
    assert!(!remote.join("secret.env").exists());
    let listed = git(&remote, &["ls-tree", "-r", "--name-only", &outcome.tree]);
    assert!(!listed.lines().any(|line| line == "secret.env"), "{listed}");
    let second_blob = git(&local, &["hash-object", "secret.env"]);
    let probe = Command::new("git")
        .args(["cat-file", "-e", &second_blob])
        .current_dir(&remote)
        .status()
        .unwrap();
    assert!(
        !probe.success(),
        "the newly ignored content reached the sandbox"
    );

    // A force-added (tracked) file stays synced although it is ignored.
    write(&local.join("ignored/forced.txt"), "tracked anyway\n");
    git(&local, &["add", "-f", "ignored/forced.txt"]);
    load(&local, &host, &mut member);
    assert_eq!(
        fs::read_to_string(remote.join("ignored/forced.txt")).unwrap(),
        "tracked anyway\n"
    );
    assert!(!remote.join("ignored/big.bin").exists());
}

#[test]
fn a_missing_runner_is_installed_once_and_the_sync_retried() {
    let root = TempDir::new("sync-install");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    host.missing.set(true);
    let mut member = Member::default();
    let (outcome, log) = load(&local, &host, &mut member);
    assert!(outcome.installed);
    assert_eq!(host.installs.get(), 1);
    assert!(!log.contains("boatparty-runner-missing"), "{log}");
    assert_eq!(porcelain(&host.repo), porcelain(&local));
}

#[test]
fn detached_head_is_mirrored_detached() {
    let root = TempDir::new("sync-detached");
    let local = base_repo(root.path());
    let head = git(&local, &["rev-parse", "HEAD"]);
    git(&local, &["checkout", "-q", "--detach"]);
    let host = LocalRunner::new(root.path(), "remote");
    load(&local, &host, &mut Member::default());
    assert_eq!(git(&host.repo, &["rev-parse", "HEAD"]), head);
    let symbolic = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&host.repo)
        .status()
        .unwrap();
    assert!(!symbolic.success(), "HEAD should be detached");
    assert_eq!(
        git(&host.repo, &["rev-parse", "refs/heads/main"]),
        head,
        "main is mirrored as an ordinary branch"
    );
}

#[test]
fn load_refuses_a_repository_without_a_commit_or_outside_git() {
    let root = TempDir::new("sync-errors");
    let home = root.path().join("home");
    let repo = root.path().join("empty");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(
        &repo.join("boatparty.toml"),
        "version = 1\n[project]\nname = \"app\"\n",
    );
    let output = boatparty(&home)
        .current_dir(&repo)
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "workspace");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no commit"),
        "{doc}"
    );

    let outside = root.path().join("plain");
    fs::create_dir_all(&outside).unwrap();
    let output = boatparty(&home)
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", root.path())
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "workspace");
}
