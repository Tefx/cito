# macOS HUD sender

Call `showBanner(payload, options?)` from `src/banner/index.ts`. Both Codex and Pi
use this local visibility path, including passive (`effective_wake: false`)
notifications. A successful result is `{ displayed: true }` after the HUD
acknowledges the payload. An unavailable/contended HUD returns
`{ displayed: false, reason }` and logs a best-effort skip (`diagnostic` overrides
the logger). Filesystem/compiler failures reject; callers must keep their
persistent delivery outcome independent of this best-effort visibility result.

`payload` uses the wire field names: `target_harness` (`codex` or `pi`),
`target_session_id`, `target_title`, `from_user`, `from_host`, `from_title`,
`message`, `effective_wake`, and optional `count`. Each acknowledged update
replaces the existing panel content and resets its dismissal timer. Passive
subtitles start with `[仅知会]`.

## Runtime and synchronization

The macOS host needs the Apple Swift compiler at `/usr/bin/swiftc` (Command Line
Tools) and macOS's `/usr/bin/perl` with its built-in `Fcntl` and `Time::HiRes`
modules. There are no npm native dependencies. Perl holds a real kernel
`flock(2)` on an open descriptor until the Node parent closes its pipe; it also
releases the lock if the parent exits. `acquireFlock(path, timeoutMs)` is exported
for same-inode interoperability, returning `null` on contention/timeout. Always
`await lock.release()` in `finally`.

The sender acquires `sockets/banner.lock` with a 2500ms wait budget. It tries the
existing socket, probes the lifetime `banner-instance.lock` without blocking,
and only removes a stale endpoint/spawns after a successful free-instance probe.
A busy instance gets a 500ms readiness poll, a 300ms sender-lock yield, and one
reconnect/re-probe. A still-busy instance is skipped without unlinking or spawning.
The native process holds its instance lock until exit and holds both locks
through unlink, click handling or fade-out. Timer retirement defers when a
sender holds `banner.lock`; clicks wait up to 500ms for that lock.

`ensureBannerCompiled(stateDir, sourcePath?)` compares source/executable mtimes,
compiles to a temporary file, sets mode 0755, and atomically renames it into
`bin/cito-banner`. **Its caller must hold `banner.lock`**; `showBanner` does this.
The default source lookup supports `src/` execution and the repository's
`dist/src/` TypeScript output. Installed layouts that move the Swift resource
must pass `sourcePath`. Compilation errors propagate; failed builds leave no
partial published executable.

The native listener binds relative to its sockets working directory. Node
connects through a private short-lived `/tmp/cito-hud-*/s` directory symlink,
removed in `finally`. This avoids Darwin's 104-byte Unix-socket path limit
without changing Node's process-global working directory. Lock files and socket
use mode 0600; newly created state subdirectories use mode 0700.

## Environment and click behavior

`CITO_STATE_DIR` selects isolated state (shared config defaults to `~/.cito`).
On a Codex click, the Swift process uses libsqlite3 to increment only the target
Codex row's `human_event_seq`, with a parameterized update and a 1000ms SQLite
busy timeout, before opening `codex://threads/<encoded-session-id>` via
`NSWorkspace`. Missing/uninitialized/busy databases produce stderr diagnostics
and do not prevent dismissal or the URL request. Pi clicks only dismiss.
Automatic dismissal never changes SQLite or opens a URL.

Testing controls, also usable via `options.env`:

- `CITO_BANNER_HEADLESS=1`: suppress panel construction; preserve the production
  socket server, presentation model, timer, locks and SQLite click handler.
  Presentation diagnostics (PID, subtitle, message) go to stderr.
- `CITO_BANNER_DISMISS_MS`: positive dismissal interval; default **6000ms**.
  Headless mode uses a 10ms retirement delay; visible mode fades for 250ms.
- `CITO_BANNER_URL_OPENER`: executable receiving the deeplink as its only
  argument, replacing `NSWorkspace.open`. Tests use an isolated recording script.
- `SIGUSR1`: invoke the same click retirement handler as a panel press.

`showBanner` detaches/unrefs the process and ignores its stdio. Direct launches
can capture native stderr for diagnostics. No desktop activation occurs until
a Codex user click requests its deeplink. The panel uses system colors and HUD
material for light/dark appearances, wrapping text, and a VoiceOver press action.

## Verification

```sh
npm run typecheck
npm test -- tests/banner/
```

The suite compiles/runs the real Swift binary with temporary `CITO_STATE_DIR`
values and a recording URL opener. It checks cold/concurrent startup, in-place
updates and timer reset, passive formatting, instance exclusion, timer deferral,
busy-instance skip, stale endpoint recovery, retirement handoff, and Codex/Pi
SQLite/URL behavior (including busy/uninitialized SQLite and literal parameters).
The handoff test unlinks the endpoint to expose the unavailable-socket window
while the old process is alive, then requests real click retirement during the
sender's Case A poll. This makes the release/re-probe/Case B transition
deterministic without replacing the native server or locks. Another test holds
the sender lock across timer expiry. Fixtures terminate any remaining native
processes before deleting their state. Visible panel layout, VoiceOver operation,
and foreground-app focus preservation still require interactive observation;
headless tests make no claim to having exercised those UI surfaces.
