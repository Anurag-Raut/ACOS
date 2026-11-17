package com.android.webview.chromium;

// WV2: the guest side of the ACOS WebView wire protocol
// (results/handoff/WVB/WIRE.md §1-§5, normative; the broker's
// `crates/acos-webview/src/wire.rs` is the executable copy).
//
// One Unix stream socket per session, framed messages:
//
//   frame  := uint32_be length, body[length]
//   body[0] = 1: UTF-8 JSON object (request / response / event)
//   body[0] = 2: BLOB  (render frame): uint32_be nid, id, uint8 format,
//                       uint32_be width, height, stride, uint64_be seq, data
//
// This class is the *stand-in* the broker client satisfies: it owns no socket
// creation and no engine knowledge. `Socket` is constructed over the stream
// pair of the session socket the ACOS sandbox bind-mounts at
// `/run/acos/webview.sock`, so wiring the broker in is
// `new AcosWebViewTransport.Socket(input, output)` — no provider code changes.
//
// Nothing here exposes a DevTools port, a target id or a Chromium pid: the
// guest only ever sees `view` integers and JSON results.

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

import org.json.JSONObject;

/** Client side of the WVB/WV2 wire protocol (normative: WVB/WIRE.md). */
public final class AcosWebViewTransport {
    /** WIRE.md §1: the largest legal frame (64 MiB). */
    public static final int MAX_FRAME = 64 * 1024 * 1024;
    /** WIRE.md §1: `kind` for a JSON message. */
    public static final int KIND_JSON = 1;
    /** WIRE.md §1: `kind` for a render BLOB. */
    public static final int KIND_BLOB = 2;

    private AcosWebViewTransport() {}

    /** One decoded frame: the JSON message and, for `render`, its BLOB. */
    public static final class Frame {
        public final JSONObject json;
        public final byte[] blob;

        Frame(JSONObject json, byte[] blob) {
            this.json = json;
            this.blob = blob;
        }

        /** `result` of an ok response (never null: an empty object for events). */
        public JSONObject result() {
            return json.optJSONObject("result");
        }
    }

    /** A wire error (`WIRE.md` §3): a stable `code`, a message and optional detail. */
    public static final class TransportException extends IOException {
        public final String code;

        public TransportException(String code, String message) {
            super(code + ": " + message);
            this.code = code;
        }
    }

    /** Host -> guest events (`WIRE.md` §5.1) and host -> guest requests (§5.2). */
    public interface EventHandler {
        /** An event (`{"event": ...}`). */
        void onEvent(JSONObject event);

        /**
         * A host request (`{"id":"h<n>","op":...}`); the answer is the JSON
         * `result` object, or a `TransportException` to answer with an error.
         * Runs on the transport's reader thread: never block the UI thread on it.
         */
        JSONObject onRequest(String op, JSONObject params) throws TransportException;
    }

    /** The transport the provider is written against. */
    public interface Transport {
        Frame call(String op, JSONObject params) throws IOException;

        /** `render` (`WIRE.md` §4.2): the JSON response and then its BLOB. */
        Frame render(int view) throws IOException;

        void setEventHandler(EventHandler handler);

        String sessionName();

        void close();
    }

    /**
     * The framed client over a connected stream pair.
     *
     * The broker writes a response and, for `render`, its BLOB back-to-back
     * under one lock (`WIRE.md` §1), so reading the JSON response and then its
     * BLOB is the only legal order; this class does that on the reader thread
     * and hands every frame to the waiting caller.
     */
    public static final class Socket implements Transport {
        private final DataInputStream in;
        private final DataOutputStream out;
        private final Object writeLock = new Object();
        private final Map<String, Pending> pending = new HashMap<>();
        private final AtomicInteger nextId = new AtomicInteger();
        private final CountDownLatch ready = new CountDownLatch(1);
        /** The transport's own error, once the socket is gone (`WIRE.md` §6). */
        private volatile EventHandler handler;
        private volatile TransportException failure;
        private volatile String session = "";
        private volatile JSONObject helloResult;

        public Socket(InputStream input, OutputStream output) {
            this.in = new DataInputStream(input);
            this.out = new DataOutputStream(output);
            Thread reader = new Thread(this::readLoop, "acos-webview-wire");
            reader.setDaemon(true);
            reader.start();
        }

        /** `hello` must be the first frame (`WIRE.md` §6). */
        public JSONObject hello(String client) throws IOException {
            JSONObject params = new JSONObject();
            try {
                params.put("proto", 2);
                params.put("client", client);
            } catch (org.json.JSONException e) {
                throw new TransportException("BAD_REQUEST", e.toString());
            }
            JSONObject result = call("hello", params).result();
            session = result == null ? "" : result.optString("session", "");
            helloResult = result;
            return result == null ? new JSONObject() : result;
        }

        /** One in-flight request: its op and the frame the caller waits for. */
        private static final class Pending {
            final String op;
            final BlockingQueue<Frame> frames = new LinkedBlockingQueue<>(1);
            private JSONObject held;

            Pending(String op) {
                this.op = op;
            }

            /** Hold a `render` response until its BLOB arrives (`WIRE.md` §1). */
            synchronized void hold(JSONObject json) {
                held = json;
            }

            synchronized JSONObject release() {
                JSONObject json = held;
                held = null;
                return json;
            }
        }

        @Override
        public String sessionName() {
            return session;
        }

        public JSONObject engineInfo() {
            JSONObject result = helloResult;
            return result == null ? null : result.optJSONObject("engine");
        }

        @Override
        public void setEventHandler(EventHandler handler) {
            this.handler = handler;
        }

        @Override
        public Frame call(String op, JSONObject params) throws IOException {
            TransportException broken = failure;
            if (broken != null) {
                throw broken;
            }
            String id = "g" + nextId.incrementAndGet();
            Pending slot = new Pending(op);
            synchronized (pending) {
                pending.put(id, slot);
            }
            try {
                JSONObject request = new JSONObject();
                request.put("id", id);
                request.put("op", op);
                if (params != null) {
                    // `keys()` (not `keySet()`) keeps this source compilable
                    // against both the Android and the desktop `org.json`.
                    java.util.Iterator<String> keys = params.keys();
                    while (keys.hasNext()) {
                        String key = keys.next();
                        request.put(key, params.get(key));
                    }
                }
                writeFrame(KIND_JSON, request.toString().getBytes(StandardCharsets.UTF_8));
                Frame frame = slot.frames.poll(30, TimeUnit.SECONDS);
                // A connection-level error frame (id "g?", WIRE.md §2/§6) is
                // fatal for every pending request, never a per-request answer:
                // readLoop records it here and wakes every waiter.
                TransportException nowBroken = failure;
                if (nowBroken != null) {
                    throw nowBroken;
                }
                if (frame == null) {
                    throw new TransportException("TIMEOUT", op + " answered nothing in 30 s");
                }
                JSONObject json = frame.json;
                if (json.has("event")) {
                    throw new TransportException(
                            "BAD_FRAME", op + " was answered with event " + json.optString("event"));
                }
                if (!json.optBoolean("ok", false)) {
                    JSONObject error = json.optJSONObject("error");
                    throw new TransportException(
                            error == null ? "INTERNAL" : error.optString("code", "INTERNAL"),
                            error == null ? "no error object" : error.optString("message", ""));
                }
                return frame;
            } catch (org.json.JSONException e) {
                throw new TransportException("BAD_REQUEST", e.toString());
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new TransportException("INTERNAL", "interrupted waiting for " + op);
            } finally {
                synchronized (pending) {
                    pending.remove(id);
                }
            }
        }

        /** `render`: the JSON response followed by its BLOB (§1, §4.2). */
        public Frame render(int view) throws IOException {
            JSONObject params = new JSONObject();
            try {
                params.put("view", view);
                params.put("format", "png");
            } catch (org.json.JSONException e) {
                throw new TransportException("BAD_REQUEST", e.toString());
            }
            return call("render", params);
        }

        @Override
        public void close() {
            try {
                in.close();
            } catch (IOException ignored) {
            }
            try {
                out.close();
            } catch (IOException ignored) {
            }
        }

        private void writeFrame(int kind, byte[] body) throws IOException {
            int length = body.length + 1;
            synchronized (writeLock) {
                out.writeInt(length);
                out.writeByte(kind);
                out.write(body);
                out.flush();
            }
        }

        private void readLoop() {
            try {
                while (true) {
                    int length = in.readInt();
                    if (length <= 0 || length > MAX_FRAME) {
                        throw new TransportException("BAD_FRAME", "frame length " + length);
                    }
                    int kind = in.readUnsignedByte();
                    byte[] body = new byte[length - 1];
                    in.readFully(body);
                    if (kind == KIND_JSON) {
                        dispatch(new Frame(new JSONObject(new String(body, StandardCharsets.UTF_8)), null));
                    } else if (kind == KIND_BLOB) {
                        // A BLOB follows its `render` JSON response; the JSON
                        // frame waiting on the queue is completed with it.
                        byte[] blob = decodeBlob(body);
                        dispatchBlob(blob);
                    } else {
                        throw new TransportException("BAD_FRAME", "kind " + kind);
                    }
                }
            } catch (EOFException e) {
                failure = new TransportException("ENGINE_CRASHED", "socket closed by the broker");
            } catch (Exception e) {
                failure = e instanceof TransportException
                        ? (TransportException) e
                        : new TransportException("BAD_FRAME", e.toString());
            } finally {
                ready.countDown();
                if (failure == null) {
                    failure = new TransportException("ENGINE_CRASHED", "socket closed by the broker");
                }
                failAll(failure);
            }
        }

        /** Fail every in-flight request with one fatal transport error. */
        private void failAll(TransportException error) {
            failure = error;
            synchronized (pending) {
                for (Pending slot : pending.values()) {
                    slot.frames.clear();
                    slot.frames.offer(new Frame(new JSONObject(), null));
                }
            }
        }

        /** Answer one host -> guest request (`WIRE.md` §5.2). */
        private void answerHostRequest(String id, String op, JSONObject params) {
            android.util.Log.i("ACOSWvProvider", "host request " + op + " id=" + id);
            JSONObject response = new JSONObject();
            try {
                response.put("id", id);
                EventHandler h = handler;
                if (h == null) {
                    throw new TransportException(
                            "UNSUPPORTED", "no request handler for host op " + op);
                }
                JSONObject result = h.onRequest(op, params);
                response.put("ok", true);
                response.put("result", result == null ? new JSONObject() : result);
            } catch (TransportException e) {
                try {
                    JSONObject error = new JSONObject();
                    error.put("code", e.code);
                    error.put("message", e.getMessage());
                    response.put("ok", false);
                    response.put("error", error);
                } catch (org.json.JSONException ignored) {
                }
            } catch (Exception e) {
                try {
                    JSONObject error = new JSONObject();
                    error.put("code", "INTERNAL");
                    error.put("message", e.toString());
                    response.put("ok", false);
                    response.put("error", error);
                } catch (org.json.JSONException ignored) {
                }
            }
            try {
                writeFrame(KIND_JSON, response.toString().getBytes(StandardCharsets.UTF_8));
            } catch (IOException e) {
                failure = new TransportException("ENGINE_CRASHED", "answer write: " + e);
            }
        }

        /** `WIRE.md` §1 BLOB: nid, id, format, w, h, stride, seq, data. */
        private byte[] decodeBlob(byte[] body) throws TransportException {
            int p = 0;
            int nid = readInt(body, p);
            p += 4;
            if (nid < 0 || p + nid > body.length) {
                throw new TransportException("BAD_FRAME", "BLOB id length " + nid);
            }
            p += nid;
            if (p + 1 + 16 > body.length) {
                throw new TransportException("BAD_FRAME", "BLOB header truncated");
            }
            p += 1; // format
            p += 12; // width, height, stride
            p += 8; // seq
            byte[] data = new byte[body.length - p];
            System.arraycopy(body, p, data, 0, data.length);
            return data;
        }

        private static int readInt(byte[] b, int off) {
            return ((b[off] & 0xff) << 24)
                    | ((b[off + 1] & 0xff) << 16)
                    | ((b[off + 2] & 0xff) << 8)
                    | (b[off + 3] & 0xff);
        }

        private void dispatch(Frame frame) {
            JSONObject json = frame.json;
            if (json.has("event")) {
                EventHandler h = handler;
                if (h != null) {
                    h.onEvent(json);
                }
                return;
            }
            String id = json.optString("id", "");
            // A host -> guest request (`WIRE.md` §5.2: req.intercept,
            // nav.shouldOverride, js.call) has an `op`. It runs on this reader
            // thread and its answer goes back as a normal response frame.
            if (json.has("op")) {
                answerHostRequest(id, json.optString("op", ""), json);
                return;
            }
            Pending slot;
            synchronized (pending) {
                slot = pending.get(id);
            }
            if (slot == null) {
                // No request owns this id: the gate's connection-level error
                // ("g?" on a failed attach, WIRE.md §2/§6) is fatal for the
                // whole connection, so every pending request fails with the
                // same structured error instead of hanging until its timeout.
                if ("g?".equals(id) && json.has("ok") && !json.optBoolean("ok", false)) {
                    JSONObject error = json.optJSONObject("error");
                    String codeName = error == null
                            ? "ENGINE_UNAVAILABLE"
                            : error.optString("code", "ENGINE_UNAVAILABLE");
                    String message = error == null
                            ? "the WebView broker closed the connection"
                            : error.optString("message", "");
                    failAll(new TransportException(codeName, message));
                }
                return;
            }
            // `render` is the one op with two frames (`WIRE.md` §1, §6): the
            // JSON response is followed by its BLOB on the same stream, so the
            // response is only complete once the BLOB has arrived. Offering the
            // JSON alone would hand the caller a frame-less render.
            if ("render".equals(slot.op) && json.optBoolean("ok", false)) {
                slot.hold(json);
                return;
            }
            slot.frames.offer(frame);
        }

        private void dispatchBlob(byte[] blob) {
            // The BLOB of a `render` completes the response held for it.
            Pending slot;
            synchronized (pending) {
                slot = null;
                for (Pending candidate : pending.values()) {
                    if ("render".equals(candidate.op)) {
                        slot = candidate;
                        break;
                    }
                }
            }
            if (slot == null) {
                return;
            }
            JSONObject held = slot.release();
            if (held == null) {
                return;
            }
            slot.frames.offer(new Frame(held, blob));
        }
    }
}
