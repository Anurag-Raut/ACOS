// Minimal Flutter app that talks to the engine through dart:ui only.
//
// It deliberately does not import package:flutter: ACOS only needs a real
// Flutter engine (libflutter.so) plus a real AOT app snapshot (libapp.so),
// and this keeps the fixture buildable without the 1.5 GB Flutter SDK.
import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;

ui.PlatformDispatcher get _dispatcher => ui.PlatformDispatcher.instance;
ui.FlutterView? get _view => _dispatcher.implicitView;

int _counter = 0;
ui.Rect _buttonRect = ui.Rect.zero;
bool _buttonDown = false;

// W169-A: a real app runs a continuous animation. `dart:ui` 3.47 has no
// `scheduleFrameCallback` (that is `SchedulerBinding` in the framework), so
// the raw probe ticks by scheduling the next frame from the draw callback, the
// same engine animator/vsync path a `Ticker` ultimately uses.
int _animStep = -1;
bool _animPending = false;
const int _animFrames = 6;
int _beginFrames = 0;
int _lastBeginMicros = 0;

// W169-A: platform-channel surface. A real app talks to the embedder through
// `flutter/platform` (SystemChrome), `flutter/textinput`, `flutter/navigation`
// (popRoute) and `flutter/keydata`; these exercise the same engine transport
// from `dart:ui` without the framework.
bool _probeSent = false;

void main() {
  print("ACOS_DART_MAIN entered");
  print(
    'ACOS_DART_MAIN views=${_dispatcher.views.length} '
    'implicit=${_dispatcher.implicitView?.viewId}',
  );
  _dispatcher.onBeginFrame = _onBeginFrame;
  _dispatcher.onDrawFrame = _onDrawFrame;
  _dispatcher.onPointerDataPacket = _onPointerDataPacket;
  _dispatcher.onMetricsChanged = () => _dispatcher.scheduleFrame();
  _dispatcher.onKeyData = _onKeyData;
  _dispatcher.onSemanticsEnabledChanged = _onSemanticsEnabledChanged;
  // W169-A: embedder -> Dart listeners. `channelBuffers.setListener` registers
  // the channel with the engine (`SendChannelUpdate`), so a
  // `FlutterJNI.dispatchPlatformMessage` reaches it. The custom channel is the
  // method-channel round trip; `flutter/navigation` is the back-button channel
  // a real app's WidgetsBinding would answer.
  ui.channelBuffers.setListener('acos/dart_in', _onDartIn);
  ui.channelBuffers.setListener('flutter/navigation', _onNavigation);
  // W156-A: handle the accessibility action channel so `acos tap --text` can
  // drive the button through `FlutterJNI.dispatchSemanticsAction` as well as
  // the pointer path. W169-A: the pinned dart:ui 3.47 API is the
  // `SemanticsActionEvent` callback (`onSemanticsAction` does not exist; the
  // committed fixture never compiled against this platform).
  _dispatcher.onSemanticsActionEvent = _onSemanticsActionEvent;
  _dispatcher.scheduleFrame();
}

/// W149-A/W156-A: publish a semantic node for the counter button so the
/// engine's accessibility stream is non-empty. Raw `dart:ui` apps must build
/// semantics themselves; a widget app gets this from the framework.
///
/// W156-A: the engine's `_updateSemanticsEnabled(true)` callback can run
/// before `main` installs `onSemanticsEnabledChanged` (the isolate starts
/// asynchronously), so the node is also re-published from every frame while
/// `PlatformDispatcher.semanticsEnabled` is true.
void _onSemanticsEnabledChanged() {
  if (!_dispatcher.semanticsEnabled) {
    return;
  }
  _publishSemantics();
}

/// W156-A/W169-A: the engine-invoked accessibility action handler
/// (`nativeDispatchSemanticsAction` -> `_dispatchSemanticsAction`). The raw
/// `dart:ui` probe handles the tap on node 1 itself; a widget app would get
/// this from the framework.
void _onSemanticsActionEvent(ui.SemanticsActionEvent event) {
  print(
    'ACOS_DART_SEMANTICS_ACTION id=${event.nodeId} action=${event.type} '
    'view=${event.viewId}',
  );
  if (event.nodeId == 1 && event.type == ui.SemanticsAction.tap) {
    _counter += 1;
    _semanticsPublished = false;
    print('ACOS_DART_COUNTER counter=$_counter via=semantics');
    _dispatcher.scheduleFrame();
  }
}

bool _semanticsPublished = false;

void _publishSemantics() {
  final ui.SemanticsUpdateBuilder builder = ui.SemanticsUpdateBuilder();
  final ui.Rect rect = _buttonRect;
  final Float64List identity = Float64List.fromList(const <double>[
    1, 0, 0, 0,
    0, 1, 0, 0,
    0, 0, 1, 0,
    0, 0, 0, 1,
  ]);
  builder.updateNode(
    id: 1,
    flags: ui.SemanticsFlags(isButton: true),
    // The engine serialises the Java `AccessibilityBridge.Action` mask, whose
    // TAP is bit 0. (`dart:ui`'s `SemanticsAction.tap.index` is 1 in 3.47, so
    // shifting by it would set the LONG_PRESS bit.)
    actions: 1 << 0,
    maxValueLength: -1,
    currentValueLength: -1,
    textSelectionBase: -1,
    textSelectionExtent: -1,
    platformViewId: -1,
    scrollChildren: 0,
    scrollIndex: 0,
    traversalParent: 0,
    scrollPosition: 0,
    scrollExtentMax: 0,
    scrollExtentMin: 0,
    rect: rect,
    identifier: '',
    label: 'Increment',
    labelAttributes: const <ui.StringAttribute>[],
    value: 'counter: $_counter',
    valueAttributes: const <ui.StringAttribute>[],
    increasedValue: '',
    increasedValueAttributes: const <ui.StringAttribute>[],
    decreasedValue: '',
    decreasedValueAttributes: const <ui.StringAttribute>[],
    hint: '',
    hintAttributes: const <ui.StringAttribute>[],
    tooltip: '',
    textDirection: ui.TextDirection.ltr,
    transform: identity,
    hitTestTransform: identity,
    childrenInTraversalOrder: Int32List(0),
    childrenInHitTestOrder: Int32List(0),
    additionalActions: Int32List(0),
    controlsNodes: null,
    inputType: ui.SemanticsInputType.none,
    locale: null,
    minValue: '',
    maxValue: '',
  );
  _dispatcher.updateSemantics(builder.build());
  _semanticsPublished = true;
  print('ACOS_DART_SEMANTICS nodes=1 label=Increment rect=$rect');
}

void _onBeginFrame(Duration frameTime) {
  // The frame is drawn in _onDrawFrame. W169-A: count animation frames and
  // record the engine's monotonic frame time so multi-frame presentation and
  // its cadence are provable from the log.
  _beginFrames++;
  final int micros = frameTime.inMicroseconds;
  print(
    'ACOS_DART_BEGINFRAME n=$_beginFrames at=$micros '
    'delta=${micros - _lastBeginMicros}',
  );
  _lastBeginMicros = micros;
}

// W169-A: Dart -> embedder -> Dart platform-channel round trip. The payload is
// deliberately raw UTF-8 (not StandardMethodCodec): the point is the engine's
// transport, which is exactly what the framework's `MethodChannel` rides on.
void _sendPlatformProbes() {
  if (_probeSent) {
    return;
  }
  _probeSent = true;
  final Uint8List ping = Uint8List.fromList(utf8.encode('ping-from-dart'));
  _dispatcher.sendPlatformMessage(
    'acos/probe',
    ping.buffer.asByteData(),
    _onProbeReply,
  );
  final Uint8List chrome = Uint8List.fromList(
    utf8.encode('SystemChrome.setApplicationSwitcherDescription'),
  );
  _dispatcher.sendPlatformMessage(
    'flutter/platform',
    chrome.buffer.asByteData(),
    _onProbeReply,
  );
  print('ACOS_DART_CHANNEL_SENT channel=acos/probe,flutter/platform');
}

void _onProbeReply(ByteData? data) {
  final String reply = data == null
      ? '<null>'
      : utf8.decode(data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes));
  print('ACOS_DART_CHANNEL_REPLY data=$reply');
}

// Embedder -> Dart: the Java driver's `sendDartIn`.
void _onDartIn(ByteData? data, ui.PlatformMessageResponseCallback callback) {
  final String s = data == null
      ? '<null>'
      : utf8.decode(data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes));
  print('ACOS_DART_CHANNEL_IN channel=acos/dart_in data=$s');
  final Uint8List ack = Uint8List.fromList(utf8.encode('ack-from-dart'));
  callback(ack.buffer.asByteData());
}

// Embedder -> Dart: the Android back button. The framework's WidgetsBinding
// answers `popRoute`; a raw probe can only prove the engine delivered it.
void _onNavigation(ByteData? data, ui.PlatformMessageResponseCallback callback) {
  final int n = data == null ? -1 : data.lengthInBytes;
  print('ACOS_DART_NAVIGATION bytes=$n');
  callback(Uint8List(2).buffer.asByteData());
}

// Embedder -> Dart: keyboard events arrive on the engine's `flutter/keydata`
// channel and are unpacked by `PlatformDispatcher.onKeyData`.
bool _onKeyData(ui.KeyData data) {
  print(
    'ACOS_DART_KEY type=${data.type} physical=${data.physical} '
    'logical=${data.logical} character=${data.character} '
    'synthesized=${data.synthesized}',
  );
  return true;
}

int _drawAttempts = 0;

void _onDrawFrame() {
  _drawAttempts++;
  if (_drawAttempts <= 3 || _drawAttempts % 10 == 0) {
    // W128-A: the exact reason a draw produces no scene has to be visible in
    // the engine log, not inferred. `views` is the engine-pushed view registry
    // and `physicalSize` comes from the viewport metrics.
    print(
      'ACOS_DART_FRAME draw n=$_drawAttempts counter=$_counter '
      'views=${_dispatcher.views.length} '
      'implicit=${_dispatcher.implicitView?.viewId} '
      'size=${_view?.physicalSize} dpr=${_view?.devicePixelRatio} '
      'top=${_view?.padding.top}',
    );
  } else {
    print("ACOS_DART_FRAME draw counter=$_counter");
  }
  final view = _view;
  if (view == null) {
    return;
  }
  final ui.Size physical = view.physicalSize;
  final double dpr = view.devicePixelRatio;
  final double width = physical.width / dpr;
  final double height = physical.height / dpr;
  if (width <= 0 || height <= 0) {
    return;
  }
  final double top = view.padding.top;

  final recorder = ui.PictureRecorder();
  final canvas = ui.Canvas(recorder);

  // Background.
  canvas.drawRect(
    ui.Rect.fromLTWH(0, 0, width, height),
    ui.Paint()..color = const ui.Color(0xFF1B1B1F),
  );

  // W169-A: the platform probes do not need a settled frame, but sending on
  // the first draw keeps them after `main` has installed every listener.
  _sendPlatformProbes();

  // "hello acos" title.
  final title = _paragraph('hello acos', 32, const ui.Color(0xFFFFFFFF));
  title.layout(ui.ParagraphConstraints(width: width - 80));
  canvas.drawParagraph(title, ui.Offset(40, top + 20));

  // Counter button.
  final buttonRect = ui.Rect.fromLTWH(40, top + 100, 220, 64);
  _buttonRect = buttonRect;
  // W156-A: republish the semantic node once the button geometry is known and
  // semantics are enabled. This is race-free against the engine's
  // `_updateSemanticsEnabled` callback ordering.
  if (_dispatcher.semanticsEnabled && !_semanticsPublished) {
    _publishSemantics();
  }
  canvas.drawRect(buttonRect, ui.Paint()..color = const ui.Color(0xFF6750A4));
  final label = _paragraph(
    'counter: $_counter',
    24,
    const ui.Color(0xFFFFFFFF),
  );
  label.layout(ui.ParagraphConstraints(width: buttonRect.width));
  canvas.drawParagraph(
    label,
    ui.Offset(
      buttonRect.left + (buttonRect.width - label.width) / 2,
      buttonRect.top + (buttonRect.height - label.height) / 2,
    ),
  );

  // W169-A animation: the tap frame itself stays at rest (so the W130-A
  // parity reference still matches); the moving "Hello ACOS" Paragraph is
  // drawn on the following frames. This is both the multi-frame presentation
  // probe and the dart:ui glyph-rendering probe.
  if (_animPending) {
    _animPending = false;
    _animStep = 0;
    _dispatcher.scheduleFrame();
  } else if (_animStep >= 0 && _animStep < _animFrames) {
    final ui.Paragraph hello = _paragraph(
      'Hello ACOS',
      20,
      const ui.Color(0xFFFFD54F),
    );
    hello.layout(const ui.ParagraphConstraints(width: 300));
    canvas.drawParagraph(
      hello,
      ui.Offset(40, top + 180 + _animStep * 6),
    );
    print(
      'ACOS_DART_TEXT text=Hello ACOS frame=$_animStep width=${hello.width} '
      'height=${hello.height} longest=${hello.longestLine}',
    );
    print('ACOS_DART_ANIM step=$_animStep');
    hello.dispose();
    _animStep++;
    _dispatcher.scheduleFrame();
  } else if (_animStep >= _animFrames) {
    _animStep = -1;
  }

  final picture = recorder.endRecording();
  final builder = ui.SceneBuilder()..addPicture(ui.Offset.zero, picture);
  final scene = builder.build();
  view.render(scene, size: physical);

  scene.dispose();
  picture.dispose();
  title.dispose();
  label.dispose();
}

ui.Paragraph _paragraph(String text, double size, ui.Color color) {
  final style = ui.ParagraphStyle(
    textDirection: ui.TextDirection.ltr,
    fontSize: size,
  );
  final builder = ui.ParagraphBuilder(style)
    ..pushStyle(ui.TextStyle(color: color, fontSize: size))
    ..addText(text);
  return builder.build();
}

void _onPointerDataPacket(ui.PointerDataPacket packet) {
  print("ACOS_DART_POINTER n=${packet.data.length}");
  for (final ui.PointerData data in packet.data) {
    final view = _view;
    if (view == null) {
      continue;
    }
    final double dpr = view.devicePixelRatio;
    final ui.Offset pos = ui.Offset(
      data.physicalX / dpr,
      data.physicalY / dpr,
    );
    print(
      'ACOS_DART_POINTER_EVENT change=${data.change} pos=$pos '
      'rect=$_buttonRect down=$_buttonDown',
    );
    switch (data.change) {
      case ui.PointerChange.down:
        _buttonDown = _buttonRect.contains(pos);
        break;
      case ui.PointerChange.up:
        if (_buttonDown && _buttonRect.contains(pos)) {
          _counter += 1;
          // W130-A: the tap's effect must be visible in the engine log, not
          // inferred from pixels alone.
          print('ACOS_DART_COUNTER counter=$_counter at=$pos');
          // W169-A: start the animation on the next frame (this tap frame stays
          // at rest so the parity reference is unchanged).
          _animPending = true;
          // W156-A: the value string changes with the counter, so the semantic
          // node must be republished for `observe --tree` to reflect it.
          _semanticsPublished = false;
          _dispatcher.scheduleFrame();
        }
        _buttonDown = false;
        break;
      case ui.PointerChange.cancel:
        _buttonDown = false;
        break;
      default:
        break;
    }
  }
}
