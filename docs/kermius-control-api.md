# Kermius Control API — spec for Kolphin integration

Status: draft v1 · Consumer: Kolphin (file manager) · Implementer: Kermius (desktop only: Linux first, macOS/Windows later)

## Goal

Let another local app open and **drive** Kermius local terminal tabs: get a stable id back, `cd` a tab when the
user navigates, open extra tabs that run a program (`claude`, `opencode`), receive events when a tab closes or its
shell changes directory. The existing one-shot CLI (`--new-terminal --cwd`, `--share`) stays as is.

## Transport

- Unix domain socket, stream: `$XDG_RUNTIME_DIR/kermius/control.sock`
  (fallback when `XDG_RUNTIME_DIR` is unset: `/tmp/kermius-$UID/control.sock`).
- Directory mode `0700`, socket mode `0600`. Only the same user can connect; no other auth in v1.
- Created at start-up by the primary instance, removed on exit; a stale socket file (connect fails) is replaced.
- Windows (later): named pipe `\\.\pipe\kermius-control-<username>`, same protocol.
- Framing: one JSON object per line (UTF-8, `\n`-terminated). Several clients may be connected at once.

### Messages

Request (client → Kermius):
```json
{"id": 7, "method": "session.open", "params": {...}}
```
Response (exactly one per request, same `id`):
```json
{"id": 7, "result": {...}}
{"id": 7, "error": {"code": "not_found", "message": "no session 3f2a…"}}
```
Event (Kermius → client, no `id`):
```json
{"event": "session.closed", "params": {...}}
```

Error codes: `bad_request`, `unknown_method`, `not_found`, `busy`, `spawn_failed`, `internal`.

## Starting Kermius

If the socket is missing, the client runs `kermius --background` (new flag: start without stealing focus, window
may stay minimized/hidden until a request asks for focus) and retries connecting for up to 10 s.

## Methods

### `hello`
Params: `{"client": "kolphin", "version": "0.1.0"}`
Result: `{"app": "kermius", "version": "1.39.0", "api": 1, "capabilities": ["session", "share", "cwd_events"]}`
Must be the first request; others before it get `bad_request`.

### `session.open`
Open (or reuse) a local shell tab.

Params:
| Field | Type | Notes |
|---|---|---|
| `key` | string, optional | Idempotency key, e.g. `"kolphin-main"`. If a live tab owned by this key exists, return it (and `cd` it to `cwd` if given and idle) instead of opening a new one. |
| `cwd` | string, optional | Absolute path; missing/unreadable → home. |
| `title` | string, optional | Tab title; default = shell title behaviour. |
| `icon` | string, optional | Absolute path to an SVG/PNG shown on the tab instead of the default icon (Kolphin passes its logo). |
| `focus` | bool, default `true` | Raise the Kermius window and select the tab. |

Result: `{"session": "<uuid>", "reused": false}`

Tabs opened with a `key` are "owned": they show the icon, and the key is released when the tab closes.

### `session.run`
Open a **new** tab running a program in a PTY (not the user's shell). Never reuses a tab.

Params: `{"argv": ["claude"], "cwd": "/path", "title": "Claude · Kolphin", "icon": "/path/kolphin.svg", "env": {"K": "V"}, "focus": true}`
- `argv[0]` resolved via the user's login-shell `PATH` (Claude/OpenCode live in `~/.local/bin`, `~/.npm-global/bin`…),
  i.e. run as `$SHELL -lc 'exec "$@"' _ argv...` so the user's profile applies.
- When the program exits, the tab stays open with the exit status shown and drops to an interactive shell in the
  same `cwd` (like `hold`), so error output is not lost.

Result: `{"session": "<uuid>"}`

### `session.cd`
Params: `{"session": "<uuid>", "path": "/abs/path"}`

- Only acts when the shell is **idle at its prompt**: the PTY foreground process group equals the shell's process
  group (`tcgetpgrp(master) == shell pgid`). Otherwise return `error.code = "busy"` and do nothing — never type into
  vim, `claude`, a running build, etc.
- Writes ` cd -- '<path, each ' written as '\''>'\r` to the PTY (leading space keeps it out of bash history with
  `HISTCONTROL=ignorespace`).
- Never clear what the user is typing: if Kermius knows the input line is non-empty (OSC 133 prompt marks), return
  `busy`. Without OSC 133, send as is.
- No-op (success) if the shell's last reported cwd (OSC 7) already equals `path`.

Result: `{}`

### `session.focus`
Params: `{"session": "<uuid>"}` — raise window, select tab. Result `{}`.

### `session.close`
Params: `{"session": "<uuid>"}` — close the tab (SIGHUP to the PTY). Result `{}`.

### `session.list`
Params: `{"key": "kolphin-main"}` (optional filter)
Result: `{"sessions": [{"session": "<uuid>", "key": "kolphin-main", "cwd": "/home/u", "busy": false, "title": "…"}]}`
`cwd` = last OSC 7 value, else the spawn cwd.

### `share`
Params: `{"paths": ["/abs/a.png", "/abs/dir"]}` — same behaviour as `kermius --share`. Result `{}`.

## Events

Sent to every connected client that called `hello`.

| Event | Params | When |
|---|---|---|
| `session.closed` | `{"session", "key"}` | Tab closed by the user, by `session.close`, or the shell exited. |
| `session.cwd` | `{"session", "key", "cwd"}` | Shell reported a new directory via OSC 7 (`\e]7;file://host/path\a`). Only for sessions with a `key`. |

Kermius should make sure OSC 7 is emitted: for bash/zsh spawned by Kermius, inject a `PROMPT_COMMAND` /
`precmd` hook printing OSC 7 (VTE's `vte.sh` already does this for bash on Ubuntu when `VTE_VERSION` is set; do not
rely on it). Percent-decode the path.

## Behaviour Kolphin relies on

1. `session.open` with the same `key` twice → one tab.
2. User closes the owned tab → `session.closed` arrives; the next `session.open` with that key creates a fresh tab
   without any error dialog.
3. `session.cd` on a closed session → `not_found` (Kolphin then calls `session.open` again).
4. Kermius quitting closes the socket; clients see EOF and treat all sessions as gone.
5. Requests are answered in under 100 ms (spawning may complete asynchronously; the id is returned immediately).
6. `session.close` releases the `key` synchronously, before its response is sent: a `session.open` with the same key
   right after must create a new tab, never return the closing one.
   **Bug in 1.39.0:** `close` then immediate `open` returns `{"reused": true}` with the closing session's id, and that
   tab disappears a moment later (`session.list` is then empty).

## Acceptance tests (Kermius side)

Using `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/kermius/control.sock`:
1. `hello` → result with `"api": 1`.
2. `session.open {"key":"t","cwd":"/tmp"}` twice → same `session`, second has `"reused": true`, one tab visible with the given icon.
3. `session.cd` to `/var` → prompt shows `/var`; a `session.cwd` event with `/var` is received.
4. Run `sleep 30` in the tab, `session.cd` → `busy`, nothing typed.
5. Close the tab by hand → `session.closed` event; `session.cd` → `not_found`.
6. `session.run {"argv":["bash","-c","echo hi; exit 3"]}` → tab shows `hi`, exit status 3, then a shell prompt.
7. A path containing spaces and `'` cds correctly.
8. Two clients connected: both receive events.
9. `session.open {"key":"r"}` → wait 1 s → `session.close` → immediately `session.open {"key":"r"}` → new id,
   `"reused": false`, and the tab is still listed 2 s later.
10. Fresh tab: `busy` is `true` while the shell starts, then `false` at the prompt; typing `sleep 60` makes it `true`
    again and `session.cd` returns `busy` without typing anything.
