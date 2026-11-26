---
title: account.toml reference
description: Every key in account.toml, the per-user file that holds boatparty's provider settings, defaults, account-wide limits and per-project limits for every project on this computer.
order: 43
category: Reference
summary: Provider settings, defaults, account limits and project limits in one file.
---

`account.toml` holds every limit for every project on this computer, in one place. It lives at `~/.config/boatparty/account.toml` (see [Files and environment variables](/docs/files-and-variables) for other locations) and is created with commented defaults the first time a command needs it.

It is separate from `boatparty.toml` on purpose: `boatparty.toml` is committed and describes what a project needs, while `account.toml` is yours and decides how much of your boat account each project may use. A project's file may ask for less than its limits, never more.

You can edit the file by hand, or use [`boatparty limits set`](/docs/limits#changing-limits), which keeps its comments and layout. Unknown keys are an error, so a typo never silently does nothing. The API key is **not** here; it lives in `credentials.toml` next to it.

## The default file

```toml
version = 1

[provider]
name = "boat"
# api_url = "https://example.invalid/api/v1"

[defaults]
machine = "default"
max_sandboxes = 8
steal_after_minutes = 30
lease_minutes = 5
idle_minutes = 5
heartbeat_minutes = 2
acquire_wait_seconds = 900

[account]
# max_running = 4
reserve_starts_per_hour = 0
reserve_starts_per_day = 0
# paid_starts = true

# Projects registered by `boatparty init`, one [projects.<name>] table each.
[projects.app]
```

## Top level

| Key | Type | Meaning |
| --- | --- | --- |
| `version` | integer | The file format version. Must be `1` |

## [provider]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | `"boat"` | The sandbox provider. `"boat"` is the only one |
| `api_url` | string | boat's public API | Another API base URL: `https://`, or `http://` to `127.0.0.1`, `localhost` or `[::1]` only. Mainly for testing |

## [defaults]

Defaults for every project. A project's own table can lower some of them.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `machine` | string | `"default"` | Machine size for new sandboxes: `small`, `default`, `large` or `xlarge` |
| `max_sandboxes` | integer | `8` | The most sandboxes one project's pool may own |
| `steal_after_minutes` | integer | `30` | An idle sandbox may be handed to another workspace after this many minutes |
| `lease_minutes` | integer | `5` (at most 5) | While a command uses a sandbox, keep its auto-stop this many minutes ahead. A client that dies leaves at most about this long of billing |
| `idle_minutes` | integer | `5` (at most 15) | After the last command ends, the sandbox stops after this many minutes |
| `heartbeat_minutes` | integer | `2` | How often a running command pushes the auto-stop forward. Must be less than `lease_minutes` |
| `acquire_wait_seconds` | integer | `900` | How long to wait for a free sandbox before failing with `pool_exhausted` |

## [account]

Limits across every project of this boat account.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `max_running` | integer | unset (only boat's limit) | The most sandboxes running at once, across all projects |
| `reserve_starts_per_hour` | integer | `0` | Starts per rolling hour that boatparty leaves unused for other tools and manual starts. boatparty waits rather than use them |
| `reserve_starts_per_day` | integer | `0` | The same, per rolling day |
| `paid_starts` | bool | unset (allowed) | Whether boatparty may use boat's paid extra starts when they are on. `false` never pays for a start |

## [projects.NAME]

One table per project, created by `boatparty init`. Every key is optional; an unset key uses `[defaults]` (or no project-specific cap).

| Key | Type | Meaning |
| --- | --- | --- |
| `max_sandboxes` | integer | The most sandboxes this project's pool may own, instead of `[defaults] max_sandboxes` |
| `max_running` | integer | The most of this project's sandboxes running at once |
| `max_machine` | string | The largest machine size the project's `boatparty.toml` may ask for |
| `starts_per_hour` | integer | Starts per rolling hour for this project |
| `starts_per_day` | integer | Starts per rolling day for this project |
| `idle_minutes` | integer | Instead of `[defaults] idle_minutes`, at most 15 |
| `lease_minutes` | integer | Instead of `[defaults] lease_minutes`, at most 5; it can only lower the default |

A `[sandbox] machine` in `boatparty.toml` above the project's `max_machine` is an error that names both files.

## Examples

```bash
# Never run more than four sandboxes; keep 20 starts a day for CI
boatparty limits set account max_running=4 reserve_starts_per_day=20

# A heavy project: larger machines allowed, but at most 10 starts an hour
boatparty limits set project app max_machine=large starts_per_hour=10

# Remove a project cap again
boatparty limits set project app max_running=
```

The relevant parts of the resulting file:

```toml
[account]
max_running = 4
reserve_starts_per_hour = 0
reserve_starts_per_day = 20

[projects.app]
max_machine = "large"
starts_per_hour = 10
```

See [Starts, limits and costs](/docs/limits) for what each limit does in practice, and [Pools and leases](/docs/pools-and-leases) for the pool settings.
