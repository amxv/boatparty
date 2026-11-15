//! Large packs: the choice between the runner's stdin and the host's upload
//! route, the fallback when an upload fails, and the runner reading an
//! uploaded pack. The runner runs locally; the "upload" copies the pack into
//! its state directory, as the signed upload does on a sandbox.

mod common;

use std::fs;
use std::path::Path;

use boatparty::runner::{RunnerHost, TrailerFilter};
use boatparty::sync::{self, History, MirrorOutcome, MirrorRequest, PackRoute, WorktreeInfo};
use common::TempDir;
use common::runner::{LocalRunner, base_repo, git, write};

fn load(local: &Path, host: &LocalRunner, known: &[String], history: History) -> MirrorOutcome {
    let info = WorktreeInfo::read(local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let (refs, _) = sync::mirrored_refs(&info, history).unwrap();
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: true,
        known,
        history,
        seed: None,
    };
    let mut log = Vec::new();
    sync::mirror(host, &request, &mut log)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&log)))
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain=v1", "--untracked-files=all"])
}

/// A repository with about 200 KB of incompressible content.
fn bulky_repo(root: &Path) -> std::path::PathBuf {
    let local = base_repo(root);
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let bytes: Vec<u8> = (0..200_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        })
        .collect();
    fs::write(local.join("blob.bin"), bytes).unwrap();
    local
}

fn uploaded_files(host: &LocalRunner) -> usize {
    fs::read_dir(host.state.join("in"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn packs_below_the_threshold_go_over_stdin_and_larger_ones_are_uploaded() {
    let root = TempDir::new("upload-threshold");
    let local = bulky_repo(root.path());

    let small = LocalRunner::new(root.path(), "small");
    small.upload_threshold.set(Some(10 * 1024 * 1024));
    let outcome = load(&local, &small, &[], History::Head);
    assert_eq!(outcome.pack_route, PackRoute::Ssh);
    assert_eq!(small.uploads.get(), 0);
    assert_eq!(porcelain(&small.repo), porcelain(&local));

    let large = LocalRunner::new(root.path(), "large");
    large.upload_threshold.set(Some(64 * 1024));
    let outcome = load(&local, &large, &[], History::Head);
    assert_eq!(outcome.pack_route, PackRoute::SignedUrl);
    assert_eq!(outcome.upload_fallback, None);
    assert_eq!(large.uploads.get(), 1);
    assert!(outcome.pack_bytes > 64 * 1024, "{}", outcome.pack_bytes);
    assert_eq!(porcelain(&large.repo), porcelain(&local));
    assert_eq!(
        fs::read(large.repo.join("blob.bin")).unwrap(),
        fs::read(local.join("blob.bin")).unwrap()
    );
    git(
        &large.repo,
        &["fsck", "--connectivity-only", "--no-progress"],
    );
    assert_eq!(
        uploaded_files(&large),
        0,
        "the runner removes uploaded packs"
    );
    let git_dir = local.join(".git");
    assert!(
        !fs::read_dir(&git_dir).unwrap().flatten().any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("boatparty-pack-")),
        "the local pack file is removed"
    );

    // Nothing new: no pack, no upload.
    let known = outcome.known.clone();
    let again = load(&local, &large, &known, History::Head);
    assert_eq!(again.pack_route, PackRoute::None);
    assert_eq!(large.uploads.get(), 1);

    // A host without an upload route always streams.
    let plain = LocalRunner::new(root.path(), "plain");
    let outcome = load(&local, &plain, &[], History::Full);
    assert_eq!(outcome.pack_route, PackRoute::Ssh);
}

#[test]
fn a_failed_upload_falls_back_to_stdin() {
    let root = TempDir::new("upload-fallback");
    let local = bulky_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    host.upload_threshold.set(Some(0));
    host.fail_uploads.set(true);
    let outcome = load(&local, &host, &[], History::Full);
    assert_eq!(outcome.pack_route, PackRoute::Ssh);
    let reason = outcome.upload_fallback.expect("the fallback is reported");
    assert!(reason.contains("connection failed"), "{reason}");
    assert_eq!(porcelain(&host.repo), porcelain(&local));
}

#[test]
fn an_upload_made_before_the_runner_was_installed_is_reused() {
    let root = TempDir::new("upload-install");
    let local = bulky_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    host.upload_threshold.set(Some(0));
    host.missing.set(true);
    let outcome = load(&local, &host, &[], History::Head);
    assert!(outcome.installed);
    assert_eq!(host.uploads.get(), 1, "the pack was uploaded again");
    assert_eq!(outcome.pack_route, PackRoute::SignedUrl);
    assert_eq!(porcelain(&host.repo), porcelain(&local));
}

#[test]
fn an_uploaded_pack_that_assumed_too_much_is_answered_with_needfull_and_resent() {
    let root = TempDir::new("upload-needfull");
    let local = bulky_repo(root.path());
    let head = git(&local, &["rev-parse", "HEAD"]);
    let host = LocalRunner::new(root.path(), "remote");
    host.upload_threshold.set(Some(0));
    let outcome = load(&local, &host, &[head], History::Full);
    assert!(outcome.full_retry);
    assert_eq!(host.uploads.get(), 2);
    assert_eq!(porcelain(&host.repo), porcelain(&local));
}

#[test]
fn the_runner_refuses_pack_names_that_leave_its_upload_directory() {
    let root = TempDir::new("upload-names");
    let local = base_repo(root.path());
    let head = git(&local, &["rev-parse", "HEAD"]);
    let host = LocalRunner::new(root.path(), "remote");
    write(&root.path().join("outside.pack"), "not a pack\n");
    for name in ["@../outside.pack", "@", "@.hidden", "2"] {
        let output = host
            .command(&[
                "sync".into(),
                "app".into(),
                "nbad".into(),
                name.into(),
                head.clone(),
                head.clone(),
                "main".into(),
                "keep".into(),
                "head".into(),
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{name}");
        let mut filter = TrailerFilter::new("nbad");
        filter.push(&output.stderr);
        assert_eq!(filter.result(), None, "{name}");
    }
    assert!(root.path().join("outside.pack").exists());
}
