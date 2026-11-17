# WVB — host WebView broker (ADR-0031), production crate + gate re-measurement

Branch `wvb` (from `spike-wv`). Crate `crates/acos-webview` — **host side only**,
no guest code, nothing in `acos-services` or `fixtures/apps/webview-provider`
touched.

Status: **DONE** — the broker is implemented and the ADR-0031 gate passes against
the real pinned engine over the real wire protocol.

## 1. What the crate is

| module | responsibility |
|---|---|
| `wire.rs` | the guest protocol, framed and normative with `WIRE.md` §1–§3 (kind byte, 64 MiB cap, `g`/`h` id spaces, blob follows its JSON) |
| `cdp.rs` | DevTools client on `--remote-debugging-pipe` (fds 3/4, NUL-terminated JSON, reader thread, per-session event queues, timeouts). **No TCP port exists.** |
| `engine.rs` | the pinned install: Chrome for Testing 157.0.8103.78, URL + SHA-256, `curl` + `sha256` + `unzip`, staging dir, `VERIFIED.json`, archive dropped after install |
| `sandbox.rs` | the bwrap argv the engine runs under, and the throwaway profile dir |
| `broker.rs` | engine supervisor: one engine per host, lazy start, crash restart, idle shutdown, sessions (browser contexts), views, PSS |
| `view.rs` | one target: viewport/device metrics, navigation + callbacks, JS both ways (binding + shim), frames, input, Fetch interception |
| `ax.rs` | `Accessibility.getFullAXTree` ⨝ `DOMSnapshot.captureSnapshot` → nodes with display-coordinate bounds |
| `server.rs` | the per-session Unix socket: framing, dispatch, per-view event pumps, host→guest requests (`nav.shouldOverride`, `js.call`, `req.intercept`) |

Engine identity reported to the guest and in verdicts:
`HeadlessChrome/157.0.8103.78`, revision `@4b3d0dabb8250b6f152d2d85d79e7706e309d087`,
protocol `1.3` (`fidelity_note()` says host Blink, not the phone's WebView build).

## 2. Gate (real engine, real wire, `results/handoff/WVB/evidence/gate.json` + the raw console log `evidence/gate-run.log`)

Run: `cargo test -p acos-webview --test engine_gate -- --nocapture --test-threads=1`
under `run-exclusive.sh --task WVB --exclusive` + `guarded.sh --mem 8G --time 600`
with `ACOS_WEBVIEW_TEST=1`. Wall time 5.7 s, test result `ok. 1 passed`.

| ADR-0031 item | measurement | result |
|---|---|---|
| page loads (local page served by the test) | `http://127.0.0.1:<port>/` → title `acos-wv-test`, 8.3 ms engine-side / 15.8 ms round trip | **PASS** |
| JS host → page | `document.getElementById('n').textContent` → `"hello-page"` | **PASS** |
| JS page → host → page | `window.acosEcho.echo('ping')` → provider saw `js.call{method:"echo",argsJson:"[\"ping\"]"}` → page got `"ping"`; a provider error rejects the promise (`"rejected:JS_EXCEPTION: asked to fail"`) | **PASS** |
| tap web text by AX | AX `button "Tap me"` bounds `[0,209.7,630,210]` display px → `input.tap` centre (315,315) → page state `idle` → `tapped` | **PASS** |
| frame < 50 ms @1080×2400@2.625 | 20 frames: p50 **11.7 ms**, p95 **15.6 ms**, max 15.6 ms, PNG 57 KB | **PASS** |
| ≤ 60 MiB per extra session | PSS 1 context **211.2 MiB**, 3 contexts **218.9 MiB** → **+3.9 MiB per extra browser context**; fixed engine cost **130.3 MiB PSS / 8 processes** (targets in one context: 1) | **PASS** |
| guest-served assets | `file:///android_asset/index.html` + `app.js` intercepted (`req.intercept`, 2 pauses), page title became `asset-js-ok` | **PASS** |
| structured failures | `NO_VIEW`, `UNSUPPORTED`, `INVALID_VIEWPORT` (never a silent success) | **PASS** |
| idle shutdown | no views for 3 s → `state:"down"`, engine pid reaped (`/proc/<pid>` gone) | **PASS** |
| lazy restart | fresh `hello` → new engine generation, view + JS work, `restarts:0` (idle stop is not a crash) | **PASS** |

Unit tests (`cargo test -p acos-webview --lib`): 17 passed — framing (round trip,
ordering, zero/oversized/truncated/unknown-kind rejection), response/event shapes,
engine pin paths + marker rejection, sandbox argv (home masked before the profile
bind, engine flags after `--`), profile throwaway, viewport validation, base64,
key table.

`tests/engine_bare.rs` (no bwrap) proves the same binary speaks DevTools over the
pipe — it exists so a host/sandbox regression is attributable.

## 3. Measured surprises (all three cost a debugging round; all are pinned by a
test or a comment in the code)

1. **`dup2(fd, 3)` is a no-op when `fd == 3`, and a no-op keeps `O_CLOEXEC`** →
   the engine exits with *"Remote debugging pipe file descriptors are not open"*
   and no stderr. `cdp.rs` now skips the `dup2` and clears `FD_CLOEXEC`
   explicitly.
2. **An empty-root bwrap layout (`--tmpfs /` + per-path `--ro-bind`s) kills the
   headless shell with SIGTRAP and no output.** The sandbox binds the host root
   read-only in one step and masks `/home`, `/root`, `/run/user` with tmpfs
   instead (the profile is bound back *after* the mask — mount order matters,
   and the argv test asserts it).
3. **DevTools input takes CSS pixels, not device pixels.** The AX export is in
   display coordinates by contract, so `view.rs` divides by `density` on the way
   in; the first gate run tapped 2.625× too far right and down and the click
   never landed. `WIRE.md` §4 now states the conversion explicitly.

## 4. Sandbox

`bwrap --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try
--die-with-parent --new-session --cap-drop ALL --clearenv --ro-bind / /
--tmpfs /home --tmpfs /root --tmpfs /run/user --proc /proc --dev-bind /dev /dev
--tmpfs /tmp --tmpfs /dev/shm --ro-bind <engine-dir> <engine-dir>
--bind <profile> <profile> --setenv HOME=<profile> … --chdir <engine-dir> --
<engine> …`

* the user's home, `/root` and `/run/user` are empty inside, so no
  `~/.config/google-chrome`, no `~/.config/chromium`, no `~/.ssh`;
* the only writable paths are the throwaway profile (0700, removed on drop, a
  fresh one per engine start) and tmpfs;
* network stays shared with the host (the page fetch happens on the host, per
  ADR-0031 item 1) and the guest never gets a DevTools endpoint;
* Chromium's own namespace sandbox is left enabled (it works in this layout —
  measured).

## 5. Reproduce

```sh
cd <worktree>
export ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvb-target
# fast, no lock needed
cargo test --offline -p acos-webview --lib
# heavy: real Chromium (queue; --exclusive because it measures frames + PSS)
tools/art-runtime/run-exclusive.sh --task WVB --exclusive -- tools/guarded.sh \
  --mem 8G --time 600 -- env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvb-target \
  cargo test --offline -p acos-webview --test engine_gate -- --nocapture --test-threads=1
```

Without `ACOS_WEBVIEW_TEST=1` both heavy tests print `SKIP …` and return; they
never report a pass they did not measure.

## 6. Engine install (shared cache)

`engine.rs` resolves the cache as `$ACOS_WEBVIEW_CACHE` → `$ACOS_HOME/.cache/webview`
→ `<checkout>/.cache/webview`, and installs
`engine/157.0.8103.78/chrome-headless-shell-linux64/chrome-headless-shell`
plus `VERIFIED.json` on first use. The gate run installed it and the verified
tree was moved to the shared cache:

```
/home/Qui/Projects/acos/.cache/webview/engine/157.0.8103.78/   277 MiB
  VERIFIED.json  {version 157.0.8103.78, zipSha256 f9032972…d153a,
                  binary 211,094,864 B, binarySha256 8e4c5db8…7a96}
```

The 122 MiB zip is deleted after a verified install (the marker keeps its
digest); re-installing re-downloads and re-verifies. Nothing in the worktree
cache remains.

## 7. What is deliberately not here (and what the next step is)

* **No guest code and no framework wiring**: the provider (WV2), the
  `webviewupdate` peer and the PMS/multi-package foundation are other tracks.
  This crate is a library: `Broker` + `serve()` + `ServerConfig`.
* `render` emits **PNG only**; `format:"rgba"` is a structured `UNSUPPORTED`
  (decoding belongs in the guest, which has to draw it anyway — the spike's
  ~57–72 KB PNGs are the measured cost).
* `view.history` supports `back`/`forward`; `clear` is `UNSUPPORTED` with the
  reason (Chromium has no history-clear; the provider must replace the target).
* `shouldInterceptRequest` for `http(s)` is opt-in (`view.setInterceptAll`);
  the default pauses only `file://` and `content://` so a page load is not
  serialised behind the guest.
* Writes to `history`/`localStorage` per-session isolation is Chromium's browser
  context (already per session), not a per-view cookie jar.
