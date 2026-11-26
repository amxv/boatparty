---
title: Dev servers, ports and URLs
description: Run a dev server on a sandbox with boatparty serve, open it on localhost through port forwards, find its URL from another terminal with boatparty url, reach local services from the sandbox, and publish a public HTTPS URL with expose.
order: 21
category: Guides
summary: serve, forwards and auto ports, url, standalone forwards, reverse forwards and expose.
---

A dev server running on a sandbox is only useful if you can reach it. boatparty gives you three ways:

| You want | Use | You get |
| --- | --- | --- |
| A server you open in your own browser | `serve --forward` | `http://localhost:PORT` on your computer |
| A local tunnel to something already running on the sandbox (a database, a server started by hand) | `forward` | `http://localhost:PORT` on your computer |
| A URL other people or webhooks can reach | `expose` | A public `https://` URL, token-gated by default |

## Run a dev server with serve

```bash
boatparty serve --forward auto:3000 -- npm run dev
```

`serve` is like `run` for commands that do not end by themselves. It syncs your working tree, runs the same steps and gives the command the same environment as `run`, then keeps it running until you stop it with Ctrl-C. It takes the command in the same three forms as `run`: after `--`, as a `--sh` line, or as a name (`boatparty serve --forward auto:3000 dev` runs `npm run dev` through the script runner).

The output looks like this:

```text
boatparty: ensure npm-install done in 1s

> hello-api@1.0.0 dev
> node server.js

listening on http://localhost:3000
boatparty: port 3000 is listening on the sandbox (0.5s after the server started); here at http://localhost:50135
```

The last line is the one to watch for: the server is up, and `http://localhost:50135` on your computer reaches port 3000 on the sandbox.

```bash
curl http://localhost:50135/hi
```

```text
{"hello":"from the sandbox","path":"/hi"}
```

### Choosing local ports

`--forward [BIND:]LOCAL:REMOTE` makes `LOCAL` on your computer reach `REMOTE` on the sandbox. You can repeat it for several ports.

```bash
boatparty serve --forward 3000:3000 -- npm run dev          # the same port on both sides
boatparty serve --forward 8080:3000 -- npm run dev          # localhost:8080 here reaches 3000 there
boatparty serve --forward auto:3000 -- npm run dev          # a free local port, chosen for you
boatparty serve --forward auto:3000 --forward auto:6006 dev # two ports
```

`auto` is the best choice when several worktrees run the same server: each gets its own free local port, so they never collide. Your `localhost:3000` stays free for a server you run locally.

Local forwards listen on **both loopback addresses**, `127.0.0.1` and `::1`, at the same port, and the URL uses `localhost`. That way `http://localhost:PORT` works whichever address your browser or tool resolves `localhost` to. If your computer has no IPv6 loopback, boatparty says so and listens on `127.0.0.1` only:

```text
boatparty: IPv6 loopback is unavailable; forwarding localhost on 127.0.0.1 only
```

To listen on another address, put it first: `--forward 127.0.0.1:3000:3000` for IPv4 only, or `--forward 0.0.0.0:3000:3000` to let other machines on your network reach it (boatparty warns you when a forward listens beyond the loopback).

If an OAuth provider, CORS rule or auth library expects a specific origin, use the matching form. Since 0.5.1 the default origin is `http://localhost:PORT`; earlier versions used `http://127.0.0.1:PORT`.

### Knowing when the server is ready

boatparty watches a sandbox port and logs once when something starts listening on it, which is more reliable than guessing from the server's output:

- With an `auto` forward, it watches that forward's remote port automatically.
- `--ready-port PORT` chooses the port explicitly, and takes precedence.

With `--json`, readiness is also a `{"type":"event","event":"ready"}` frame, so a script or agent can wait for it before opening the URL.

### Telling the server its own URL

Many frameworks need to know the URL they are served at (for redirects, auth callbacks, absolute links). `serve` exports the local URLs to the command before applying `[run.env]`:

| Variable | Value |
| --- | --- |
| `BOATPARTY_FORWARD_URL` | The first local forward's URL, such as `http://localhost:50135` |
| `BOATPARTY_FORWARD_URLS` | Every local forward's URL, separated by spaces |
| `BOATPARTY_FORWARD_<REMOTEPORT>_URL` | The URL for one sandbox port, such as `BOATPARTY_FORWARD_3000_URL` |

Because they are set before `[run.env]` is expanded, you can refer to them there:

```toml
[run.env]
BETTER_AUTH_URL = "$BOATPARTY_FORWARD_URL"
NEXTAUTH_URL = "$BOATPARTY_FORWARD_3000_URL"
```

### Stopping the server

Press Ctrl-C (or send SIGTERM). The server is stopped on the sandbox, and `serve` exits with 130 (or 143). If the `serve` process is killed, the sandbox stops the server within seconds. The dedicated SSH connection closes its local listeners and disconnects forwarded clients, so you can restart on the same port even with a browser tab open.

`--max-minutes N` stops a forgotten server after N minutes. The default is 120; `--max-minutes 0` removes the cap.

If the server exits by itself, `serve` exits with its exit code, and files it changed in the checkout come back like `run`. A server you stopped sends nothing back; the next sync replaces those files.

### Running serve from an agent

`serve` blocks until the server ends. An agent should start it with its harness's background process tool, then read the URL with `boatparty url` (below), rather than parsing the `serve` output.

## Send edits to a running server

Once the server has started, other commands in the same workspace no longer wait for it. To push local edits to the running server, sync:

```bash
boatparty sync
```

```text
loaded main at f209b85b12 into bp-hello-api-1 (bx_d5zd8s2j) /home/user/hello-api: sent 299 B, 0.7s
```

Your server's file watcher (Vite, Next.js, `cargo watch`, `nodemon`) sees the changed files and reloads, exactly as it would locally. A `run` in the same workspace syncs too, so running the tests also updates the server.

The server shares the checkout, including build outputs, with every command beside it. When the server exits after a sync, boatparty notes that its result may mix old and new files. If you need isolated builds while a server runs, use a separate worktree.

## Find a running server's URL with url

```bash
boatparty url 3000
```

```text
http://localhost:50135
```

`url` prints the local URLs of servers running in **this workspace**, one per line. With a port, it prints only forwards to that sandbox port; without one, every running serve forward. It reads boatparty's local records and makes no network call, so it is instant.

It is made for a second terminal or a second agent: one agent starts the server, any other process in the same worktree asks for its URL.

| Exit status | Meaning |
| --- | --- |
| 0 | At least one URL was printed |
| 1 | No running serve in this workspace matches (`boatparty: error: no running serve forward in this workspace`) |
| 125 | boatparty itself failed |

With `--json` you also get the remote port, the serve process ID and its start time:

```json
{
  "forwards": [
    {
      "pid": 72373,
      "remote_port": 3000,
      "started_at": 1795717890116,
      "url": "http://localhost:50135"
    }
  ],
  "ok": true,
  "project": "hello-api",
  "workspace": "/Users/you/code/hello-api"
}
```

A forward appears as soon as the server command launches, which can be before the server listens. Wait for the `ready` event if you need the application to be up.

`boatparty status` also lists running servers under their sandbox:

```text
* bp-hello-api-1       bx_d5zd8s2j  running   files complete auto-stop 2026-11-26T18:36:30.750Z for /Users/you/code/hello-api
  serve (pid 72373): npm run dev http://localhost:50135
```

### One server per port

A second `serve` in the same workspace that claims a sandbox port already served fails with `already_serving` and points at the existing server, so you can reuse it:

```text
boatparty: error: port 3000 is already served for this workspace at http://localhost:50135 (pid 72373, started 41s ago); use it or stop that serve
```

To free the port, stop the process that runs that `serve`.

## Reach your computer from the sandbox with --reverse

Sometimes the server on the sandbox needs something that runs on your computer, like a local API or a database. `--reverse [BIND:]REMOTE:LOCAL` makes `REMOTE` on the sandbox reach `LOCAL` on your computer:

```bash
boatparty serve --reverse 20000:7777 --sh 'API_URL=http://127.0.0.1:20000 npm start'
```

Reverse forwards listen on the sandbox's `127.0.0.1` unless you give another bind address. Like local forwards, they travel on the server's own SSH connection and end with it.

## Standalone forwards with forward

`forward` opens a local port to a sandbox port without running anything, for services that are already running there, such as a database you started with `shell`:

```bash
boatparty forward 5432               # localhost:5432 here reaches 5432 on the sandbox
boatparty forward 8000:18000         # localhost:18000 here reaches 8000 there
boatparty forward 3000 --local auto  # a free local port
boatparty forward 8000 --bind 0.0.0.0  # also reachable from other machines
```

```text
boatparty: forwarding localhost:18000 -> sandbox port 8000 on bp-app-1 (bx_d5zd8s2j); here at http://localhost:18000; Ctrl-C to stop
```

`forward` holds the forward, and keeps the sandbox awake, until you press Ctrl-C. If its connection drops, it reopens it for up to 10 minutes, bringing the sandbox up again if it stopped. It exits 130 or 143 when you stop it and 125 if the connection fails for good. Standalone forwards are separate from `serve` and do not show up in `boatparty url`.

## Share a public URL with expose

`expose` publishes a sandbox port on an HTTPS URL that works from anywhere, which is useful for showing work to someone, testing on a phone, or receiving webhooks:

```bash
boatparty expose 8000                       # a URL that only works with its token
boatparty expose 5173 --localhost           # for a service bound to 127.0.0.1
boatparty expose 8000 --public --title docs # anyone with the URL can open it
boatparty expose 8000 --hide                # take the URL down again
```

The URL is printed on stdout and nowhere else:

```text
https://<generated-host>/?_token=<access-token>
boatparty: exposed port 8000 of bp-app-1 (bx_d5zd8s2j); the URL carries an access token: share it like a password
boatparty: the service must listen on 0.0.0.0:8000 (for one bound to 127.0.0.1, add --localhost)
```

Things to know:

- **Without `--public`, the URL carries an access token** (`_token`). Anyone with the whole URL can open it, so treat it like a password. boatparty does not store it; run the command again to print it again.
- **The URL is stable** for that sandbox and port, across stops and resumes.
- **The service must listen on `0.0.0.0`**, or on `127.0.0.1` with `--localhost`.
- **Some ports are reserved** by boat and refused: 22, 6080, 8090, 8911 and 47984 to 48010.
- **`--hide` takes the URL down.** The server keeps running, and the token is kept, so exposing the port again brings back the same URL.
- **Webhooks:** a token-gated URL answers a webhook's POST with a redirect that sets a cookie, which most webhook senders do not follow. Use `--public` for webhook receivers.
- `expose` needs boat's tunnel; see [Reliability and recovery](/docs/reliability#when-boats-tunnel-is-degraded) if it reports `tunnel_degraded`.

## GUI applications

`serve --desktop` runs a graphical application in the sandbox's desktop session, and `boatparty desktop --open` lets you see it. See [Desktop and GUI apps](/docs/desktop).
