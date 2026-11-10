package dev.acos.fixtures.flutterhello;

import io.flutter.embedding.android.FlutterActivity;

/**
 * Launcher activity for the ACOS Flutter fixture.
 *
 * <p>FlutterActivity's default Dart entrypoint is {@code main}, which is exactly
 * what the fixture's AOT snapshot exports.
 *
 * <p>Software-rendering fallback: if the device/engine combination cannot create
 * a GPU surface, override {@link #getFlutterShellArgs()} and return
 * {@code new FlutterShellArgs(new String[] {"--enable-software-rendering"})}.
 * The Flutter 3.47.9 engine supports that flag; it is documented in
 * results/flutter/BUILD.md.
 */
public class MainActivity extends FlutterActivity {
}
