package com.android.webview.chromium;

// WV2: the session-socket stand-in.
//
// WIRE.md §0: ACOS creates one Unix stream socket per session and bind-mounts
// it into the guest at `/run/acos/webview.sock`; the guest connects once, sends
// `hello`, and keeps the connection for the session's life. The guest never
// sees a DevTools port, a target id or a Chromium pid.
//
// This class only *connects*: it is the one piece the broker client replaces
// (with the same `AcosWebViewTransport.Transport` result). A missing socket is
// a structured failure, never a silent WebView that paints nothing: the caller
// (`WebViewChromiumFactoryProvider.transport`) turns it into the honest
// `ENGINE_UNAVAILABLE` error, exactly like `WebViewFactory` does on a phone
// without a provider.

import android.net.LocalSocket;
import android.net.LocalSocketAddress;
import android.util.Log;

import java.io.IOException;

/** Opens the guest end of the session socket and builds the framed client. */
final class AcosSessionSocket {
    private AcosSessionSocket() {}

    static AcosWebViewTransport.Transport open(String path) {
        try {
            LocalSocket socket = new LocalSocket();
            socket.connect(new LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM));
            AcosWebViewTransport.Socket transport =
                    new AcosWebViewTransport.Socket(socket.getInputStream(), socket.getOutputStream());
            transport.hello("acos-webview/0.4.0");
            // Close the *socket* when the framed client closes.
            return new Closing(transport, socket);
        } catch (IOException e) {
            Log.e(WebViewChromiumFactoryProvider.TAG,
                    "ACOS WebView engine unavailable: " + path + ": " + e);
            throw new IllegalStateException(
                    "ACOS WebView engine unavailable (ENGINE_UNAVAILABLE): "
                            + path + ": " + e.getMessage(), e);
        }
    }

    private static final class Closing implements AcosWebViewTransport.Transport {
        private final AcosWebViewTransport.Socket mSocket;
        private final LocalSocket mLocal;

        Closing(AcosWebViewTransport.Socket socket, LocalSocket local) {
            mSocket = socket;
            mLocal = local;
        }

        @Override
        public AcosWebViewTransport.Frame call(String op, org.json.JSONObject params) throws IOException {
            return mSocket.call(op, params);
        }

        @Override
        public AcosWebViewTransport.Frame render(int view) throws IOException {
            return mSocket.render(view);
        }

        @Override
        public void setEventHandler(AcosWebViewTransport.EventHandler handler) {
            mSocket.setEventHandler(handler);
        }

        @Override
        public String sessionName() {
            return mSocket.sessionName();
        }

        @Override
        public void close() {
            mSocket.close();
            try {
                mLocal.close();
            } catch (IOException ignored) {
            }
        }
    }
}
