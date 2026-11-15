#!/usr/bin/env bash
# Install or upgrade boatparty from a GitHub Release of the private
# amxv/boatparty repository. Downloads through the GitHub CLI, so it works
# with whatever access `gh auth login` already has; nothing is compiled.
#
#   gh release download -R amxv/boatparty -p install.sh -O - | bash
#   bash scripts/install.sh [--version vX.Y.Z] [--dir DIR]
#
# Options:
#   --version TAG   release tag to install (default: the latest release)
#   --dir DIR       install directory (default: ~/.local/bin)
#   --repo OWNER/R  repository (default: amxv/boatparty)
#
# Supports macOS and Linux on arm64 and x86_64. Each archive is checked
# against the release's SHA256SUMS before anything is installed.
set -euo pipefail

repo="amxv/boatparty"
version=""
dir="${HOME}/.local/bin"

die() { echo "boatparty install: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    --repo) [ $# -ge 2 ] || die "--repo needs a value"; repo="$2"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

command -v gh >/dev/null 2>&1 || die "the GitHub CLI (gh) is required: https://cli.github.com"
gh auth status >/dev/null 2>&1 || die "gh is not signed in; run: gh auth login"
command -v tar >/dev/null 2>&1 || die "tar is required"

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) die "unsupported OS: $(uname -s) (macOS and Linux only)" ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
# A shell running under Rosetta reports x86_64; prefer the native binary.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] \
  && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
target="${arch}-${os}"

if [ -z "$version" ]; then
  version="$(gh release view -R "$repo" --json tagName --jq .tagName)" \
    || die "could not find the latest release of $repo"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "boatparty install: downloading $version for $target" >&2
gh release download "$version" -R "$repo" -D "$work" \
  -p "boatparty-*-${target}.tar.gz" -p SHA256SUMS \
  || die "download failed (does $version have a $target archive?)"

archive="$(find "$work" -maxdepth 1 -name "boatparty-*-${target}.tar.gz" -exec basename {} \; | head -n 1)"
[ -n "$archive" ] || die "release $version has no archive for $target"

expected="$(awk -v f="$archive" '$2 == f || $2 == "*"f { print $1 }' "$work/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $archive"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$archive" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "$work/$archive" | awk '{ print $1 }')"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive"

tar -xzf "$work/$archive" -C "$work"
bin="$(find "$work" -type f -name boatparty -perm -u+x | head -n 1)"
[ -n "$bin" ] || die "archive $archive has no boatparty binary"

mkdir -p "$dir"
cp "$bin" "$dir/boatparty.tmp.$$"
chmod 755 "$dir/boatparty.tmp.$$"
mv -f "$dir/boatparty.tmp.$$" "$dir/boatparty"

echo "boatparty install: installed $("$dir/boatparty" --version) at $dir/boatparty" >&2
case ":${PATH}:" in
  *":${dir}:"*) ;;
  *) echo "boatparty install: add $dir to your PATH" >&2 ;;
esac
