#!/usr/bin/env bash
# Print the changelog section for VERSION (the "## VERSION ..." heading up to
# the next "## " heading) from the docs changelog, for the GitHub Release body.
#
# Usage: scripts/release/notes.sh VERSION
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 VERSION" >&2; exit 2; }
root="$(cd "$(dirname "$0")/../.." && pwd)"
changelog="$root/docs/src/content/docs/changelog.md"

notes="$(awk -v v="$1" '
  /^## / { if (found) exit; if ($2 == v) { found = 1; next } }
  found { print }
' "$changelog" 2>/dev/null || true)"

if [ -n "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
  printf '%s\n' "$notes" | sed -e '/./,$!d'
else
  echo "boatparty $1. See the changelog for details."
fi
cat <<'INSTALL'

## Install

```bash
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
```

Downloads the archive for this machine, checks it against `SHA256SUMS` and installs `boatparty` into `~/.local/bin`.

On Windows, run this in PowerShell:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod https://boatparty.ashray.xyz/install.ps1)))
```

The Windows installer verifies the ZIP and adds the installation directory to your user PATH.
INSTALL
