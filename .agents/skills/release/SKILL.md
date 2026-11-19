---
name: release
description: Use this skill when the user says to cut, ship, publish, or create a new release of boatparty.
allowed-tools: Bash, Read, Write, Edit
---

# Release boatparty

`Cargo.toml` owns the version. `docs/src/content/docs/changelog.md` owns the release notes. `.github/workflows/release.yml` owns the build and the GitHub Release. Nothing is compiled on the developer's machine:

- Releases run only on GitHub-hosted runners, never on boat or the developer's machine.
- Linux x86_64 and aarch64 are tested and built on `ubuntu-latest` with cargo-zigbuild against glibc 2.28.
- macOS arm64 and x86_64 are tested and built natively on `macos-latest`; they are signed ad hoc.
- Windows x64 and Arm64 are built on `windows-latest`, with native x64 tests against a Linux SSH server. Windows users install with the PowerShell installer at `https://boatparty.ashray.xyz/install.ps1`.
- The release holds `boatparty-<version>-<target>.tar.gz` for Unix, matching `.zip` archives for Windows, `install.sh`, `install.ps1` and `SHA256SUMS`.

Users install with `curl -fsSL https://boatparty.ashray.xyz/install.sh | sh` (`docs/public/install.sh`, also attached to each release; it needs only `curl`). There is no npm or crates.io publication.

## 1. Inspect release state

```bash
git status --short --branch
git fetch origin --tags
git tag --sort=-version:refname | head -n 10
gh release list -R amxv/boatparty --limit 10
git log --oneline --decorate -n 30
```

Review the commits since the last tag. Release from `main`, pushed and in sync with `origin/main`.

## 2. Choose the version

Use the user's exact version if given. Otherwise take the smallest SemVer bump the user-visible changes justify (before 1.0.0: minor for breaking changes, patch for compatible fixes). A version with a `-` suffix (`0.2.0-rc.1`) is published as a prerelease. Never reuse a tag or release.

Update `package.version` in `Cargo.toml` and the root package entry in `Cargo.lock` by hand (the `name = "boatparty"` block). Do not run Cargo locally to do it.

## 3. Write the changelog

Add a `## <version> - <YYYY-MM-DD>` section at the top of `docs/src/content/docs/changelog.md`: user-visible behavior, fixes, and installation or compatibility changes, combined into concise notes. `scripts/release/notes.sh <version>` turns that section into the release body; run it to preview.

## 4. Validate

The CI workflow (GitHub-hosted Linux and Windows) must be green on the release commit: `gh run list -R amxv/boatparty --commit "$(git rev-parse HEAD)"`. The release needs no secret besides the workflow's `GITHUB_TOKEN`.

For changes to the release machinery itself, the pull request that changes it runs a full dry run. On `main`, `just release-dry-run` does the same on demand.

## 5. Commit, push, tag

Commit `Cargo.toml`, `Cargo.lock` and the changelog, push, confirm the pushed commit is the one to release, then:

```bash
just release-tag <version>
```

It checks the version against `Cargo.toml` and pushes `v<version>`. Do not create a different tag to recover from a failed run: fix the cause and re-run the failed jobs (`gh run rerun <id> --failed`).

## 6. Watch and verify

```bash
gh run list -R amxv/boatparty --workflow release.yml --limit 5
gh run watch <run-id> --exit-status
gh release view "v<version>" -R amxv/boatparty --json tagName,url,assets
```

Expect four tarballs, two Windows ZIPs, `install.sh`, `install.ps1` and `SHA256SUMS`. Verify the installer on a clean location: `sh docs/public/install.sh --version v<version> --dir "$(mktemp -d)"`. It downloads, checks the checksum and runs `boatparty --version`.

GitHub-hosted runners end with their jobs; nothing needs cleaning up.

## 7. Report

Version and tag, release commit, changelog summary, CI and release run results, the release URL and its assets, the installer check, and any warnings.
