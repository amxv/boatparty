# boat facts (Phase 1)

Measured on 2026-11-14 (22:23 to 22:36 UTC) against `https://boat.dev/api/v1` with a scoped API key, from a macOS laptop (ICMP round trip to the sandbox about 207 ms). The account is on the $20 plan. One sandbox, `bx_pvh2rerd` (`bp-phase1-s1`), was created, used for every question, and then deleted. It started as `small` and was resumed once onto `default`. It used 3 starts in total: one create, one reclaim resume that boat did not count, and one new-machine resume.

The API key is never shown. Every call goes through the helper `api.sh` (Appendix A), which reads the bearer header from a private file with `curl -H @file`, so the key never appears in argv. The helper scripts are in Appendix A. Each section below gives the commands it ran, the raw output (trimmed only where marked), and a conclusion.

Redactions: signed URLs, `_token` values and the client's own public IP addresses are replaced with placeholders.

## 1. Key scope

**Key used.** The user authorized an agent to create it directly. That diverges from plan assumption 2, which says the user creates keys.

```bash
boat api-key create boatparty-phase1 --ttl 1d --json \
  --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.delete,ssh,host,snapshot.read,snapshot.write,account.read,file.read,file.write
```

The response, with the secret removed, confirmed the scope:

```json
"credentialLane": "scoped-v1",
"scope": { "actions": ["sandbox.create","sandbox.read","sandbox.update","sandbox.stop","sandbox.resume",
  "sandbox.delete","ssh","host","snapshot.read","snapshot.write","account.read","file.read","file.write"],
  "environments": "*", "sandboxes": "*", "grandfathered": false }
```

The key is the actions in assumption 3 plus `file.read` and `file.write`. The docs (`ssh-access.md`, "Large files") say a download URL needs `file.read` and an upload URL needs `file.write`, so the two were added for question 2.

**Every call the provider boundary needs, made with that key** (the status line comes from `api.sh`):

| Call | Action it exercises | Result |
| --- | --- | --- |
| `GET /limits` | `account.read` | `200` (full body in question 5) |
| `GET /sandboxes?label=boatparty.phase1%3Dtrue` | `sandbox.read` | `200` |
| `POST /sandboxes` with `labels`, `noEnv`, `Idempotency-Key` | `sandbox.create` | `202`; labels came back in the body |
| Same `POST /sandboxes` repeated with the same key and body | `sandbox.create` | `202`, same id, no start counted |
| `GET /sandboxes/{id}` | `sandbox.read` | `200` |
| `POST /sandboxes/{id}/sshkey` | `ssh` | `200` with `hostKey`, `machineIp`, `sshEndpoint` |
| `PATCH /sandboxes/{id}` `{"ttlSeconds":1800}` | `sandbox.update` | `200`, `archiveAfter` moved to now + 1800 s |
| `POST /sandboxes/{id}/host` `{"port":8000}` | `host` | `200`, `access: private`, `isProtected: true`, URL with `_token` |
| `POST /sandboxes/{id}/files/upload-url` | `file.write` | `200` |
| `POST /sandboxes/{id}/files/download-url` | `file.read` | `200` |
| `POST /sandboxes/{id}/stop` | `sandbox.stop` | `202` |
| `POST /sandboxes/{id}/resume` | `sandbox.resume` | `200` (reclaim) or `202` (new machine); see question 5 |
| `GET /saved-snapshots?namePrefix=bp-phase1` | `snapshot.read` | `200` |
| `POST /saved-snapshots` `{"sandboxId":…,"name":"bp-phase1-test"}` | `snapshot.write` | `202` `saving`, then `ready` 0.9 s later |
| `GET /saved-snapshots/bp-phase1-test` | `snapshot.read` | `200`, then `404` after delete |
| `DELETE /saved-snapshots/bp-phase1-test` | `snapshot.write` | `200` `deleted` |
| `DELETE /sandboxes/{id}` with `X-Ascii-Confirm-Delete: {id}` | `sandbox.delete` | `202`, operation `completed` about 17 s later |
| `GET /deletion-operations/{op}` | `sandbox.read` or `account.read` | `200` |

Raw excerpts:

```text
[…] GET /limits -> 200 0.329426
[…] POST /sandboxes -> 202 0.551945
[…] POST /sandboxes/bx_pvh2rerd/sshkey -> 200 0.44
[…] PATCH /sandboxes/bx_pvh2rerd -> 200 0.371806   {"ok":true,"archiveAfter":"2026-11-14T22:54:46.144Z"}
[…] POST /sandboxes/bx_pvh2rerd/host -> 200 0.443408
{"ok":true,"type":"port.hosted","port":8000,"access":"private","isProtected":true,"url":"https://box-node-64838685549cea57-8000.on.boat.dev?_token=REDACTED"}
[…] POST /sandboxes/bx_pvh2rerd/files/upload-url -> 200 0.277398
[…] POST /sandboxes/bx_pvh2rerd/files/download-url -> 200 0.249268
[…] POST /sandboxes/bx_pvh2rerd/stop -> 202 0.266474
[…] POST /sandboxes/bx_pvh2rerd/resume -> 200 0.613076
[…] POST /sandboxes/bx_pvh2rerd/resume -> 202 1.177538
[…] POST /saved-snapshots -> 202 0.537849
{"ok":true,"type":"snapshot.saved.saving","status":"saving","snapshot":{"name":"bp-phase1-test","status":"saving","sourceSandboxId":"bx_pvh2rerd","createdAt":"2026-11-14T22:34:23.724Z"}}
+0.9s ready
{"name":"bp-phase1-test","status":"ready","sizeBytes":94654464,"sourceSandboxId":"bx_pvh2rerd","createdAt":"2026-11-14T22:34:23.724Z"}
[…] DELETE /saved-snapshots/bp-phase1-test -> 200 0.561066
{"ok":true,"type":"snapshot.saved.deleted","name":"bp-phase1-test","status":"deleted"}
[…] DELETE /sandboxes/bx_pvh2rerd -> 202 0.412534
{"ok":true,"type":"sandbox.deleting","operation":{"id":"bdop_8c86906803da420b85becdfc03862f30","kind":"sandbox","targetId":"bx_pvh2rerd","reason":"explicit","status":"processing","stage":"removing",…}}
blocked {"stage":"retrying","completedAt":null}
processing {"stage":"retrying","completedAt":null}
completed {"stage":"completed","completedAt":"2026-11-14T22:35:30.894Z"}
```

**Not tested:** `POST /sandboxes` with `from: <saved snapshot>`, which per `api-keys/scopes.md` needs `sandbox.create` plus `snapshot.read` on the source (both are in the key). Also not tested: a key restricted to particular sandboxes or environments. Whether each action is strictly needed was not measured, since that would take one key per removed action. Each action is the one the docs map to an operation the plan uses, so none is known to be unnecessary.

**Command a user should run:**

```bash
boat api-key create boatparty --ttl 90d \
  --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.delete,ssh,host,snapshot.read,snapshot.write,account.read,file.read,file.write
```

**Conclusion:** assumption 3's actions are enough for every lifecycle, SSH, host, saved-snapshot and limits call. The only gap is signed file URLs, which need `file.write` (upload) and `file.read` (download). Since question 2 selects signed upload URLs, both belong in the list.

## 2. Transfer speed

Data: 32 MiB (33,554,432 bytes) from `/dev/urandom`, sha256 `2e8b42a0…71404`, checked on both ends after every transfer. Sandbox `small`. SSH used one control master, `ServerAliveInterval 5`, default ciphers. Client OpenSSH 10.2p1, server OpenSSH 9.6p1.

Command: `f-transfer.sh` (Appendix A). Raw output:

```text
ssh target: 51.91.69.154 19042
== (a) SSH stdin -> cat > file
86.34s  0.389 MB/s
2e8b42a068bd8693d060ee94c5e79db1e8c57d632a6f990fca224f2a6c071404  /home/user/phase1/ssh-up.bin
== (b) signed upload URL (PUT)
{"ok":true,"type":"file.upload_url","method":"PUT","path":"/home/user/phase1/url-up.bin","expiresAt":"2026-11-14T22:41:39.000Z","urlHost":"https://box-node-64838685549cea57.on.boat.dev"}
PUT http 200 up 33554432B 7.477681s
url request .350s; transfer:
7.50s  4.473 MB/s
{"success":true,"path":"/home/user/phase1/url-up.bin","size":33554432}
2e8b42a068bd8693d060ee94c5e79db1e8c57d632a6f990fca224f2a6c071404  /home/user/phase1/url-up.bin
== (c) SSH stdout
21.60s  1.553 MB/s
2e8b42a068bd8693d060ee94c5e79db1e8c57d632a6f990fca224f2a6c071404  data/ssh-down.bin
== (d) signed download URL (GET)
{"ok":true,"type":"file.download_url","method":"GET","path":"/home/user/phase1/url-up.bin","expiresAt":"2026-11-14T22:42:09.000Z","urlHost":"https://box-node-64838685549cea57.on.boat.dev"}
GET http 200 down 33554432B 3.156006s
url request .298s; transfer:
3.18s  10.548 MB/s
2e8b42a068bd8693d060ee94c5e79db1e8c57d632a6f990fca224f2a6c071404  data/url-down.bin
```

**Route.** The sandbox's machine had only an IPv6 address (`ip: 2001:41d0:403:329a:100::2a`), so the API set `sshEndpoint: 51.91.69.154:19042`, and SSH went through it. On the sandbox, `SSH_CONNECTION` showed the client's real public IPv4 (`<client-ipv4> 2191 192.168.122.42 22`), so `sshEndpoint` is a port forward (NAT), not a proxy that terminates SSH.

Control runs. The laptop has IPv6, so the machine address on port 22 was also reachable directly:

```text
direct: <client-ipv6> 54632 2001:41d0:403:329a:100::2a 22
== direct IPv6 up 8 MiB
23.84s  0.352 MB/s
== direct IPv6 down 32 MiB
7.78s  4.316 MB/s
== relay up 8 MiB again (control)
32.54s  0.258 MB/s
```

The signed-URL host goes through Cloudflare, with an edge near the client:

```text
$ dig +short box-node-64838685549cea57.on.boat.dev A
104.18.23.116
104.18.22.116
$ curl -sS -o /dev/null -D - https://box-node-64838685549cea57.on.boat.dev/ | grep -iE '^(server|cf-ray|via):'
via: 1.1 Caddy
server: cloudflare
cf-ray: a5a9735b1f02b155-BLR
```

| Direction | SSH | Signed URL | URL / SSH |
| --- | --- | --- | --- |
| Up | 0.389 MB/s through `sshEndpoint` (0.258 on a repeat, 0.352 direct IPv6) | 4.47 MB/s, plus 0.35 s to get the URL | 11.5x |
| Down | 1.55 MB/s through `sshEndpoint` (4.32 direct IPv6) | 10.5 MB/s, plus 0.30 s to get the URL | 6.8x (2.4x against direct IPv6) |

**Conclusion:** SSH upload is about 0.26 to 0.39 MB/s whether it goes through `sshEndpoint` or straight to the machine's IPv6 address, so the relay is not the cause. Signed upload URLs, which terminate at a nearby Cloudflare edge, are 11.5x faster. The upload direction meets both conditions of the Phase 6 rule: signed URLs are at least 5x faster, and SSH is below 1 MB/s. One client location, one sample per row.

## 3. Client death on a multiplexed connection

Remote probe `f3-remote.sh` reads stdin in a loop and never writes to stdout. It writes `EOF <time>` to a file when `read` sees end of file, and keeps a `sleep 900` child running. Locally, a `sleep 1000 |` writer holds the ssh client's stdin open for the whole test, so any EOF the remote sees comes from the client's death, not from the pipe closing. The watcher runs over the shared master. Commands: `f3-local.sh` (Appendix A).

**3a. Shared master (`-o ControlMaster=auto`, master already running).**

```text
Master running (pid=84606)
local ssh pid 87152 (ssh -F … -o ControlMaster=auto bpf-bx_pvh2rerd bash ~/phase1/f3-remote.sh mux), stdin writer pid 87151
== remote started; SIGKILL local ssh client at 1794695358.356819000
== watch 15s (through the shared master)
t+0s wrapper[9497]: 5755 Ss 0 parent=sshd | child[9501]: 9497 S | eof: EOF 1794695358.499296415
t+1s wrapper[9497]: 5755 Ss 1 parent=sshd | child[9501]: 9497 S | eof: EOF 1794695358.499296415
…  (identical lines for t+2s to t+14s)
t+15s wrapper[9497]: 5755 Ss 15 parent=sshd | child[9501]: 9497 S | eof: EOF 1794695358.499296415
== now end the local stdin writer (closes the pipe's write end)
t+0s … t+5s wrapper[9497]: 5755 Ss 21 parent=sshd | child[9501]: 9497 S | eof: EOF 1794695358.499296415
== remote process table for the probe
   9497    5755 Ss        21 bash /home/user/phase1/f3-remote.sh mux
   9501    9497 S         21 sleep 900
```

The parent 5755 is the shared connection's `sshd: user@notty`, which also serves the master (480 s old). Afterwards the local master still held a pipe from the killed client:

```text
$ lsof -p 84606 | grep PIPE
ssh     84606 ashray    8   PIPE 0x74ccefa19ea5b712    16384  ->0xde4cc9afef334158
```

The local `tee` reading the killed client's stdout did not exit until the remote probe was killed. The master had kept the client's passed stdout descriptor.

**3b. Dedicated connection (`-o ControlMaster=no -o ControlPath=none`).**

```text
local ssh pid 92244 (ssh -F … -o ControlMaster=no -o ControlPath=none bpf-bx_pvh2rerd bash ~/phase1/f3-remote.sh direct), stdin writer pid 92243
== remote started; SIGKILL local ssh client at 1794695575.314229000
t+0s wrapper[12037]: 1 Ss 0 parent=systemd | child[12041]: 12037 S | eof: EOF 1794695575.467923470
…  (identical lines for t+1s to t+14s)
t+15s wrapper[12037]: 1 Ss 16 parent=systemd | child[12041]: 12037 S | eof: EOF 1794695575.467923470
== remote process table for the probe
  12037       1 Ss        21 bash /home/user/phase1/f3-remote.sh direct
  12041   12037 S         21 sleep 900
```

(The two clocks are not synchronized. The remote EOF came about 0.15 s after the local kill, roughly one network leg.)

**Conclusion:** in both modes the remote `read` saw end of file within about 0.15 s of the SIGKILL, and the remote process kept running for the whole 15 s, because nothing remote reacts by itself. On the shared master the wrapper stays a child of the connection's `sshd`. On a dedicated connection `sshd` exits and the wrapper is reparented to PID 1 immediately. The runner uses stdin EOF to stop attached services and piped commands. For `run`, EOF ends the attachment. The client renews a ten-minute lease over stdin and can send an explicit cancel. The detached supervisor kills the command group when the lease expires. Output files live under a private directory in `/tmp`, outside the home mount that boat replaces during restore. The client resumes each stream at its last confirmed byte. A replacement machine loses those records along with its processes (see section 4).

## 4. What survives a new machine

Markers and a package were installed at 22:33 UTC. A late marker was written at 22:34:19.93, 0.6 s before the stop was requested. Commands: the heredoc in `f4-markers` and the checks in `f-cycle.sh` (Appendix A).

```text
sl not installed / tree already installed / cowsay not installed
passwordless sudo ok
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q sl   -> apt exit 0, real 0m3.899s
Status: install ok installed   Version: 5.02-1
boot_id 288a343a-2513-4bb9-af73-981e2aa133ab   hostname box-node-64838685549cea57
live process pid 12998 (nohup sleep 3000)
```

**After a stop followed by an immediate resume (same type, reclaimed machine):**

```text
boot_id now 288a343a-2513-4bb9-af73-981e2aa133ab at create 288a343a-2513-4bb9-af73-981e2aa133ab
box-node-64838685549cea57
12998 sleep 3000
/home/user/.cache/bp-phase1/marker   cache-marker
/home/user/.cache/pip/bp-marker      pipcache
/home/user/keep/marker               keep-marker
/opt/bp-phase1/marker                opt-marker
/var/tmp/bp-phase1-marker            vartmp-marker
/tmp/bp-phase1-marker                tmp-marker
Status: install ok installed
```

**After stop, wait for `archived`, resume with `{"type":"default"}` (new machine: new boot id, hostname, IP and `sshEndpoint`), wait for `hydrated: true`:**

```text
+2.16s provisioned hydrated=false phase=folding
+3.01s idle hydrated=false phase=sysdelta        (hydration {"entriesDone":182,"entriesTotal":22533,"percent":0})
… hydratedAt 2026-11-14T22:34:37.659Z (resume sent 22:34:31.05, so about 6.6 s)
boot_id now f032bdd7-ae6b-4949-a923-b34a152c739e at create 288a343a-2513-4bb9-af73-981e2aa133ab
box-node-8610c7eeb8b913ff
no live sleep 3000
/home/user/.cache/bp-phase1/marker   MISSING
/home/user/.cache/pip/bp-marker      MISSING
/home/user/keep/marker               keep-marker
/home/user/keep/late-marker          late-marker
/opt/bp-phase1/marker                opt-marker
/var/tmp/bp-phase1-marker            MISSING
/tmp/bp-phase1-marker                MISSING
Status: install ok installed
-rwxr-xr-x 1 root root 26568 Feb  2  2019 /usr/games/sl
~/.cache now: cua-driver dconf evolution fontconfig gstreamer-1.0 ibus mesa_shader_cache motd.legal-displayed obexd openbox tracker3
```

Also seen: the new-machine resume's `snapshotCompletedAt` was `22:34:30.366Z`, after the stop's own snapshot at `22:34:21.801Z`, so boat re-snapshotted the kept machine before it released it. Separately, `noEnv` held: `~/.config/ascii/boat/config.json` on the new machine contains only `api_url` and `channel` (no API key). However, `GET /sandboxes/{id}` reports `environment: "base"` and does not expose `noEnv`.

**Conclusion:** on a new machine, `~/.cache` (including `~/.cache/pip`), `/var/tmp` and `/tmp` are lost. `~/keep`, `/opt` and an apt-installed package survive, as does a home file written 0.6 s before the stop. A reclaimed machine keeps everything, including `/tmp` and running processes, which explains gg-desktop's belief that `~/.cache` "outlives" Docker state (C2 row 2). The docs are right about `~/.cache`. `/var/tmp` is not saved although `/var` in general is (C2 row 1, only partly). In this one sample the late write was kept (C2 row 3).

### 4b. `/usr/local/rustup` and `/usr/local/cargo` across a new machine (Phase 9)

Run on 2026-11-15 with a scratch `default` sandbox (`bx_atdh43k7`, project `bp9-dur`), deleted afterwards. Before the stop, with `CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup`: `rustup toolchain install 1.95.0 --profile minimal`, `cargo fetch` of a crate depending on `itoa`, and marker files in both homes and in `~`. Then `boatparty dock --wait` (hydration complete, stopped). A resume with `--type default` (the size it already had) reclaimed the same machine (`status: running`, same IP), so it was stopped again and resumed with `--type small`, which landed on a new machine (`status: resuming`, new IP, new boot id):

```text
before: boot_id 4bf1f846-b381-4749-a5c7-b503969005c1  box-node-3f838b1c4d88a171
after:  boot_id e5a5ffd7-b2e1-442b-afb5-3cf243542ac3  box-node-b5194b61d11ebe3c
/usr/local/cargo/bp9-marker: cargo-marker
/usr/local/rustup/bp9-marker: rustup-marker
/home/user/keep-bp9: opt-check
itoa-1.0.19.crate                       (in /usr/local/cargo/registry/cache)
1.95.0-x86_64-unknown-linux-gnu         (rustup toolchain list)
585M /usr/local/rustup/toolchains/1.95.0-x86_64-unknown-linux-gnu
rustc 1.95.0 (59807616e 2026-04-14)
offline-build-ok                        (cargo build --offline)
```

**Conclusion:** changes under `/usr/local/rustup` and `/usr/local/cargo` (installed toolchains, the registry) survive a resume onto a new machine, as the docs' "your changes in /usr" says. The `rust` preset keeps `CARGO_HOME=/usr/local/cargo` and `RUSTUP_HOME=/usr/local/rustup`. Also seen: a resume that names the sandbox's current type does not force a new machine; only a different type does.

## 5. Start accounting

`GET /limits` `starts.*.used` around each start (`minute/hour/day`), with response headers. The counts are account-wide; the `minute` count also slides with time.

| Step | Before | After | HTTP and response | `RateLimit-Policy` / `RateLimit` |
| --- | --- | --- | --- | --- |
| Create | 1/4/46 | 2/5/47 | `202`, `type: sandbox.created`, `status: provisioning` | `"ip";q=10000;w=60, "starts-minute";q=12;w=60, "starts-hour";q=60;w=3600, "starts-day";q=200;w=86400` / `"ip";r=9986;t=60, "starts-minute";r=10;t=60` |
| Create retried with the same `Idempotency-Key` and body | 2/5/47 | 2/5/47 | `202`, same id, state `idle` | ip policy only |
| Stop, `archived` after 1.46 s, resume `{}` 2.2 s later | 0/5/47 | 0/5/47 | **`200`**, `type: sandbox.resuming`, **`status: running`**, `sandbox.state: ready`, `hydrated: true`, same `ip` and `sshEndpoint` | **ip policy only**: `"ip";q=10000;w=60` / `"ip";r=9993;t=60` |
| Stop, `archived`, resume `{"type":"default"}` | 0/5/47 | 1/6/48 | **`202`**, `type: sandbox.resuming`, **`status: resuming`**, `sandbox.state: provisioned`, `hydrated: false`, new `ip` and `sshEndpoint`, `wipePendingUntilRestart: true` | `"ip";q=10000;w=60, "starts-minute";q=12;w=60, "starts-hour";q=60;w=3600, "starts-day";q=200;w=86400` / `"ip";r=9978;t=60, "starts-minute";r=11;t=60` |

Raw `starts` from the create step:

```text
before: {"minute":{"limit":12,"used":1,"remaining":11},"hour":{"limit":60,"used":4,"remaining":56},"day":{"limit":200,"used":46,"remaining":154}}
after:  {"minute":{"limit":12,"used":2,"remaining":10},"hour":{"limit":60,"used":5,"remaining":55},"day":{"limit":200,"used":47,"remaining":153}}
```

`RateLimit` names only the start window with the fewest starts left (here `starts-minute`), as the OpenAPI describes.

**Conclusion:** a reclaimed resume is free, and its response tells you so. A counted start answers `202` with `status: "resuming"` and lists `starts-*` in `RateLimit-Policy`. A reclaim answers `200` with `status: "running"`, a running `sandbox.state`, `hydrated: true`, and only the `ip` policy. An idempotent create retry also counts nothing.

## 6. Latencies

All times are wall clock from the laptop. "+" times count from sending the start request. Polls ran every 0.5 s.

| Operation | Measured |
| --- | --- |
| Create to running | `POST` answered in 0.55 s with `provisioned`; first poll at +1.36 s already `ready` |
| Resume (reclaim) to running | `POST` 0.61 s, `200` already `ready`; `idle` at +1.53 s |
| Resume (new machine, `small` to `default`) to running | `POST` 1.18 s; `provisioned` +2.16 s; `idle` +3.01 s; `hydrated` about +6.6 s (small disk, 22,533 entries) |
| First `POST /sshkey` after a start (sent once state was running) | create: 1 attempt, 0.44 s; reclaim: 1 attempt, 0.33 s; new machine: 1 attempt, 0.54 s. No retryable error (`sandbox_restoring`, `sandbox_securing`, `ssh_host_key_unavailable`) appeared |
| `PATCH ttlSeconds` (5 calls) | 0.372, 0.471, 0.329, 0.335, 0.429 s (median 0.37) |
| `GET /sandboxes/{id}` (5 calls) | 0.280, 0.219, 0.239, 0.236, 0.212 s (median 0.24) |
| `GET /limits` | 0.33 to 0.68 s |
| Stop to `archived` | 1.46 s, 1.68 s, 3.36 s |
| Saved snapshot of a stopped sandbox to `ready` | 0.9 s (94.6 MB) |
| Sandbox delete operation to `completed` | about 17 s (went `blocked`/`retrying` first) |
| SSH: new control master plus first command | 2.45 s; warm command through the master 0.36 s |

**Conclusion:** one `PATCH ttlSeconds` costs 0.33 to 0.47 s, about as much as a whole warm SSH command (0.36 s). TTL changes must therefore stay off the synchronous path. Starts are fast (running in 1.4 to 3 s), and `POST /sshkey` succeeded on the first try once the state was running.

## Cleanup evidence

```text
[…] GET /sandboxes/bx_pvh2rerd -> 404   {"ok":false,"code":"not_found","status":404}
[…] GET /sandboxes?label=boatparty.phase1%3Dtrue -> 200   {"count":0,"ids":[]}
[…] GET /sandboxes?namePrefix=bp-phase1 -> 200   {"count":0}
[…] GET /saved-snapshots?namePrefix=bp-phase1 -> 200   {"ok":true,"count":0}
[…] GET /limits -> 200   {"starts":{"minute":0,"hour":6,"day":48},"activeSandboxes":1}
```

The remaining active sandbox predates this phase and was not touched.

## 7. Saving over an existing saved snapshot (2026-11-15)

Measured with a stopped `default` builder (`bx_4b5vuvn7`) whose name `bp-bpe2e-rust` already held a ready snapshot, by `POST /saved-snapshots` with the same name and polling `GET /saved-snapshots/{name}` every second:

```text
before: ready  snapshotId 1d62518f-…  createdAt 2026-11-15T09:28:21.787Z  sizeBytes 20406272
POST -> 202 snapshot.saved.saving, status saving, still snapshotId 1d62518f-…
+0s    saving 1d62518f-…  createdAt 09:28:21.787Z
+65s   ready  101dd6f7-…  createdAt 09:28:21.787Z  sizeBytes 34361344
```

**Conclusion:** during a re-save the name reports `saving` at once, and `ready` comes with a new `snapshotId`; `createdAt` keeps the name's first save. `base build` still waits for a `snapshotId` different from the one before its save, so a stale `ready` can never pass for the new base. The builder and the snapshot were deleted afterwards.

## Appendix A: helper scripts

All scripts lived in `/tmp/bpf1` (mode 700) and were removed afterwards. To rerun them, put the key in `/tmp/bpf1/key` (mode 600), then:

```bash
umask 077; printf 'Authorization: Bearer %s\n' "$(cat /tmp/bpf1/key)" > /tmp/bpf1/authhdr
mkdir -p /tmp/bpf1/ssh /tmp/bpf1/out
ssh-keygen -q -t ed25519 -N '' -C boatparty-phase1 -f /tmp/bpf1/ssh/id_ed25519
```

`now.sh`:

```bash
#!/bin/bash
perl -MTime::HiRes=time -e 'printf "%.3f\n", time'
```

`api.sh`: one API call. It prints the body, writes headers to `$HDR`, and prints status and time on stderr.

```bash
#!/bin/bash
m=$1; p=$2; b=${3-}
HDR=${HDR:-/tmp/bpf1/out/last.hdr}
args=(-sS -X "$m" "https://boat.dev/api/v1$p" -H @/tmp/bpf1/authhdr -D "$HDR" -w '\n%{http_code} %{time_total}\n')
[ -n "$b" ] && args+=(-H 'Content-Type: application/json' --data "$b")
[ -n "${EXTRA_HDR:-}" ] && args+=(-H "$EXTRA_HDR")
out=$(curl "${args[@]}")
code_time=$(printf '%s' "$out" | tail -n1)
printf '%s\n' "$out" | sed '$d'
echo "[$(perl -MTime::HiRes=time -e 'printf "%.3f", time')] $m $p -> $code_time" >&2
```

`waitstate.sh ID REGEX [interval]`: polls `GET /sandboxes/{id}` and prints each change of state, hydration flag or hydration phase.

```bash
#!/bin/bash
id=$1; re=$2; iv=${3:-0.5}
T0=${T0:-$(/tmp/bpf1/now.sh)}; last=""
while :; do
  body=$(/tmp/bpf1/api.sh GET /sandboxes/$id 2>/dev/null)
  st=$(jq -r '.sandbox.state // .code' <<<"$body")
  cur="$st hydrated=$(jq -r '.sandbox.hydrated|tostring' <<<"$body") phase=$(jq -r '.sandbox.hydration.phase // "-"' <<<"$body")"
  t=$(/tmp/bpf1/now.sh)
  [ "$cur" != "$last" ] && { printf '+%.2fs %s\n' "$(echo "$t - $T0" | bc)" "$cur"; last=$cur; }
  [[ $st =~ $re ]] && { echo "$body" > /tmp/bpf1/out/last-get.json; break; }
  sleep "$iv"
done
```

`sshkey.sh ID`: calls `POST /sshkey` until it returns `200`, logging every attempt. It then pins the host key under `HostKeyAlias=<id>` and writes an OpenSSH config with host alias `bpf-<id>`, using `sshEndpoint` when set, else `machineIp` port 22.

```bash
#!/bin/bash
id=$1; T0=${T0:-$(/tmp/bpf1/now.sh)}; pub=$(cat /tmp/bpf1/ssh/id_ed25519.pub); n=0
while :; do
  n=$((n+1)); ts=$(/tmp/bpf1/now.sh)
  body=$(HDR=/tmp/bpf1/out/sshkey.hdr /tmp/bpf1/api.sh POST /sandboxes/$id/sshkey "$(jq -nc --arg k "$pub" '{key:$k}')" 2>/tmp/bpf1/out/sshkey.err)
  code=$(awk '{print $(NF-1)}' /tmp/bpf1/out/sshkey.err); te=$(/tmp/bpf1/now.sh)
  printf 'attempt %d at +%.2fs: http %s (%.2fs) %s\n' $n "$(echo "$ts - $T0"|bc)" "$code" "$(echo "$te - $ts"|bc)" \
    "$(jq -c 'if .ok then {ok,machineIp,sshEndpoint,sshUser,hostKeyType:(.hostKey|split(" ")[0])} else {code,message,details:.error.details} end' <<<"$body")"
  if [ "$code" = 200 ]; then
    hk=$(jq -r .hostKey <<<"$body"); ep=$(jq -r '.sshEndpoint // empty' <<<"$body")
    if [ -n "$ep" ]; then host=${ep%:*}; port=${ep##*:}; else host=$(jq -r .machineIp <<<"$body"); port=22; fi
    echo "$id $hk" > /tmp/bpf1/ssh/known_hosts_$id
    printf 'Host bpf-%s\n  HostName %s\n  Port %s\n  User user\n  IdentityFile /tmp/bpf1/ssh/id_ed25519\n  IdentitiesOnly yes\n  StrictHostKeyChecking yes\n  UserKnownHostsFile /tmp/bpf1/ssh/known_hosts_%s\n  HostKeyAlias %s\n  ControlPath /tmp/bpf1/ssh/cm-%%C\n  ControlPersist 600\n  ServerAliveInterval 5\n  ServerAliveCountMax 3\n' \
      "$id" "$host" "$port" "$id" "$id" > /tmp/bpf1/ssh/config_$id
    echo "ssh target: $host port $port (sshEndpoint=${ep:-null})"; break
  fi
  [ $n -ge 120 ] && exit 1; sleep 1
done
```

`f-create.sh` (questions 1, 5, 6): reads limits, creates, polls to running, authorizes SSH, reads limits, retries the create idempotently.

```bash
#!/bin/bash
cd /tmp/bpf1
./api.sh GET /limits | jq -c '{starts,activeSandboxes,canStart}'
IK=$(uuidgen)
BODY='{"name":"bp-phase1-s1","type":"small","ttlSeconds":1800,"noEnv":true,"labels":{"boatparty.phase1":"true","boatparty.role":"phase1-test"}}'
T0=$(./now.sh); export T0
HDR=out/create.hdr EXTRA_HDR="Idempotency-Key: $IK" ./api.sh POST /sandboxes "$BODY" > out/create.json
grep -i '^ratelimit' out/create.hdr
ID=$(jq -r .sandbox.id out/create.json); echo "$ID" > out/s1.id
./waitstate.sh "$ID" '^(ready|idle|running|error|cancelled)$' 0.5
./sshkey.sh "$ID"
./api.sh GET /limits | jq -c '{starts,activeSandboxes}'
HDR=out/create2.hdr EXTRA_HDR="Idempotency-Key: $IK" ./api.sh POST /sandboxes "$BODY" | jq -c '{ok,type,id:.sandbox.id,state:.sandbox.state}'
grep -i '^ratelimit' out/create2.hdr; ./api.sh GET /limits | jq -c '.starts'
```

Question 6 latencies and question 1 calls, run by hand:

```bash
ID=$(cat /tmp/bpf1/out/s1.id); cd /tmp/bpf1
for i in 1 2 3 4 5; do ./api.sh GET /sandboxes/$ID >/dev/null; done
for i in 1 2 3 4 5; do ./api.sh PATCH /sandboxes/$ID '{"ttlSeconds":1800}' | jq -c '{ok,archiveAfter:.sandbox.archiveAfter}'; done
./api.sh POST /sandboxes/$ID/host '{"port":8000,"title":"phase1"}' | jq -c '{ok,type,port,access,isProtected,url:(.url|sub("_token=[^&]*";"_token=REDACTED"))}'
F=/tmp/bpf1/ssh/config_$ID
T=$(./now.sh); ssh -F $F -o ControlMaster=auto bpf-$ID 'uname -a; echo "$SSH_CONNECTION"'; echo "$(echo "$(./now.sh) - $T"|bc)s"
T=$(./now.sh); ssh -F $F bpf-$ID true; echo "$(echo "$(./now.sh) - $T"|bc)s"
```

`f-transfer.sh` (question 2). Signed URLs are never printed.

```bash
#!/bin/bash
cd /tmp/bpf1; ID=$(cat out/s1.id); F=/tmp/bpf1/ssh/config_$ID; H=bpf-$ID
mkdir -p data; [ -f data/up.bin ] || head -c 33554432 /dev/urandom > data/up.bin
rate() { perl -e 'printf "%.2fs  %.3f MB/s\n", $ARGV[1], $ARGV[0]/$ARGV[1]/1e6' 33554432 "$1"; }
ssh -F $F $H 'mkdir -p ~/phase1'
t0=$(./now.sh); ssh -F $F $H 'cat > ~/phase1/ssh-up.bin' < data/up.bin; rate "$(echo "$(./now.sh)-$t0"|bc)"
U=$(./api.sh POST /sandboxes/$ID/files/upload-url '{"path":"phase1/url-up.bin"}' | jq -r .url)
t1=$(./now.sh); curl -sS -X PUT -T data/up.bin -o out/put.body "$U"; rate "$(echo "$(./now.sh)-$t1"|bc)"; unset U
t0=$(./now.sh); ssh -F $F $H 'cat ~/phase1/ssh-up.bin' > data/ssh-down.bin; rate "$(echo "$(./now.sh)-$t0"|bc)"
U=$(./api.sh POST /sandboxes/$ID/files/download-url '{"path":"phase1/url-up.bin"}' | jq -r .url)
t1=$(./now.sh); curl -sS -o data/url-down.bin "$U"; rate "$(echo "$(./now.sh)-$t1"|bc)"; unset U
ssh -F $F $H 'sha256sum ~/phase1/*.bin'; shasum -a 256 data/*.bin
```

For the direct IPv6 control, the same config was used with `HostName <machine ipv6>` and `Port 22`.

`f3-remote.sh` (uploaded to `~/phase1/`):

```bash
#!/bin/bash
tag=$1; d=$HOME/phase1/f3-$tag; mkdir -p "$d"; rm -f "$d"/*
echo $$ > "$d/pid"; date +%s.%N > "$d/start"
sleep 900 & echo $! > "$d/child"
while IFS= read -r l; do echo "line $(date +%s.%N)" >> "$d/lines"; done
echo "EOF $(date +%s.%N)" > "$d/eof"
wait
```

`f3-watch.sh TAG N` (uploaded to `~/phase1/`):

```bash
#!/bin/bash
tag=$1; n=${2:-15}; d=$HOME/phase1/f3-$tag; p=$(cat $d/pid); c=$(cat $d/child)
for i in $(seq 0 $n); do
  ws=$(ps -o ppid=,stat=,etimes= -p $p 2>/dev/null | xargs); cs=$(ps -o ppid=,stat= -p $c 2>/dev/null | xargs)
  pp=$(echo $ws | cut -d' ' -f1); pc=$( [ -n "$pp" ] && ps -o comm= -p $pp 2>/dev/null)
  echo "t+${i}s wrapper[$p]: ${ws:-GONE} parent=${pc:--} | child[$c]: ${cs:-GONE} | eof: $(cat $d/eof 2>/dev/null || echo none)"
  [ $i -lt $n ] && sleep 1
done
```

`f3-local.sh TAG [ssh options]` (question 3). Run as `f3-local.sh mux -o ControlMaster=auto`, then `f3-local.sh direct -o ControlMaster=no -o ControlPath=none`.

```bash
#!/bin/bash
cd /tmp/bpf1; ID=$(cat out/s1.id); F=/tmp/bpf1/ssh/config_$ID; H=bpf-$ID; tag=$1; shift
ssh -F $F -O check $H
sleep 1000 | ssh -F $F "$@" $H "bash ~/phase1/f3-remote.sh $tag" &
SSHPID=$!; WRITER=$(pgrep -n -f '^sleep 1000$')
for i in $(seq 1 20); do ssh -F $F $H "test -s ~/phase1/f3-$tag/child" && break; sleep 0.5; done
echo "SIGKILL local ssh client at $(date +%s.%N)"; kill -9 $SSHPID
ssh -F $F $H "bash ~/phase1/f3-watch.sh $tag 15"
kill $WRITER; ssh -F $F $H "bash ~/phase1/f3-watch.sh $tag 5"
ssh -F $F $H "ps -eo pid,ppid,stat,etimes,cmd | grep -E '[f]3-remote|[s]leep 900'"
```

Question 4 markers, run over SSH:

```bash
ssh -F $F bpf-$ID 'bash -s' <<'EOF'
mkdir -p ~/.cache/bp-phase1 ~/.cache/pip ~/keep
echo cache-marker > ~/.cache/bp-phase1/marker; echo pipcache > ~/.cache/pip/bp-marker; echo keep-marker > ~/keep/marker
sudo mkdir -p /opt/bp-phase1 && echo opt-marker | sudo tee /opt/bp-phase1/marker >/dev/null
echo vartmp-marker > /var/tmp/bp-phase1-marker; echo tmp-marker > /tmp/bp-phase1-marker
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q sl
cat /proc/sys/kernel/random/boot_id > ~/keep/boot_id_create
nohup sleep 3000 >/dev/null 2>&1 &
sync
EOF
```

`f-cycle.sh TAG RESUME_BODY [stop-only|resume-only|both]` (questions 4, 5, 6). Runs were `B '{}' both`, then `C '' stop-only` (preceded by writing `~/keep/late-marker`), then `D '{"type":"default"}' resume-only`, then `E '' stop-only`.

```bash
#!/bin/bash
cd /tmp/bpf1; ID=$(cat out/s1.id); tag=$1; RB=$2; what=${3:-both}
L() { ./api.sh GET /limits 2>/dev/null | jq -c '{starts:(.starts|{minute:.minute.used,hour:.hour.used,day:.day.used}),activeSandboxes}'; }
if [ "$what" != resume-only ]; then
  T0=$(./now.sh); export T0
  HDR=out/stop-$tag.hdr ./api.sh POST /sandboxes/$ID/stop '{}' | jq -c '{ok,type,status,sstate:.sandbox.state}'
  ./waitstate.sh $ID '^(archived|error)$' 0.5; echo "limits after stop: $(L)"
fi
[ "$what" = stop-only ] && exit 0
echo "limits before resume: $(L)"; T0=$(./now.sh); export T0
HDR=out/resume-$tag.hdr ./api.sh POST /sandboxes/$ID/resume "$RB" | tee out/resume-$tag.json | jq .
grep -iE '^(ratelimit|retry-after)' out/resume-$tag.hdr; echo "limits right after resume: $(L)"
./waitstate.sh $ID '^(ready|idle|running|error|cancelled)$' 0.5
./sshkey.sh $ID
ssh -F ssh/config_$ID -o ControlMaster=auto bpf-$ID 'echo "boot_id now $(cat /proc/sys/kernel/random/boot_id) at create $(cat ~/keep/boot_id_create)"; hostname; pgrep -a -f "^sleep 3000" || echo "no live sleep 3000"; for f in ~/.cache/bp-phase1/marker ~/.cache/pip/bp-marker ~/keep/marker ~/keep/late-marker /opt/bp-phase1/marker /var/tmp/bp-phase1-marker /tmp/bp-phase1-marker; do printf "%-36s %s\n" $f "$(cat $f 2>/dev/null || echo MISSING)"; done; dpkg -s sl | grep ^Status || echo "sl: NOT INSTALLED"'
```

Saved snapshot round trip (question 1), on the stopped sandbox:

```bash
./api.sh POST /saved-snapshots "{\"sandboxId\":\"$ID\",\"name\":\"bp-phase1-test\"}"
until [[ $(./api.sh GET /saved-snapshots/bp-phase1-test 2>/dev/null | jq -r .snapshot.status) =~ ^(ready|failed)$ ]]; do sleep 2; done
./api.sh DELETE /saved-snapshots/bp-phase1-test
```

Delete (question 1 and cleanup):

```bash
EXTRA_HDR="X-Ascii-Confirm-Delete: $ID" ./api.sh DELETE /sandboxes/$ID | tee out/delete.json
./api.sh GET /deletion-operations/$(jq -r .operation.id out/delete.json)
./api.sh GET '/sandboxes?label=boatparty.phase1%3Dtrue'
```

## Desktop streaming and host CPU

A 2026-11-26 scope probe against `POST /sandboxes/bx_00000000/desktop` with the older scoped key returned 403 and named the missing action `desktop`. New keys include it. The endpoint returns a private `desktopUrl` and can report `desktop_not_ready` during startup. The boat image can run lightdm, sunshine, moonlight-web, ascii-streaming-audio and gnome-remote-desktop as system services. Stopping the active streaming stack frees CPU for builds.

Linux reports steal in the eighth CPU counter on the aggregate `cpu` line of `/proc/stat`. boatparty samples immediately around the command. Guest counters are already included in user and nice time.
