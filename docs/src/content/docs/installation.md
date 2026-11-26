---
title: Installation
description: Install boatparty on macOS, Linux or Windows with one command, check the prerequisites, and learn how to upgrade, pin a version or uninstall.
order: 2
category: Start
summary: One curl or PowerShell command, checksum-verified, no package manager needed.
---

boatparty is a single executable. The installers download it from the project's [GitHub Releases](https://github.com/amxv/boatparty/releases), check it against the release's published checksums, and put it in a directory in your home folder. Nothing needs administrator rights except, on Windows, adding the optional OpenSSH Client feature.

| Platform | Architectures | Installer |
| --- | --- | --- |
| macOS | Apple silicon (arm64), Intel (x86_64) | `install.sh` with `curl` |
| Linux | x86_64, aarch64 (glibc 2.28 or newer) | `install.sh` with `curl` |
| Windows | x64, Arm64 | `install.ps1` with PowerShell |

Whatever your computer runs, your commands always run on the same Linux x86_64 sandbox. You do not need a Linux machine, a container runtime or a compiler locally.

## Before you install

You need three things on your computer and one account:

- **Git**, because boatparty mirrors a Git working tree. Your repository needs at least one commit.
- **OpenSSH** (`ssh` and `ssh-keygen`), because every command reaches its sandbox over SSH. boatparty creates and manages its own SSH key; it never touches `~/.ssh`.
- **`curl`** (macOS and Linux) for the installer.
- **A [boat.dev](https://boat.dev) account.** boatparty drives boat's API with an API key, which you create in [Your first run](/docs/getting-started).

macOS and most Linux distributions already have Git, OpenSSH and curl. On Windows, the installer can add the missing ones for you (see [Windows](#windows)).

The [boat CLI](https://docs.boat.dev) is optional but recommended: when it is installed and signed in, `boatparty login --create-key` can create a correctly scoped API key for you in one step.

## macOS and Linux

Run this in a terminal:

```bash
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
```

You should see something like:

```text
boatparty install: downloading v0.5.1 for aarch64-apple-darwin
boatparty install: installed boatparty 0.5.1 at /Users/you/.local/bin/boatparty
```

The script:

1. Detects your operating system and CPU (on a Mac running a shell under Rosetta, it still picks the native Apple silicon build).
2. Finds the latest release, downloads its archive and `SHA256SUMS`, and refuses to install anything if the checksum does not match.
3. Runs the new binary once to make sure it works on this machine.
4. Moves it into `~/.local/bin/boatparty` in a single rename, so a running copy is never left half-written.

It needs only `curl`, `tar`, `awk` and a POSIX shell. It does not use Homebrew, npm or the GitHub CLI.

### Put boatparty on your PATH

If `~/.local/bin` is not on your `PATH`, the installer ends with `boatparty install: add /Users/you/.local/bin to your PATH`. Add it in your shell's startup file, for example:

```bash
# zsh (the macOS default)
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc

# bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
```

Open a new terminal and check:

```bash
boatparty --version
```

```text
boatparty 0.5.1
```

### Installer options

Pass options after `sh -s --`, or set the matching environment variable:

| Option | Environment variable | Default | Meaning |
| --- | --- | --- | --- |
| `--version TAG` | `BOATPARTY_VERSION` | the latest release | Install a specific release, such as `v0.5.1` |
| `--dir DIR` | `BOATPARTY_INSTALL_DIR` | `~/.local/bin` | Install into another directory (an absolute path) |

```bash
# Install a specific release
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh -s -- --version v0.5.1

# Install into another directory
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh -s -- --dir "$HOME/bin"
```

## Windows

boatparty runs natively on Windows, on x64 and Arm64. Open PowerShell (Windows PowerShell 5.1 or PowerShell 7) and run:

```powershell
irm https://boatparty.ashray.xyz/install.ps1 | iex
```

The installer:

1. Downloads the ZIP for your architecture and verifies it against the release checksums (it uses `curl.exe` when available).
2. Checks that the downloaded `boatparty.exe` runs and reports the expected version.
3. Installs it as `%LOCALAPPDATA%\boatparty\bin\boatparty.exe` and adds that directory to your user `PATH`.
4. Looks for the tools boatparty needs and offers to install any that are missing:
   - **Git for Windows**, installed for the current user only, from a checksum-verified installer.
   - **The Windows OpenSSH Client** optional feature. Windows asks for administrator approval for this one.
5. Offers to install the optional **boat CLI** and start its sign-in, so `boatparty login --create-key` works afterwards.

Open a new terminal afterwards so the updated `PATH` takes effect, then check with `boatparty --version`.

### Installer settings on Windows

Set these environment variables before running the installer:

| Variable | Meaning |
| --- | --- |
| `$env:BOATPARTY_VERSION` | Install a specific release, such as `v0.5.1` |
| `$env:BOATPARTY_INSTALL_DIR` | Install into another directory |
| `$env:BOATPARTY_PREREQUISITES` | `Ask` (the default) prompts for missing tools; `Install` installs the required tools without asking; `Skip` installs only boatparty |

For an unattended setup, for example on a fresh virtual machine:

```powershell
$env:BOATPARTY_PREREQUISITES = 'Install'
irm https://boatparty.ashray.xyz/install.ps1 | iex
```

If Windows asks for a restart after adding OpenSSH, restart and then run `boatparty doctor` to confirm everything is found. [Using boatparty on Windows](/docs/windows) explains how the Windows client differs from macOS and Linux.

## Upgrading

Run the same install command again. It replaces the binary with the latest release in place. To move to a specific version, use `--version` (or `$env:BOATPARTY_VERSION` on Windows).

After an upgrade:

- Re-run `boatparty doctor`. New releases sometimes need new permissions on your boat API key (for example, 0.4.0 added `environment.read` and `environment.write`). If the `key_scope` check fails, run `boatparty login --create-key` again to get a key with every permission the new version uses.
- If you gave a coding agent the boatparty skill, refresh it with `boatparty skill` so it matches the new version.
- Read the [changelog](/docs/changelog) for behavior changes.

## Uninstalling

boatparty keeps everything in a few known places, so removing it is simple. First delete the sandboxes it created, while you still have the binary:

```bash
boatparty pool                 # see every project's sandboxes
boatparty rm bp-app-1          # delete one sandbox at boat
boatparty base rm              # in a project with a base: delete its snapshot and builder
```

Then remove the binary and, if you want, its files:

```bash
rm ~/.local/bin/boatparty
rm -rf ~/.config/boatparty ~/.local/state/boatparty
```

On Windows, delete `%LOCALAPPDATA%\boatparty` and the `.config\boatparty` and `.local\state\boatparty` folders in your user profile, and remove the install directory from your user `PATH`. [Files and environment variables](/docs/files-and-variables) lists every file boatparty writes.

You may also want to revoke the API key at boat with `boat api-key list` and `boat api-key revoke ID`.

## Next step

Continue with [Your first run](/docs/getting-started): create an API key, check your setup with `doctor`, and run your first command on a sandbox.
