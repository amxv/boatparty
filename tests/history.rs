//! `history = "head"`: the sandbox holds HEAD's commit and tree, the
//! snapshot, and nothing older, while HEAD moves forward, sideways and to
//! other branches, and a switch to `full` deepens it. The runner runs
//! locally against scratch repositories.

#![cfg(unix)] // These fixtures execute the Linux runner or POSIX tool shims locally.

mod common;

use std::fs;
use std::path::Path;

use boatparty::sync::pack::usable_known;
use boatparty::sync::{
    self, History, MirrorOutcome, MirrorRequest, PullBack, RunEnd, WorktreeInfo,
};
use common::TempDir;
use common::runner::{LocalRunner, base_repo, git, write};

/// The client's record of a member between syncs.
#[derive(Default)]
struct Member {
    mode: String,
    known: Vec<String>,
    digest: Option<String>,
}

fn load(local: &Path, host: &LocalRunner, member: &mut Member, history: History) -> MirrorOutcome {
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
        seed: None,
    };
    let mut log = Vec::new();
    let outcome = sync::mirror(host, &request, &mut log)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&log)));
    assert_eq!(outcome.tree, snapshot.tree);
    member.mode = history.as_str().to_string();
    member.known = outcome.known.clone();
    member.digest = Some(digest);
    outcome
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn has_object(dir: &Path, sha: &str) -> bool {
    common::isolated("git")
        .args(["cat-file", "-e", sha])
        .current_dir(dir)
        .status()
        .unwrap()
        .success()
}

/// Remote and local agree on HEAD, branch and status; the remote shows one
/// commit, and Git's integrity checks find nothing missing.
fn assert_head_mirror(local: &Path, remote: &Path) {
    assert_eq!(
        git(remote, &["rev-parse", "HEAD"]),
        git(local, &["rev-parse", "HEAD"])
    );
    let branch = |dir: &Path| {
        common::isolated("git")
            .args(["symbolic-ref", "-q", "--short", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap()
            .stdout
    };
    assert_eq!(branch(remote), branch(local));
    assert_eq!(porcelain(remote), porcelain(local));
    assert_eq!(git(remote, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(git(remote, &["log", "--format=%H"]).lines().count(), 1);
    git(remote, &["fsck", "--connectivity-only", "--no-progress"]);
    git(remote, &["fsck", "--no-dangling", "--no-progress"]);
    assert_eq!(
        git(remote, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
}

/// A repository with several commits, a tag and another branch.
fn history_repo(root: &Path) -> std::path::PathBuf {
    let local = base_repo(root);
    for n in 2..=5 {
        write(&local.join(format!("file{n}.txt")), &format!("{n}\n"));
        commit(&local, &format!("commit {n}"));
    }
    git(&local, &["tag", "v1", "HEAD~2"]);
    git(&local, &["branch", "side", "HEAD~1"]);
    local
}

#[test]
fn a_first_head_sync_sends_one_commit_and_its_tree_and_nothing_older() {
    let root = TempDir::new("head-first");
    let local = history_repo(root.path());
    write(&local.join("untracked.txt"), "new\n");
    write(&local.join("a.txt"), "edited\n");
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head);

    let remote = &host.repo;
    assert_head_mirror(&local, remote);
    assert!(!has_object(remote, &git(&local, &["rev-parse", "HEAD~1"])));
    assert_eq!(
        git(
            remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads",
                "refs/tags",
                "refs/remotes"
            ]
        ),
        "refs/heads/main",
        "only the checked-out branch is mirrored"
    );
    assert_eq!(
        fs::read_to_string(remote.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(fs::read_to_string(remote.join("file5.txt")).unwrap(), "5\n");
    assert!(!remote.join("ignored").exists());
    assert_eq!(
        member.known.len(),
        2,
        "head mode knows the snapshot and HEAD"
    );
}

#[test]
fn head_moving_forward_several_commits_syncs_thin_and_keeps_one_commit() {
    let root = TempDir::new("head-forward");
    let local = history_repo(root.path());
    // A file deleted in HEAD whose content is still in older history: when
    // it comes back, the sandbox (which never had that history) needs it.
    write(&local.join("revived.txt"), "revived content\n");
    commit(&local, "add revived");
    fs::remove_file(local.join("revived.txt")).unwrap();
    commit(&local, "delete revived");
    let big: String = (0..20_000).map(|n| format!("line {n}\n")).collect();
    write(&local.join("big.txt"), &big);
    commit(&local, "big");
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    let first = load(&local, &host, &mut member, History::Head);

    write(&local.join("revived.txt"), "revived content\n");
    commit(&local, "revive");
    for n in 0..3 {
        write(&local.join("step.txt"), &format!("{n}\n"));
        commit(&local, &format!("step {n}"));
    }
    let second = load(&local, &host, &mut member, History::Head);
    assert!(
        second.pack_bytes < first.pack_bytes / 4,
        "unchanged files were sent again: {} then {}",
        first.pack_bytes,
        second.pack_bytes
    );
    let remote = &host.repo;
    assert_head_mirror(&local, remote);
    assert_eq!(
        fs::read_to_string(remote.join("revived.txt")).unwrap(),
        "revived content\n"
    );
    assert!(!has_object(remote, &git(&local, &["rev-parse", "HEAD~1"])));

    // Nothing changed: nothing is sent.
    let third = load(&local, &host, &mut member, History::Head);
    assert_eq!(third.pack_bytes, 0);
    assert_head_mirror(&local, remote);
}

#[test]
fn a_rewritten_head_after_amend_or_reset_syncs_sideways() {
    let root = TempDir::new("head-rewrite");
    let local = history_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head);

    write(&local.join("file5.txt"), "five, amended\n");
    git(&local, &["add", "-A"]);
    git(
        &local,
        &["commit", "-q", "--amend", "-m", "commit 5 amended"],
    );
    load(&local, &host, &mut member, History::Head);
    assert_head_mirror(&local, &host.repo);
    assert_eq!(
        fs::read_to_string(host.repo.join("file5.txt")).unwrap(),
        "five, amended\n"
    );

    // A rebase-like rewrite: drop two commits and build a different one.
    git(&local, &["reset", "-q", "--hard", "HEAD~2"]);
    write(&local.join("other.txt"), "rebased\n");
    commit(&local, "rebased work");
    load(&local, &host, &mut member, History::Head);
    assert_head_mirror(&local, &host.repo);
    assert!(!host.repo.join("file5.txt").exists());
    assert!(!host.repo.join("file4.txt").exists());
}

#[test]
fn switching_branches_moves_the_boundary_and_mirrors_only_the_current_branch() {
    let root = TempDir::new("head-branch");
    let local = history_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head);

    git(&local, &["checkout", "-q", "side"]);
    write(&local.join("side.txt"), "side\n");
    commit(&local, "side work");
    load(&local, &host, &mut member, History::Head);
    let remote = &host.repo;
    assert_head_mirror(&local, remote);
    assert_eq!(
        git(
            remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads",
                "refs/tags"
            ]
        ),
        "refs/heads/side"
    );

    git(&local, &["checkout", "-q", "main"]);
    load(&local, &host, &mut member, History::Head);
    assert_head_mirror(&local, remote);
    assert_eq!(
        git(
            remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads",
                "refs/tags"
            ]
        ),
        "refs/heads/main"
    );
    assert!(!remote.join("side.txt").exists());

    git(&local, &["checkout", "-q", "--detach"]);
    load(&local, &host, &mut member, History::Head);
    assert_eq!(
        git(remote, &["rev-parse", "HEAD"]),
        git(&local, &["rev-parse", "HEAD"])
    );
    git(remote, &["fsck", "--connectivity-only", "--no-progress"]);
}

#[test]
fn switching_to_full_deepens_the_member_and_back_needs_no_cleanup() {
    let root = TempDir::new("head-deepen");
    let local = history_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head);
    write(&local.join("more.txt"), "more\n");
    commit(&local, "more");
    load(&local, &host, &mut member, History::Head);

    load(&local, &host, &mut member, History::Full);
    let remote = &host.repo;
    assert_eq!(
        git(remote, &["rev-list", "--count", "HEAD"]),
        git(&local, &["rev-list", "--count", "HEAD"])
    );
    assert_eq!(
        git(remote, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    assert_eq!(
        git(remote, &["rev-parse", "v1"]),
        git(&local, &["rev-parse", "v1"])
    );
    assert_eq!(
        git(remote, &["rev-parse", "side"]),
        git(&local, &["rev-parse", "side"])
    );
    assert_eq!(
        git(remote, &["rev-parse", "refs/remotes/origin/main"]),
        git(&local, &["rev-parse", "refs/remotes/origin/main"])
    );
    git(remote, &["fsck", "--no-dangling", "--no-progress"]);
    assert_eq!(porcelain(remote), porcelain(&local));

    // Back to head: works on the deep repository without any cleanup.
    write(&local.join("again.txt"), "again\n");
    commit(&local, "again");
    load(&local, &host, &mut member, History::Head);
    assert_head_mirror(&local, remote);
}

/// A repository whose history is much larger than its HEAD tree.
fn long_history_repo(root: &Path) -> std::path::PathBuf {
    let local = base_repo(root);
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for n in 0..24 {
        // Incompressible content, rewritten every commit.
        let bytes: Vec<u8> = (0..20_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed & 0xff) as u8
            })
            .collect();
        fs::write(local.join("data.bin"), bytes).unwrap();
        commit(&local, &format!("data {n}"));
    }
    local
}

#[test]
fn a_first_head_pack_of_a_long_history_is_far_smaller_than_a_full_one() {
    let root = TempDir::new("head-size");
    let local = long_history_repo(root.path());
    let head = load(
        &local,
        &LocalRunner::new(root.path(), "head"),
        &mut Member::default(),
        History::Head,
    );
    let full = load(
        &local,
        &LocalRunner::new(root.path(), "full"),
        &mut Member::default(),
        History::Full,
    );
    assert!(
        head.pack_bytes * 10 < full.pack_bytes,
        "head {} vs full {}",
        head.pack_bytes,
        full.pack_bytes
    );
}

#[test]
fn copy_back_diffs_against_the_latest_snapshot_in_head_mode() {
    let root = TempDir::new("head-copyback");
    let local = history_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut member = Member::default();
    load(&local, &host, &mut member, History::Head);
    // HEAD moves and the working tree differs from it before the run.
    write(&local.join("next.txt"), "next\n");
    commit(&local, "next");
    write(&local.join("a.txt"), "local edit\n");

    let info = WorktreeInfo::read(&local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let (refs, digest) = sync::mirrored_refs(&info, History::Head).unwrap();
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: member.digest.as_deref() != Some(digest.as_str()),
        known: &member.known,
        history: History::Head,
        seed: None,
    };
    let mut sink = Collect::default();
    let outcome = sync::run_command(
        &host,
        &request,
        "printf 'from the command\\n' >> next.txt; printf 'made\\n' > made.txt",
        &mut sink,
        &sync::NeverCancel,
    )
    .unwrap();
    let RunEnd::Done {
        exit_code,
        patch_bytes,
        ..
    } = outcome.end
    else {
        panic!("no result: {}", String::from_utf8_lossy(&sink.stderr));
    };
    assert_eq!(exit_code, 0);
    assert!(patch_bytes > 0);
    let patch = sync::fetch_patch(&host, "app", &outcome.nonce).unwrap();
    let text = String::from_utf8_lossy(&patch).to_string();
    assert!(!text.contains("a.txt"), "the local edit came back: {text}");
    match sync::apply_pullback(&info, &patch).unwrap() {
        PullBack::Applied { mut files } => {
            files.sort();
            assert_eq!(files, ["made.txt", "next.txt"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        fs::read_to_string(local.join("next.txt")).unwrap(),
        "next\nfrom the command\n"
    );
    assert_eq!(
        fs::read_to_string(local.join("a.txt")).unwrap(),
        "local edit\n"
    );
    git(
        &host.repo,
        &["fsck", "--connectivity-only", "--no-progress"],
    );
}

#[derive(Default)]
struct Collect {
    stderr: Vec<u8>,
}

impl sync::Sink for Collect {
    fn stdout(&mut self, _bytes: &[u8]) {}

    fn stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
    }
}
