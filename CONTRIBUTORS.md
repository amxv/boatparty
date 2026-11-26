# Contributing to boatparty

Thanks for helping. `AGENTS.md` describes the architecture, the module map
and the conventions every change follows; read it first.

## Building and testing

Development compiles never run on the contributor's machine: boatparty
checks itself on a boat.dev sandbox, through boatparty. With a boat account
and boatparty installed (`curl -fsSL https://boatparty.ashray.xyz/install.sh | sh`,
then `boatparty login --key-stdin`):

```bash
cargo fmt --all              # the only local step; it compiles nothing
just remote-fast             # fmt --check and clippy -D warnings
just remote-test [FILTER]    # cargo test --locked
just remote-check            # the full gate: fmt, clippy, tests, shellcheck, docs
```

Without a boat account, the same gate runs in GitHub Actions on every push
and pull request (`.github/workflows/ci.yml`), with just the docs check for
changes limited to `docs/` and top-level Markdown files. To run the gate locally
with the pinned toolchain from `rust-toolchain.toml`, use `just check`.

## Conventions in short

- Every command accepts `--json`; a change to a JSON shape changes
  `docs/json.md` in the same commit (`tests/json.rs` checks it).
- boatparty's own messages go to stderr prefixed `boatparty: `; stdout
  belongs to the result or the remote command.
- Secrets never reach an argument list, a log, `state.json` or `runs.jsonl`.
- The runner (`src/runner/script/*.sh`) stays stack-neutral and bash 3.2
  compatible.

## Docs

The documentation site lives in `docs/` (Astro). `just docs-check` checks
it; pushes to `main` that change `docs/**` deploy it.

## Releases

Maintainers cut releases with `.agents/skills/release/SKILL.md`: a tag
`vX.Y.Z` matching `Cargo.toml` makes `.github/workflows/release.yml` build,
test and publish on GitHub-hosted runners.
