// Minimal Flutter app that talks to the engine through dart:ui only.
//
// It deliberately does not import package:flutter: ACOS only needs a real
// Flutter engine (libflutter.so) plus a real AOT app snapshot (libapp.so),
// and this keeps the fixture buildable without the 1.5 GB Flutter SDK.
import 'dart:ui' as ui;

ui.PlatformDispatcher get _dispatcher => ui.PlatformDispatcher.instance;
ui.FlutterView? get _view => _dispatcher.implicitView;

int _counter = 0;
ui.Rect _buttonRect = ui.Rect.zero;
bool _buttonDown = false;

void main() {
  print("ACOS_DART_MAIN entered");
  _dispatcher.onBeginFrame = _onBeginFrame;
  _dispatcher.onDrawFrame = _onDrawFrame;
  _dispatcher.onPointerDataPacket = _onPointerDataPacket;
  _dispatcher.onMetricsChanged = () => _dispatcher.scheduleFrame();
  _dispatcher.scheduleFrame();
}

void _onBeginFrame(Duration _) {
  // The frame is drawn in _onDrawFrame.
  print("ACOS_DART_BEGINFRAME");
}

void _onDrawFrame() {
  print("ACOS_DART_FRAME draw");
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
    switch (data.change) {
      case ui.PointerChange.down:
        _buttonDown = _buttonRect.contains(pos);
        break;
      case ui.PointerChange.up:
        if (_buttonDown && _buttonRect.contains(pos)) {
          _counter += 1;
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
