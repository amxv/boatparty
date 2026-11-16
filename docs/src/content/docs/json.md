---
title: JSON output
description: The --json conventions every boatparty command follows, and where the full contract lives.
order: 6
category: Reference
summary: One document per command, frames for streaming commands, error codes.
---

Every command accepts `--json` (before its own arguments). The full contract, with the shape of every command's output and every error code, is [`docs/json.md`](https://github.com/amxv/boatparty/blob/main/docs/json.md) in the repository; a test checks every command's output against it.

## Conventions

- Non-streaming commands print exactly one JSON document on stdout, with `"ok": true` on success.
- `run`, `serve`, `pipe` and `shell -- CMD` print newline-delimited frames instead: `{"type":"stdout","data":...}`, `{"type":"stderr","data":...}` and `{"type":"event","event":...}` as they happen, then exactly one `{"type":"result",...}`. Output that is not valid UTF-8 travels as `"base64"` instead of `"data"`.
- A failure is `{"ok": false, "error": {"code": "...", "message": "..."}}` (with `"type": "result"` for a streaming command), and the process exits 125. The `code` is stable; the message is for people.
- boatparty's own messages stay on stderr, prefixed `boatparty: `.
- No document carries a secret, with one deliberate exception: `expose` returns its URL, which may carry an access token.
