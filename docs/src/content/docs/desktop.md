---
title: Desktop and GUI apps
description: Open a sandbox's streamed Linux desktop in your browser with boatparty desktop, run graphical applications there with serve --desktop, and control when the streaming services run.
order: 27
category: Guides
summary: desktop --open, serve --desktop and the [sandbox] desktop setting.
---

Every boat sandbox can stream a full Linux desktop to your browser. boatparty uses it for two things: looking at the sandbox's screen, and running graphical applications (an Electron app, a browser under test, a native GUI) on Linux while you watch.

## Open the desktop

```bash
boatparty desktop --open
```

This starts the sandbox's streaming services (if they are not running), asks boat for a private, signed link to the desktop, and opens it in your default browser. Without `--open`, it prints the link instead:

```bash
boatparty desktop
```

```text
https://<generated-host>/<signed-path>
```

- **Treat the link like a password.** Anyone who has it can see and control the desktop. boatparty prints it on stdout only and never stores it.
- **Open it soon.** Open the link within ten minutes; after that, run the command again for a fresh one.
- **The sandbox's idle rules still apply.** The desktop does not keep the sandbox awake by itself: when no boatparty command holds a lease, it stops after `idle_minutes` as usual.

An explicit `desktop` request keeps the streaming services running for the rest of that boot of the machine.

`desktop` needs the `desktop` permission on your API key (`boatparty login --create-key` includes it) and boat's tunnel; see [Reliability and recovery](/docs/reliability#when-boats-tunnel-is-degraded) if it reports `tunnel_degraded`.

## Run a GUI application

```bash
boatparty serve --desktop -- ./scripts/run-electron-app.sh
boatparty desktop --open
```

`serve --desktop` runs the command inside the sandbox's desktop session. It starts the streaming services if needed, imports the session's display environment (`DISPLAY`, `XAUTHORITY`, the D-Bus session address and related variables), and then runs your command like any other `serve`: synced first, with the project's steps and environment, until you press Ctrl-C.

Readiness works differently from a web server:

- By default, `serve --desktop` reports **ready when the application's first visible window appears** (this works when `xdotool` is installed on the sandbox):

  ```text
  boatparty: first desktop window appeared (2.3s after the server started)
  ```

- With `--ready-port PORT`, or an `auto` forward, it reports ready when that port listens instead, as for any server.

You can combine it with forwards, for example to reach the app's debug port:

```bash
boatparty serve --desktop --forward auto:9222 -- npx electron . --remote-debugging-port=9222
```

## Why the streaming services stop when idle

Streaming a desktop costs CPU on the sandbox, which competes with your builds. So by default (`[sandbox] desktop = "auto"`), boatparty stops idle streaming services in the background after commands, unless:

- you asked for the desktop explicitly with `boatparty desktop` during this boot, or
- a `serve --desktop` server is still running.

If you want the desktop services left alone, for example because you keep a desktop tab open all day:

```toml
[sandbox]
desktop = "keep"
```

## Reading CPU steal

Run summaries report **CPU steal**, the share of time the sandbox's host gave the CPU to someone else. A busy desktop stream and a busy host both slow builds down. At 15% steal or more, boatparty adds a note, because timing-sensitive tests may fail for that reason alone. See [Reliability and recovery](/docs/reliability#slow-hosts).
