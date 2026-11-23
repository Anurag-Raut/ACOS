# ACOS sessions in T3 Code's Device panel — spec

Status: **agreed with the owner 2026-11-23, not started.** Starts after v0.4.1 ships
(PR #1 merged by the owner). Delivered as **one PR to `main`**; the lead gets it
ready, the owner merges.

## 1. Goals and non-goals

**Goals**

1. A live ACOS agent session appears in T3 Code's **Device panel**, like a running
   Android emulator, with a live picture.
2. A **human can tap, swipe and type** into it from the panel.

**Non-goals**

- General adb compatibility. We implement only what T3 Code's device hub needs.
  `adb install`, `logcat`, `reverse`, `forward`, `push`, `pull`, `jdwp`, `root` and
  `emu` console commands are out of scope. They answer honestly as unsupported.
- A phone-like "device" with a home screen that outlives agents (owner chose
  option (b): agent sessions show up as devices; see §9 Q1).
- Take/release control. Input is **free-form** like T3's own emulators: human and
  agent input both land, and each is labelled in the event feed (§6).

## 2. How T3 Code finds and drives an Android emulator (facts)

Source: T3's bundled `expo-device-hub` 0.22.1, `vendor/serve-emu/dist/`
(in `~/.t3/tools/expo-device-hub/0.22.1/node_modules/expo-device-hub/`). That code
is the reference. Re-read it if the version changes.

- **Discovery:** `adb devices` (`adb.js: listAllDevices`), keeping state `device`.
  A serial matching `^emulator-(\d+)$` is an emulator. Its number is the **console
  port**. Emulators get the `grpc` stream modes; any other serial gets `scrcpy`
  only (`device-capabilities.js`).
- **gRPC endpoint:** `emulator-grpc.js: findEmulatorGrpcEndpoint`.
  - It scans `pid_<pid>.ini` / `pid_<pid>_info.ini` files in several directories:
    `$XDG_RUNTIME_DIR/avd/running`, `~/.android/avd/running`, `$TMPDIR/avd/running`
    and others.
  - It skips a file whose pid is not alive.
  - It matches `port.serial` to the console port.
  - It reads `grpc.port`, `grpc.token` (bearer, optional; a warning is logged
    without it) and `avd.name`.
  - **Fallback** (only when no file is found): `adb emu grpc <port>` through the
    emulator console. We do **not** implement the console; the ini file is the
    supported path.
- **Video:**
  - gRPC `android.emulation.control.EmulatorController/streamScreenshot`, with
    `getScreenshot` as fallback.
  - Format RGB888 (`IMG_FORMAT_RGB888 = 2`) or RGBA8888 (`= 1`).
  - The hub turns the frames into H.264 itself with host `ffmpeg` (`h264-encoder.js`).
  - An optional MMAP shared-memory path (`grpc-mmap.js`) is not needed.
- **Input:** gRPC `sendTouch` and `sendKey` (`grpc-session.js`, `input.js`).
- **Panel side info (`adb shell`, via `adb.js`):**
  - `wm size`, `wm density`
  - `dumpsys window displays` (rotation)
  - `settings get system font_scale`
  - `cmd uimode night`
  - `settings get global …` (reduce motion / animator scales)
  - `settings get secure high_text_contrast_enabled`
  - `settings get secure font_weight_adjustment`
  - `settings get secure show_ime_with_hard_keyboard`
  - network status (`wifi_on`, `mobile_data`)
  - `exec-out screencap -p` (screenshot button)
  - the matching `settings put` / `cmd window user-rotation` / `wm density` /
    `svc wifi|data` setters
  - `uiautomator dump` (accessibility) and `dumpsys activity` (foreground app),
    from `accessibility.js` and `foreground-component.js`
- **adb's emulator scan:** the adb server finds local emulators by connecting to
  **`127.0.0.1:<console+1>`** for console ports **5554, 5556, … 5584**. That is
  **16 slots**. The serial is `emulator-<console>`.

Reference outputs captured from a real phone (moto g35, API 35, 2026-11-23):

```
$ wm size                                    Physical size: 1080x2400
$ wm density                                 Physical density: 400
$ settings get system font_scale             1.0
$ cmd uimode night                           Night mode: no
$ settings get secure high_text_contrast_enabled   0
$ settings get secure font_weight_adjustment       0
$ settings get secure show_ime_with_hard_keyboard  1
$ settings get system user_rotation          0
$ dumpsys window displays   (rotation line)  … mDisplayRotation=ROTATION_0 … mRotation=ROTATION_0} …
```

The parsers in `adb.js` are the contract. Every answer must parse there.

## 3. What appears, and when

- **Opt-in per server:** `acos serve --adb`, or the equivalent MCP-config line.
  With it off, nothing changes anywhere: no listener, no ini file, no thread, no
  cost.
  - **Why opt-in:** the user's adb server is shared with real phones (moto g35).
    Tools that run `adb` without `-s` fail as soon as two devices are online.
- **Which sessions:** every live ART session owned by that `acos serve`.
  - CLI-only sessions (`acos launch` without serve) are not exposed in v1.
- **Lifetime:** a device appears once the session is ready. It disappears when the
  session is destroyed, expires (idle 600 s) or its server exits. That includes
  crashes: slot locks and ini files are tied to the owning pid (§4.4).
- **`acos verify` runs:** when one runs under an `--adb` server, it is shown
  **watch-only**. Panel input is dropped, and the event feed records
  `panel input ignored: verify run`. A verdict is never influenced by a human.
- **Cap: 16** (adb's scan range).
  - A session beyond the cap still runs normally.
  - Its launch reply carries `device_panel: {"exposed": false, "reason": "all 16 adb emulator slots in use"}`.
- **Name shown:** `avd.name = ACOS <package> <short-session-id>`.

## 4. Design

### 4.1 Components

```
 T3 device hub ──adb devices / adb shell──► adb server ──TCP 127.0.0.1:<console+1>──► (A) adb endpoint ┐
       │                                                                               (in acos serve) │
       └──gRPC 127.0.0.1:<grpc.port> (bearer token)──► (B) acos-devbridge ◄──unix socket──────────────┤
                                                       (separate binary)                               ▼
                                                                                  ACOS session (frames, input, state)
 ~/.android/avd/running/pid_<session-pid>.ini  ◄── written by acos serve per exposed session (C)
```

- **(A) adb endpoint**, inside `acos serve` with plain threads (no async runtime).
  - One TCP listener per exposed session on `127.0.0.1:<console+1>`.
  - It speaks the adb wire protocol for the subset in §4.2.
- **(B) `acos-devbridge`**, a new small binary.
  - It is the only ACOS code that uses `tokio`, `tonic` and `prost`. That needs a
    one-time crates.io download, pinned in `Cargo.lock`; it is approved.
  - `acos serve --adb` starts it lazily on the first exposed session and stops it
    with the server.
  - It serves the gRPC subset in §4.3 for every exposed session of its server,
    on one port per session.
  - It talks to `acos serve` over a private unix socket (same uid, 0700 directory,
    `SO_PEERCRED` checked, like the live-view stream socket).
- **(C) Discovery files**, written by `acos serve`. One `pid_<session-pid>.ini` per
  exposed session, in `~/.android/avd/running/`, where `<session-pid>` is that
  session's own `acos-art` process. That name fits the hub's
  `pid_<digits>(_info)?.ini` pattern and is unique per session, and the pid is
  alive exactly as long as the session is.
  - The file is deleted on session end.
  - If the process dies, the hub ignores the leftover file (dead pid), and
    `acos serve` sweeps its own stale files at start.
  - Contents:
    ```
    port.serial=<console>
    port.adb=<console+1>
    grpc.port=<grpc port>
    grpc.token=<random 128-bit hex>
    avd.name=ACOS <package> <short-id>
    ```

### 4.2 adb endpoint subset (A)

- **Handshake:** answer the server's `CNXN` with `CNXN` and a banner such as
  `device::ro.product.name=acos;ro.product.model=ACOS;ro.product.device=acos;features=shell_v2,cmd`.
  - No `AUTH`, as for an emulator with `ro.adb.secure=0`.
- **`shell_v2` is required.** It gives the hub real exit codes, so an unsupported
  command fails (non-zero) instead of looking like success.
- **Services:**
  - `shell:<cmd>` and `shell,v2,…:<cmd>`
  - `exec:<cmd>` (for `exec-out screencap -p`)
  - `host-serial` features as adb needs them.
  - Anything else: `FAIL` with `acos: unsupported adb service: <name>`.
- **Shell commands:** a host-side translator. There is no shell in the guest.
  - Exactly the hub's commands from §2.
  - Getters return values from the session, e.g. `wm size` = the session's
    display size, and `dumpsys window displays` gives the real rotation.
  - Setters either change the session through existing ACOS paths, such as
    rotation via the existing `rotate` action, or exit 1 with
    `acos: unsupported: <cmd>`. They never fake success.
  - `exec-out screencap -p` returns the current frame as PNG.
  - `uiautomator dump` returns the UI tree as standard uiautomator XML, built from
    `observe_tree`.
  - Every other command exits 1 with `acos: unsupported: <cmd>`.
  - `adb install` (`pm install` / `cmd package install`) exits 1 with
    `acos: install into a live session is not supported; launch a new session with the APK`.

### 4.3 gRPC subset (B)

Use AOSP's `emulator_controller.proto` (Apache-2.0), vendored under the crate with
its licence. Methods:

| Method | Behaviour |
|---|---|
| `getScreenshot(ImageFormat)` | current frame, RGB888 or RGBA8888, full display size, rotation set |
| `streamScreenshot(ImageFormat)` | push a frame on every composed change (not polling), capped at 30 fps; nothing sent while the screen is still |
| `sendTouch(TouchEvent)` | becomes ACOS pointer input: down/move/up, multi-touch ids, display coordinates |
| `sendKey(KeyboardEvent)` | becomes ACOS key/text input: keycodes, text, BACK/HOME/APP_SWITCH (HOME/APP_SWITCH answer per current ACOS support, never faked) |
| `getStatus`, other calls the hub makes | minimal truthful answers; everything else `UNIMPLEMENTED` |

- Every call requires `authorization: Bearer <grpc.token>`.
- The listener binds 127.0.0.1 only.

### 4.4 Slots, locks and cleanup

- Slot `k` (0–15): console port `5554 + 2k`, adb port `5555 + 2k`.
- Claiming a slot means holding an `flock` on `$XDG_RUNTIME_DIR/acos-adb/slot-<k>.lock`
  (`O_CLOEXEC`) and binding the adb port.
  - If the port is busy (a real emulator, another tool), skip to the next slot.
- On session end: close the listener, tell the bridge, delete the ini, release
  the lock.
- On a crash, the kernel drops the flock and the listener. Stale ini files are
  ignored by the hub (dead pid) and swept on the next serve start.

### 4.5 Frames

- The live-view producer (`docs/LIVE_VIEW.md`) gains a **raw RGB888 full-size,
  change-driven** subscription format next to PNG.
- Frames are produced only while a subscriber exists. The existing rule
  ("no subscriber = one atomic load") still holds.

## 5. Decision change: ADR-0027 §5 amended

ADR-0027 §5 said "the viewer is view-only". The new ADR (next free number) says:

- Viewers **may send input**, but only through the device-panel path: the gRPC
  bridge, local and token-protected.
- Input is free-form (no control lock).
- Every human input is recorded with `source: panel`.
- `acos verify` runs ignore it.
- `acos view` itself stays view-only.

## 6. Event feed labelling

Every input event in a session's action/event feed (and in `acos_logs` / the
view hub) carries `source`:
- `agent`: ACOS tools / MCP
- `panel`: gRPC from the T3 hub
- `adb`: `adb shell input …` if it is ever supported; in v1 it is unsupported

An agent that sees an unexpected screen can check the feed for `source: panel`.

## 7. No cost when unused (hard requirement)

- Without `--adb`: no threads, listeners, files or bridge process. Tracked
  benchmarks are identical to v0.4.1 (idle CPU, memory, latency lattice).
- With `--adb` but no panel open: per session, one idle listener and one ini file.
  - Idle CPU and wakeups must stay within the gate's noise bands.
  - The bridge is idle: no frame subscription until `streamScreenshot` is called.

## 8. Acceptance (definition of done)

**Automated** (in the PR, using the real `/usr/bin/adb` with a private
`ANDROID_ADB_SERVER_PORT` so the user's adb server and phone are untouched):

1. `adb devices` lists `emulator-55xx device` for a live `--adb` session. The
   device is gone within 5 s after `acos_destroy`.
2. Each hub shell command from §2 returns output that the **hub's own parsers** in
   `adb.js` accept.
   - Test this by running those functions from the vendored hub copy under node,
     pointed at our device, if node is available. Otherwise use parser ports
     checked against the same golden strings.
3. An unsupported shell command and `adb install` exit non-zero with the
   `acos: unsupported…` text.
4. A small gRPC test client:
   - `getScreenshot` returns a frame of the right size, with pixels matching
     `acos observe --screenshot`;
   - `streamScreenshot` delivers a new frame after a UI change and none while
     the screen is idle;
   - `sendTouch` on a button changes the app's UI, checked by `observe_tree`;
   - `sendKey` types text into an EditText.
5. The event feed shows `source: panel` for gRPC input and `source: agent` for tool
   input.
6. A verify run under `--adb` ignores panel input; its verdict is unaffected and
   the feed says so.
7. 17 concurrent sessions: 16 exposed, the 17th runs and reports `exposed: false`.
8. Crash cleanup: kill `acos serve -9`. The device disappears from `adb devices`,
   and the next serve start sweeps the stale ini.
9. Benchmarks: `acos_bench.py --phase memory` and the latency lattice, with
   `--adb` off, match v0.4.1 within gate noise. With `--adb` on and no panel,
   idle CPU stays within noise.
10. `tools/verify/check-layout.sh` passes. There are no raw evidence files in the
    PR, only `.md` reports.

**Manual, by the owner (once):** open T3 Code's Device panel with an agent session
running under `acos serve --adb`. The session appears, shows the app live, and taps
and typing work.

## 9. Owner decisions (grilling session 2026-11-23)

| # | Decision |
|---|---|
| Q1 | Agent sessions show up as devices while alive (b); no phone-like home-screen device |
| Q2/Q7 | Owned by each `acos serve`; devices vanish with it; 16-slot shared allocator; CLI sessions not exposed |
| Q4/Q17 | Host-side shell translator for the hub's commands only; everything else honest unsupported |
| Q5/Q17 | No general adb features (install/logcat/reverse/forward/push/pull) |
| Q6 | No cost when unused: hard requirement |
| Q10 | `acos verify` runs are watch-only in the panel |
| Q11 | `adb install` → honest unsupported |
| Q12 | Opt-in per server (`acos serve --adb`) |
| Q13 | Input is labelled by source in the event feed |
| Q14 | Writing discovery files under `~/.android/avd/running/` is OK |
| Q15 | Acceptance as §8 |
| Q16/Q18 | One PR to `main`, after v0.4.1 |
| Q20 | gRPC in a separate binary (`acos-devbridge`); crates.io download of tokio/tonic/prost approved |
| Q21 | Free-form input, no take/release, no idle timer, no `HUMAN_IN_CONTROL` |

## 10. Risks / unknowns to settle in the PR

- **adb's emulator scan timing.** How fast the adb server notices a new
  `127.0.0.1:<console+1>` listener. Measure it. If it is slow, the serve may also
  run `adb connect`, but that produces a `127.0.0.1:port` serial, which is
  **not** an emulator to the hub. So the scan path is the one we need.
- **Exact `ImageFormat`/rotation fields the hub needs** (`grpc-session.js`).
  Implement against the hub's encoder/decoder, not memory.
- **Whether the panel needs more calls than §2** when opened: device grid,
  capabilities, app info, permissions. Log every adb service, shell command and
  gRPC method the hub sends during the manual check. Anything missing becomes a
  §2 addition or an honest unsupported.
