// Minimal Flutter app that talks to the engine through dart:ui only.
//
// It deliberately does not import package:flutter: ACOS only needs a real
// Flutter engine (libflutter.so) plus a real AOT app snapshot (libapp.so),
// and this keeps the fixture buildable without the 1.5 GB Flutter SDK.
import 'dart:typed_data';
import 'dart:ui' as ui;

ui.PlatformDispatcher get _dispatcher => ui.PlatformDispatcher.instance;
ui.FlutterView? get _view => _dispatcher.implicitView;

int _counter = 0;
ui.Rect _buttonRect = ui.Rect.zero;
bool _buttonDown = false;

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
  _dispatcher.onSemanticsEnabledChanged = _onSemanticsEnabledChanged;
  // W156-A: handle the accessibility action channel so `acos tap --text` can
  // drive the button through `FlutterJNI.dispatchSemanticsAction` as well as
  // the pointer path.
  _dispatcher.onSemanticsAction = _onSemanticsAction;
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

/// W156-A: the engine-invoked accessibility action handler (`nativeDispatchSemanticsAction`
/// -> `_dispatchSemanticsAction`). The raw `dart:ui` probe handles the tap on
/// node 1 itself; a widget app would get this from the framework.
void _onSemanticsAction(int id, ui.SemanticsAction action, ByteData? args) {
  print('ACOS_DART_SEMANTICS_ACTION id=$id action=$action');
  if (id == 1 && action == ui.SemanticsAction.tap) {
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

void _onBeginFrame(Duration _) {
  // The frame is drawn in _onDrawFrame.
  print("ACOS_DART_BEGINFRAME");
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
