# WVB/WV2 wire protocol (guest WebView provider ⇄ host WebView broker)

Version **2** (WV7). Owner: WVB (host broker, `crates/acos-webview`). Consumer:
WV2 (guest provider). This file is normative: the broker's `wire` module in
`crates/acos-webview/src/wire.rs` implements exactly this and its unit tests are
the executable copy of §1–§3. Changes bump `proto` and are additive otherwise
(unknown ops answer `UNSUPPORTED`, never a silent success).

### v2 (WV7: the version is an integer)

`proto` is the **integer** `2` on both sides. A version is a number, never a
float: a guest that says `1`, `1.0`, WV6's `1.1` or anything else is answered
`PROTO_VERSION`, whose message names `2` (never a silent coercion). Everything
WV6 added under its `1.1` label is part of v2, and the provider/broker
implementations are unchanged for it:

* `view.setVisibility` (`{view, visible, windowVisible}`) — a hidden View tells
  the engine (`Page.setWebLifecycleState`: frozen/active) so the renderer stops.
* `view.settings` (`{view, settings:{…}}` → `{applied:[…], rejected:[…]}`) —
  settings changed after `view.create`. A rejected key is never silently
  dropped: the provider raises, naming the setting.
* `view.scrollTo` (`{view, x, y}` → engine `contentWidth/contentHeight/
  scrollX/scrollY`); the same four fields are added to every `render` response,
  so `compute*ScrollRange/Offset/Extent` answer from engine truth after the
  first frame (guest geometry only until then).
* `view.focusChanged` (host event `{view, focused, editable, type, tag}`) — the
  page's focused editable field, from `document.activeElement`; it backs
  `onCheckIsTextEditor`/`onCreateInputConnection`. The `ax` tree is exposed by
  the provider as virtual `AccessibilityNodeProvider` descendants (no new op).
* A connection-level error frame has id **`"g?"`** (a failed gate attach, or a
  second connection). Both are **fatal for every pending request** on that
  connection: the provider fails all of them with the frame's code (an attach
  failure is `ENGINE_UNAVAILABLE`) instead of waiting for their timeouts.

## 0. Transport

* One **Unix stream socket per session** (`SOCK_STREAM`). ACOS creates it at a
  host path it owns and bind-mounts it read-write into the guest; the guest path
  is `/run/acos/webview.sock` (the session sandbox binds it there; the socket fd
  is the only way out — the guest never sees a DevTools port, a target id or a
  Chromium pid).
* Nothing else is on the socket. No TCP, no WebSocket, no payload escaping
  beyond §1 (JSON is UTF-8, never base64-wrapped for frames).
* The guest connects once per session, sends `hello`, and keeps the connection
  for the session's life. `hello.proto` is `1.1` in v1.1. One connection per
  session socket; a second connection is rejected with `ALREADY_CONNECTED`.
* If the engine dies the broker restarts it, closes the socket with a final
  event `engine.restarted` (views are gone) and the guest provider must
  re-create its views. The broker never silently continues.

## 1. Framing (normative)

Every message is one **frame**:

```
frame   := uint32_be length, body[length]
length  := 1 .. 67_108_864            (64 MiB cap)
body[0] := kind
kind 1  := JSON message, body[1..] is UTF-8 JSON (a JSON *object*)
kind 2  := BLOB, body[1..] is:
             uint32_be nid, id[nid]        (UTF-8 id, ASCII)
             uint8 format                   0 = png, 1 = rgba8888
             uint32_be width, height, stride
             uint64_be seq                   (monotone per view, frame counter)
             data[rest]
```

* A `length` of 0 or > 64 MiB, or an unknown `kind`, is a fatal framing error:
  the peer sends one JSON frame `{"event":"fatal","code":"BAD_FRAME",...}` if it
  still can, then closes. Partial frames are never delivered.
* A BLOB's `id` **always** matches the `id` of the JSON response that precedes
  it on the same stream (`render`). Reading the JSON response and then the BLOB
  is the only legal order; the broker writes them back-to-back under one lock.
* BLOB data is `stride * height` bytes (`png`: `stride` is the byte length of
  the PNG and `height` is the view height in device pixels).

## 2. Messages

A JSON message has one of three shapes:

| shape | keys | direction |
|---|---|---|
| request | `{"id": "<dir><n>", "op": "<name>", …}` | both |
| response (ok) | `{"id": "<same>", "ok": true, "result": {…}}` | both |
| response (error) | `{"id": "<same>", "ok": false, "error": {"code","message","detail"?}}` | both |
| event | `{"event": "<name>", …}` (no `id`) | both |

* `id` = `"g<n>"` for guest→host requests, `"h<n>"` for host→guest requests
  (`n` decimal, per-direction counter, never reused within a connection). The
  prefix makes a misdirected reply detectable instead of a hang.
* Requests may be answered **out of order** and may be in flight concurrently
  (the broker handles them on a worker pool). The guest must not assume FIFO.
  Exceptions that are ordered on the wire: `render` JSON then its BLOB.
* Unknown `op` → `UNSUPPORTED`. Unknown JSON keys are ignored (forward
  compatibility). Unknown `event` names are ignored by the receiver.

## 3. Error codes (stable strings)

`BAD_REQUEST` (malformed JSON/params), `BAD_FRAME`, `PROTO_VERSION` (unsupported
`proto`), `ALREADY_CONNECTED`, `NO_VIEW`, `VIEW_EXISTS`, `INVALID_VIEWPORT`,
`NAVIGATION_FAILED`, `JS_EXCEPTION`, `TIMEOUT`, `UNSUPPORTED`,
`ENGINE_UNAVAILABLE`, `ENGINE_CRASHED`, `INTERCEPT_FAILED`, `FRAME_FAILED`,
`INTERNAL`. `message` is human text (< 2000 chars); `detail` is optional JSON.
An unsupported behaviour is an error, never a fabricated success.

## 4. Guest → host requests

`view` is a small integer chosen by the guest provider, unique per session
(one View instance = one target/page). All geometry is in **device pixels**
(display coordinates), `density` is the Android density (`1080x2400 @ 2.625
dpr`). The broker converts to the CSS pixels DevTools input expects by dividing
by `density`; the guest never does that conversion, and the AX bounds it gets
back are already in the same space (measured: passing display pixels straight to
DevTools taps `density` times too far right and down).

### 4.1 Session

| op | params | result |
|---|---|---|
| `hello` | `{proto:1, client:"acos-webview/<ver>", session?:string}` | `{proto:1, session:"<opaque>", engine:{product, version, revision, protocolVersion, userAgent}, limits:{maxViews, maxFrameBytes}}` |
| `ping` | `{}` | `{pong:true, engineMs:number}` |
| `engineStatus` | `{}` | `{state:"up"\|"restarting"\|"down", restarts:int, engine:{…}, idleShutdownInMs:int\|null}` |
| `memory` | `{detail?:bool}` | `{engine:{pssMiB,rssMiB,processes:n}, contexts:n, targets:n, blobBytes:n}` |

### 4.2 Views

| op | params | result |
|---|---|---|
| `view.create` | `{view, width, height, density, url?, userAgent?, javascriptEnabled?, transparent?}` | `{view, width, height, density}` |
| `view.destroy` | `{view}` | `{view, destroyed:true}` |
| `view.setViewport` | `{view, width, height, density}` | `{view, width, height, density}` |
| `view.loadUrl` | `{view, url, headers?:{k:v}}` | `{view, url, loadMs}` (returns after `load`; navigation events also fire) |
| `view.loadData` | `{view, data, mime:"text/html", encoding:"utf8"\|"base64", baseUrl?}` | `{view, url:"data:…", loadMs}` |
| `view.reload` / `view.stop` | `{view}` | `{view}` |
| `view.setUserAgent` | `{view, userAgent}` | `{view, userAgent}` |
| `view.setVisibility` | `{view, visible, windowVisible}` | `{view, visible, windowVisible}` (v1.1) |
| `view.settings` | `{view, settings:{key:value,…}}` | `{view, applied:[…], rejected:[…]}` (v1.1). WV8 adds the two viewport switches: `useWideViewPort` (default `false` = the layout viewport is the View's width in CSS px and `<meta viewport>` is ignored; `true` = honour the page's `<meta viewport>`, and without one use Chromium's Android default of 980 CSS px) and `loadWithOverviewMode` (default `false` = 100 % scale; `true` = `Emulation.setPageScaleFactor(cssWidth / layoutWidth)`, a visual scale that does not change the layout width). Both re-apply the engine's device metrics, so a guest that set them before `view.create` replays them right after it. |
| `view.scrollTo` | `{view, x, y}` (display px) | `{view, x, y, contentWidth, contentHeight, scrollX, scrollY}` (v1.1) |
| `view.evaluateJavascript` | `{view, script, timeoutMs?, userGesture?}` | `{view, valueJson:string\|null, type:"string"\|"number"\|…\|"undefined"}` — `valueJson` is the JSON text of the result (Android's `ValueCallback<String>` contract) |
| `view.addJavascriptInterface` | `{view, name, methods:[string]}` | `{view, name, methods}` |
| `view.removeJavascriptInterface` | `{view, name}` | `{view, name}` |
| `view.history` | `{view, action:"back"\|"forward"\|"clear"}` | `{view, canGoBack, canGoForward}` |
| `render` | `{view, format:"png"\|"rgba", timeoutMs?, ack?:bool}` | JSON `{view, seq, width, height, density, format, bytes, damage:{x,y,w,h}, engineMs, hasDamage, contentWidth, contentHeight, scrollX, scrollY}` **then** the BLOB (§1); the four scroll/content fields are v1.1. **v2 pixels:** `width`/`height` are the frame's real pixel size as the engine produced it, and the BLOB is exactly that. Measured (WV7): this pinned `chrome-headless-shell` cannot emit a device-resolution frame under `--enable-begin-frame-control` — `HeadlessExperimental.beginFrame`'s own `screenshot` captures at the CSS size and ignores `deviceScaleFactor` (a 900x560 device-pixel View came back 343x213 = /density), `Page.captureScreenshot` never answers (every variant times out), and `Page.screencastFrame` reports/carries the CSS size too. The broker therefore takes frames from the view's screencast (device-*surface* pixels, one `beginFrame` per `render`) and reports their real size; the guest scales them into the View (letterboxing, `FILTER_BITMAP`). A device-resolution PNG needs an engine that honours `deviceScaleFactor` in a capture path, which this one does not. |
| `ax` | `{view, maxNodes?, includeIgnored?}` | `{view, count, truncated, nodes:[…]}` (§4.4) |
| `input.touch` | `{view, action:"down"\|"move"\|"up"\|"cancel", points:[{id,x,y,radiusX?,radiusY?,force?}], holdMs?}` | `{view, delivered:true}` |
| `input.tap` | `{view, x, y, holdMs?}` | `{view, delivered:true}` (convenience: down→frame→up) |
| `input.scroll` | `{view, x, y, dx, dy}` | `{view, delivered:true}` |
| `input.key` | `{view, action:"down"\|"up"\|"press", key, code?, keyCode?, modifiers?:[alt,ctrl,meta,shift]}` | `{view, delivered:true}` |
| `input.text` | `{view, text}` | `{view, delivered:true}` (IME commit → `Input.insertText`) |
| `input.setFocus` | `{view, focused:bool}` | `{view, focused}` |

`view.loadUrl` and friends are **async at the protocol level** even though the
response reports `loadMs`: the response is written after `Page.loadEventFired`
*or* after `Page.loadEventFired` already happened for that navigation; the guest
provider does not have to block its UI thread on it. Every navigation also
produces events (§5.1) in order.

### 4.3 Request interception (`file:///android_asset`, `content://`, `http(s)`)

The broker pauses matching requests with `Fetch` and asks the guest, so the
asset/content bytes never leave the guest except through this channel. Patterns
are `file:///android_asset/*`, `content://*`, `file://*` from `view.loadUrl`,
and (when the guest asks for it) `*` for `shouldInterceptRequest` fidelity:

| direction | message |
|---|---|
| host → guest request | `{"id":"h7","op":"req.intercept","view":1,"requestId":"…","url":"file:///android_asset/index.html","method":"GET","headers":{…},"resourceType":"Document","isAsset":true}` |
| guest → host response | `{"id":"h7","ok":true,"result":{"action":"fulfill","status":200,"mime":"text/html","headers":{"access-control-allow-origin":"*"},"bodyB64":"…"}}` |
| | `{"action":"continue"}` — let Chromium fetch it (http/https) |
| | `{"action":"fail","reason":"net::ERR_FILE_NOT_FOUND"}` |

`bodyB64` cap: 8 MiB; larger → the guest answers `action:"fail"` with reason
`ERR_TOO_LARGE` (assets and content:// are small; a video from content:// is not
supported in v1 and says so). The host answers the pause within 30 s; on
timeout it fails the request with `net::ERR_TIMED_OUT` so a hung provider is
visible instead of a hang.

Matching never uses `shouldInterceptRequest` for `data:` URLs (Chromium does not
network-fetch them).

### 4.4 Accessibility nodes (`ax` result)

One node per non-`ignored` AX node, plus ignored nodes only when
`includeIgnored:true`. Bounds are **display coordinates** (device px, view
origin at 0,0, already scaled by `density`) — the same space as `input.tap`, so
a guest UI-tree merge does not need a transform:

```json
{"nodeId": 41, "parentId": 12, "role": "button", "name": "Tap me",
 "value": null, "description": null,
 "bounds": {"x": 96.0, "y": 512.0, "w": 288.0, "h": 112.0},
 "clickable": true, "editable": false, "focused": false,
 "checked": null, "disabled": false, "backendDOMNodeId": 27}
```

`role` is the Chromium AX role verbatim (`button`, `link`, `StaticText`,
`textField`, `image`, `heading`, `list`, `listitem`, `RootWebArea`, …).
`InlineTextBox`/`generic` nodes with no name and no interesting role are
dropped unless `includeIgnored`. `count` is the number of nodes returned,
`truncated` is true when `maxNodes` cut the list. Order is stable (document
order from the AX tree).

## 5. Host → guest

### 5.1 Events

| event | payload |
|---|---|
| `navigation.started` | `{view, url, isNewNavigation, isRedirect}` |
| `navigation.committed` | `{view, url}` |
| `navigation.finished` | `{view, url, title, canGoBack, canGoForward}` |
| `navigation.failed` | `{view, url, code:"net::ERR_…", message}` |
| `render.invalidated` | `{view, reason:"damage"\|"load"\|"scroll"}` — the provider should schedule a `render`; coalesced, never queued per pixel |
| `js.call` is a **request**, not an event (below) |
| `view.titleChanged` | folded into `navigation.finished` |
| `view.focusChanged` | `{view, focused, editable, type, tag}` — page focus/IME report (v1.1) |
| `engine.restarted` | `{restarts, reason}` — every view is gone; the socket stays open |
| `session.closing` | `{reason}` — broker is going away (idle shutdown, host exit); the guest must close and reconnect on the next session |

### 5.2 Host → guest requests

| op | params | guest result |
|---|---|---|
| `nav.shouldOverride` | `{view, url, isRedirect, isMainFrame, gesture}` | `{allow:bool, replace?:bool}` — `allow:false` cancels the navigation; the broker then emits `navigation.failed` with `ERR_ABORTED` only if the guest asked (`reportCancel:true`) |
| `js.call` | `{view, name, method, argsJson, callId}` | `{resultJson:string\|null, error?:string}` — the broker resolves the page-side promise with `resultJson` (or rejects it with `error`) |
| `req.intercept` | see §4.3 | see §4.3 |

Answer cap: 30 s per host request. A guest that never answers gets a
`TIMEOUT` on the page side (the JS promise rejects with
`acos: timeout`), and the broker records the count in `engineStatus` as
`unanswered`.

### 5.3 JS interface, both ways

`view.addJavascriptInterface {name, methods}` makes the broker

1. call `Runtime.addBinding({name:"__acos_jsi"})` once per view, and
2. install a page shim (an isolated-world-free `Runtime.evaluate` of a script
   that defines `window.<name>.<method>(...args)` → `__acos_jsi(JSON)` returning
   a `Promise`) and the pending-call table.

A page call sends `Runtime.bindingCalled` with payload
`{"name":"<iface>","method":"<m>","args":[<json args>],"callId":"<n>"}`; the
broker forwards it as a `js.call` request; the guest's `resultJson` resolves the
Promise. Guest→page is `view.evaluateJavascript`. There is no other path: the
shim only exposes the `methods` list, so an unlisted method is a `TypeError` in
the page (Android's `@JavascriptInterface` rule, enforced host-side).

## 6. Lifecycle and failure semantics

* `hello` must be the first frame; anything else → `BAD_REQUEST` and the broker
  closes.
* Idle shutdown: with no views open for `idleShutdown` (default 10 min, config),
  the broker sends `session.closing{reason:"idle"}`, closes sockets, and stops
  Chromium. The next `hello` restarts it transparently (~250 ms).
* Crash: Chromium's pid and its `--remote-debugging-pipe` EOF are watched. On
  loss the broker restarts the engine, re-`hello`s nothing (the guest keeps its
  socket), emits `engine.restarted`, and every subsequent view op on an old
  view id answers `NO_VIEW`.
* Every response is exactly one JSON frame; the only message that emits two
  frames is `render` (JSON + BLOB).

## 7. Versioning

`proto:2` (an integer). The broker rejects any other `proto` with
`PROTO_VERSION`; the message names `2`. Additive v1.1 ops/fields are listed at
the top of this file. Additive ops/fields do not bump the version; a changed
frame layout, id scheme, or an op whose semantics change does.
