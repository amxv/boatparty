# boatparty

Warm [boat.dev](https://boat.dev) sandbox pools for any Git project, managed from one place per boat account.

boatparty is under construction. This build can store a boat API key, check that the machine and the key are ready, and show the account's limits. Sandboxes, sync and remote commands arrive in later phases.

## Install from source

```bash
cargo install --locked --path .
```

macOS and Linux only. `ssh`, `ssh-keygen` and `git` must be on `PATH`.

## First run

Create a scoped boat API key and store it. The key is read from standard input only, so it never appears in an argument list or shell history:

```bash
boat api-key create boatparty --ttl 90d \
  --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.delete,ssh,host,snapshot.read,snapshot.write,account.read,file.read,file.write \
  --json | jq -r .secret | boatparty login --key-stdin
```

Then check everything:

```bash
boatparty doctor   # tools on PATH, account file, key validity and scope; prints a fix for each failure
boatparty limits   # starts used and left per window, running sandboxes, included VM time, sizes
```

`BOATPARTY_BOAT_API_KEY` overrides the stored key.

## Conventions

- Every command accepts `--json` and then prints exactly one JSON document on stdout. A failure is `{"ok": false, "error": {"code": "...", "message": "..."}}`.
- boatparty's own messages go to stderr, prefixed `boatparty: `.
- Exit status: `0` success, `2` usage error, `125` when boatparty itself failed (configuration, credentials, provider, connection, limits).

## Files

| Path | Content |
| --- | --- |
| `~/.config/boatparty/account.toml` | Provider settings and limits, created with documented defaults on first use. Honors `XDG_CONFIG_HOME` |
| `~/.config/boatparty/credentials.toml` | The boat API key, mode 0600 |
| `~/.local/state/boatparty/` | Machine-owned state (later phases). Honors `XDG_STATE_HOME` |

`BOATPARTY_HOME`, when set, replaces both roots: configuration goes to `$BOATPARTY_HOME/config` and state to `$BOATPARTY_HOME/state`.

## Development

```bash
just check-fast   # fmt, check, clippy
just check        # fmt, clippy, tests
```

Measured boat behavior the design relies on is recorded in [`docs/boat-facts.md`](docs/boat-facts.md).
