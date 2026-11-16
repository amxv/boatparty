#!/bin/sh
# Install or upgrade boatparty from its GitHub Releases.
#
#   curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
#   curl -fsSL https://boatparty.ashray.xyz/install.sh | sh -s -- --version v0.2.0
#
# Options (or the environment variables beside them):
#   --version TAG   release to install, such as v0.2.0 (BOATPARTY_VERSION;
#                   default: the latest release)
#   --dir DIR       install directory (BOATPARTY_INSTALL_DIR; default
#                   ~/.local/bin)
#
# Downloads with curl from the public GitHub release URLs; nothing else is
# needed. Supports macOS and Linux on arm64 and x86_64. The archive is checked
# against the release's SHA256SUMS before anything is installed, and the
# binary is replaced in one rename. Running it again upgrades.
set -eu

repo="amxv/boatparty"
version="${BOATPARTY_VERSION:-}"
dir="${BOATPARTY_INSTALL_DIR:-$HOME/.local/bin}"

say() { echo "boatparty install: $*" >&2; }
die() { say "$*"; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    -h | --help) sed -n '2,18p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
case "$dir" in /*) ;; *) die "the install directory must be an absolute path: $dir" ;; esac
if [ -n "$version" ]; then
  case "$version" in v*) ;; *) version="v$version" ;; esac
  printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || die "not a release tag: $version"
fi

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) die "unsupported OS: $(uname -s) (macOS and Linux only)" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
# A shell running under Rosetta reports x86_64; prefer the native binary.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] \
  && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
target="$arch-$os"
for tool in curl tar awk; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

if [ -z "$version" ]; then
  # The latest release's page redirects to its tag.
  url="$(curl --proto '=https' --tlsv1.2 -fsSL -o /dev/null -w '%{url_effective}' \
    "https://github.com/$repo/releases/latest")" \
    || die "could not reach https://github.com/$repo/releases/latest"
  version="${url##*/}"
  case "$version" in v[0-9]*) ;; *) die "could not find the latest release of $repo" ;; esac
fi
number="${version#v}"
archive="boatparty-$number-$target.tar.gz"

work="$(mktemp -d "${TMPDIR:-/tmp}/boatparty-install.XXXXXXXX")"
trap 'rm -rf "$work"' EXIT HUP INT TERM

fetch() { # asset
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$work/$1" \
    "https://github.com/$repo/releases/download/$version/$1" \
    || die "could not download $1 from $repo $version"
}

say "downloading $version for $target"
fetch SHA256SUMS
fetch "$archive"

expected="$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")"
printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' || die "SHA256SUMS has no single entry for $archive"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$archive" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "$work/$archive" | awk '{ print $1 }')"
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $archive; nothing was installed"

tar -xzf "$work/$archive" -C "$work"
binary="$work/boatparty-$number-$target/boatparty"
if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then die "the archive has no boatparty binary"; fi

mkdir -p "$dir"
staged="$dir/.boatparty.new.$$"
cp "$binary" "$staged"
chmod 755 "$staged"
installed="$("$staged" --version 2>/dev/null)" || { rm -f "$staged"; die "the downloaded binary does not run on this machine"; }
mv -f "$staged" "$dir/boatparty"
say "installed $installed at $dir/boatparty"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) say "add $dir to your PATH" ;;
esac
