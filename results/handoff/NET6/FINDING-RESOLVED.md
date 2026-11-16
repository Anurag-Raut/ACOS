# NET5 finding, resolved — why the second fetch hung, and what was wrong under it

**Status: fixed and proven on real ART.** `scenarios/net-network-offline.yaml`
passes 14/14 in one guest process (online 200 → `offline` → `onLost` + a bounded
exception in 1.0 s → `online` → `onAvailable` → 200).

## What NET5 measured

After `service_ctl {module: network, op: offline}` a second `HttpURLConnection`
fetch in the same process never returned: 60 s, no exception, no dnsproxyd
request. `DROP`, `REJECT --reject-with tcp-reset` and `link down` all hung
identically, so the rule shape was not the cause.

## What NET6 measured (one ART run, then a wire-level decode)

1. **The stuck frame.** A watchdog in the fixture dumped every Java stack after
   8 s of a pending fetch (the same frames ART's SIGQUIT dump prints):

   ```
   "net-check-fetch" RUNNABLE
     at java.net.SocketInputStream.socketRead0(Native Method)
     at com.android.org.conscrypt.ConscryptEngineSocket$SSLInputStream.readFromSocket
     at com.android.okhttp.okio.AsyncTimeout$2.read
     at com.android.okhttp.okio.RealBufferedSource.readUtf8LineStrict
     at com.android.okhttp.internal.http.Http1xStream.readResponseHeaders
     at com.android.okhttp.internal.http.HttpEngine.readNetworkResponse
     at com.android.okhttp.internal.huc.HttpURLConnectionImpl.getResponseCode
   ```

   No TCP `connect` was in flight (`/proc` showed one `ppoll` on one descriptor,
   15 s timeout, re-entered; `ss` showed no TCP socket in the relay netns). The
   second fetch **reused the first fetch's keep-alive TLS socket** and blocked in
   `read(2)`: the relay's `offline` dropped packets, so nothing ever answered and
   nothing ever failed. The resolver was not even involved — Java's
   `InetAddress` cache answered the second lookup in 1 ms (hence "no dnsproxyd
   request" in NET5).

2. **Why the app could not observe the loss.** Three separate defects sat under
   it, all on the *host → app* path:

   | defect | measured evidence |
   |---|---|
   | a bundle value was written as `[VAL_PARCELABLE][class][payload]`, but Android writes a **length prefix** around class+payload (`Parcel.writeValue` → `isLengthPrefixed`) | the app's own decode of the delivery answered `BadParcelableException: Unmarshalling unknown type code 1124100864 at offset 91`; every `NetworkCallback` message was rejected, so `onAvailable`/`onLost` could never fire |
   | `Message.writeToParcel` has **one** uid word in the image (`sendingUid`); the r75 source cache shows two | with two, `IMessenger.Stub.onTransact` answered `Parcel data not fully consumed, unread size: 4` for every delivery (recorded in `art-frames/unsupported.jsonl` as `REVERSE_DELIVERY_EXCEPTION`) |
   | `CALLBACK_LOST` is **4**, not 3 (`3` is `CALLBACK_LOSING`) | with 3 the app's `onLosing` ran and `onLost` never did — `ConnectivityManager.java:4248-4250` |

   (On API 34 `Parcel.writeString` *is* `writeString16`, so bundle keys and
   parcelable class names are String16 — an earlier attempt to "fix" them to
   String8 is what surfaced the real, length-prefix defect.)

## The fix (cause by cause, Android's behaviour)

| # | Android on a lost network | ACOS now |
|---|---|---|
| 1 | the default route disappears; new `connect(2)` fails at once with `ENETUNREACH` | relay `offline` removes the session's default route **inside its own netns** (`ip route del default`); measured: `connect` fails in 0 ms with errno 101, while the route present hangs for the whole TCP budget. `online` restores it (`ip route replace`) |
| 2 | netd destroys the lost network's sockets (`SOCK_DESTROY`), so a blocked read returns `ECONNRESET`/`ECONNABORTED` | relay `offline` runs `ss -K state established/syn-sent/close-wait` in the session netns; measured: a blocked reader got `ECONNRESET` immediately |
| 3 | the resolver fails fast (`EAI_NODATA` → `UnknownHostException`) | the `dnsproxyd` responder answers `500` while the session is offline (`acos_services::network::offline()`), so a fresh lookup is bounded too |
| 4 | `ConnectivityManager` loses its default network and callbacks get `onLost` | `ConnectivityState::set_default_network(false)` — `getActiveNetwork()` is `null`, every list is empty, and each live request gets `CALLBACK_LOST` (one delivery per request); `online` sends the four registration messages again (`onAvailable`, capabilities, link properties, blocked status) |

With 1+2 the fetch that used to hang now fails in ~1 s with
`java.net.ConnectException: Failed to connect to example.com/…:443` (OkHttp
transparently retries the destroyed keep-alive connection, and the retry's
`connect(2)` hits the removed route) — an Android-shaped, bounded failure.

## What is *not* changed

`plan::offline_rule` (the relay's packet-level `DROP`) stays: it is the
fail-closed half, and it also covers packets already in flight. `offline` is now
two halves, not a replacement.

## Residual observation (not part of this fix)

A `runOnUiThread` update posted from a binder thread does not wake ACOS's idle
main looper promptly: the callback's own `Log.i` line appears immediately while
the `TextView` text lags until the next input (the scenario asserts the callback
on the app's log for that reason). Worth its own item; it does not affect whether
an app *receives* the callback.

## Evidence files

| path | what |
|---|---|
| `run/proof/proof.log` | the four app-level arms: `off 8/8`, `real 9/9`, `offline 14/14`, host-port PASS |
| `run/proof/offline/art/verdict.json` | the 14 steps of the offline/online proof, including the callback and exception assertions |
| `run/proof/offline/art/logs/logcat.txt` | `netcb onAvailable` → `netcb onLost` → `fetch failed: java.net.ConnectException` → `netcb onAvailable` → `fetch result=HTTP 200` |
| `run/stacks6-threads.txt` | the live threaded probe: the stuck fetch thread in `ppoll` with a 15 s timeout, no TCP socket, no resolver request |
| `run/offline1-logcat.txt` | the pre-fix run: status stays `pending` for the 60 s budget |
| `run/exp-order.yaml` | the experiment that separated "the callback fired" from "the TextView refreshed" |
| `run/acceptance-tests.log` | `cargo test -p acos-netd -p acos-services`, all targets green |
