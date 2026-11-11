// Real-widget Flutter app for ACOS: Material counter + text field + list.
//
// Unlike the raw `dart:ui` probe, this rides the full framework:
// WidgetsBinding, Material, gesture + semantics compilation, EditableText.
// It is the W189-A fixture: it proves ACOS can run an unmodified framework
// app on the *release* AOT path.
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

void main() {
  runApp(const AcosCounterApp());
}

class AcosCounterApp extends StatelessWidget {
  const AcosCounterApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ACOS Counter',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorSchemeSeed: const Color(0xFF6750A4)),
      home: const CounterPage(),
    );
  }
}

class CounterPage extends StatefulWidget {
  const CounterPage({super.key});

  @override
  State<CounterPage> createState() => _CounterPageState();
}

class _CounterPageState extends State<CounterPage> {
  int _counter = 0;
  final TextEditingController _controller = TextEditingController();
  final List<String> _items = <String>['alpha', 'beta', 'gamma'];

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _increment() {
    setState(() {
      _counter += 1;
      _items.add('item $_counter');
    });
    // ACOS fixture instrumentation: the counter evidence for a real-widget app
    // (the raw dart:ui probe printed `ACOS_DART_COUNTER` from its own handler).
    debugPrint('ACOS_APP increment counter=$_counter');
  }

  void _addFromField() {
    final String text = _controller.text.trim();
    if (text.isEmpty) {
      return;
    }
    setState(() {
      _items.add(text);
      _controller.clear();
    });
    debugPrint('ACOS_APP submitted text=$text items=${_items.length}');
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('ACOS Counter')),
      body: Column(
        children: <Widget>[
          Padding(
            padding: const EdgeInsets.all(16),
            child: Text(
              'Count: $_counter',
              key: const Key('count_label'),
              style: Theme.of(context).textTheme.headlineSmall,
            ),
          ),
          ElevatedButton(
            key: const Key('increment_button'),
            onPressed: _increment,
            child: const Text('Increment'),
          ),
          Padding(
            padding: const EdgeInsets.all(16),
            child: TextField(
              key: const Key('entry_field'),
              controller: _controller,
              decoration: const InputDecoration(
                labelText: 'Add an item',
                border: OutlineInputBorder(),
              ),
              onSubmitted: (_) => _addFromField(),
            ),
          ),
          Expanded(
            child: ListView.builder(
              key: const Key('item_list'),
              itemCount: _items.length,
              itemBuilder: (BuildContext context, int index) {
                return ListTile(title: Text(_items[index]));
              },
            ),
          ),
        ],
      ),
    );
  }
}
