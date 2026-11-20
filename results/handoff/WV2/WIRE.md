# WV2 — guest side of the WebView wire protocol

**Normative document: `results/handoff/WVB/WIRE.md` on branch `wvb`** (the host
broker's `crates/acos-webview/src/wire.rs` is its executable copy). WV2
**conforms to it** and adds nothing to the wire: no new frame kind, no new op,
no field of its own. This file only pins what the *guest provider* does with it,
so the two halves cannot drift.

## 1. Transport and framing (unchanged, WVB/WIRE.md §0–§1)

* One `SOCK_STREAM` Unix socket per session, bind-mounted by ACOS at
  `/run/acos/webview.sock` in the guest. The guest connects once, sends
  `hello` first, and keeps the connection for the session.
* `frame := uint32_be length, body[length]`, `length` 1..64 MiB;
  `body[0] == 1` JSON object, `body[0] == 2` render BLOB
  (`uint32_be nid, id, uint8 format, uint32_be w,h,stride, uint64_be seq, data`).
* The guest never sees a DevTools port, a target id or a Chromium pid.

Guest implementation (`fixtures/apps/webview-provider/src/com/android/webview/chromium/`):

| class | role |
|---|---|
| `AcosWebViewTransport` | the protocol: frame read/write, `g<n>` request ids, out-of-order responses, JSON+BLOB pairing for `render`, event dispatch, host-request answers, stable error codes (`BAD_FRAME`, `TIMEOUT`, `UNSUPPORTED`, …). `hello` is its first frame. |
| `AcosWebViewTransport.Transport` | **the stand-in the broker client satisfies**: `call(op, params)`, `render(view)`, `setEventHandler`, `sessionName`, `close`. Nothing else in the provider knows about sockets. |
| `AcosSessionSocket` | the only class that touches a socket: connects `LocalSocket(FILESYSTEM, "/run/acos/webview.sock")`, wraps it in `AcosWebViewTransport.Socket`, sends `hello`. A missing socket is `ENGINE_UNAVAILABLE`, never a silent no-op. |
| `WebViewChromiumCore` | the Android behaviour: viewport, navigation, JS, JS interfaces, frames, touch, `WebViewClient` events. |
| `WebViewChromium`, `…ViewDelegate`, `…ScrollDelegate`, `…FactoryProvider` | generated framework surface (`tools/gen-webview-provider.py`); each implemented method is a one-line forward to the core, everything else is a structured `UnsupportedOperationException`. |

## 2. Ops the guest provider implements (WVB/WIRE.md §4, §5)

| wire op / event | guest call site |
|---|---|
| `hello` | `AcosSessionSocket.open` (once per session) |
| `view.create` / `view.setViewport` / `view.destroy` | `WebViewChromiumCore.ensureCreated` / `onSizeChanged` (from `ViewDelegate.onSizeChanged`, device px + density) / `destroy` |
| `view.loadUrl`, `view.loadData` | `loadUrl`, `loadData`, `loadDataWithBaseURL` |
| `view.reload`, `view.stop` | `reload`, `stopLoading` |
| `view.evaluateJavascript` | `evaluateJavaScript` → the `ValueCallback<String>` gets `valueJson` verbatim (Android's contract) |
| `view.addJavascriptInterface` / `removeJavascriptInterface` | `addJavascriptInterface` sends the `@JavascriptInterface`-annotated **method-name list only** (the annotation rule is decided guest-side by reflection; the broker enforces the list) |
| `render` | pulled once per `render.invalidated` event (coalesced) and painted by `ViewDelegate.onDraw` at the View's own size/density (`FILTER_BITMAP`) |
| `input.touch` | `ViewDelegate.onTouchEvent(MotionEvent)` — raw pointer stream (`action`, `points[id,x,y,force]`) |
| `navigation.started` / `navigation.finished` / `navigation.failed` | `WebViewClient.onPageStarted` / `onPageFinished` / `onReceivedError`, posted to the UI thread in event order |
| host request `nav.shouldOverride` | `WebViewClient.shouldOverrideUrlLoading` on the UI thread (25 s budget), answered `{allow: !handled}` |
| host request `js.call` | the annotated bridge method, answered `{resultJson, error?}`; an unlisted method is an error, never a silent `null` |
| `engine.restarted`, `session.closing`, unknown events | ignored by v0.4.0's core; the next `view.*` op answers `NO_VIEW`/`ENGINE_UNAVAILABLE` honestly |

Not yet used by the guest (broker-side only in v1): `ping`, `engineStatus`,
`memory`, `ax`, `input.tap/scroll/key/text/setFocus`, `view.history`,
`view.setUserAgent`, `req.intercept`. They are listed so the broker side can
land first; the guest adds them as their Android surface lands.

## 3. Constants

* socket path `/run/acos/webview.sock` (guest side of what ACOS bind-mounts);
* `proto: 1`, client string `acos-webview/0.4.0`;
* request timeout 30 s, host-request answer budget 25 s guest-side (broker
  allows 30 s) — a timeout is a wire `TIMEOUT`, never a fabricated success.
