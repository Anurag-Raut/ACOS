/**
 * ACOS React Native "hello" fixture (RN 0.81.4, Hermes, legacy/Paper arch).
 *
 * Deliberately JSX-free so the bundle inputs are plain JavaScript: the only
 * transform Metro has to perform is the CommonJS/module graph. The component
 * renders the minimal Text + Button surface ACOS must reproduce.
 *
 * @format
 */
'use strict';

const React = require('react');
const {
  Button,
  StyleSheet,
  Text,
  View,
} = require('react-native');

function RnHello() {
  const [count, setCount] = React.useState(0);
  return React.createElement(
    View,
    { style: styles.container, testID: 'hello-root' },
    React.createElement(Text, { style: styles.title, testID: 'hello-text' }, 'Hello ACOS'),
    React.createElement(Text, { style: styles.subtitle }, 'React Native + Hermes'),
    React.createElement(
      Button,
      {
        title: 'Tap me',
        testID: 'hello-button',
        onPress: () => setCount((c) => c + 1),
      },
    ),
    React.createElement(Text, { style: styles.count, testID: 'count-text' }, 'count: ' + count),
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, alignItems: 'center', justifyContent: 'center' },
  title: { fontSize: 24, fontWeight: 'bold' },
  subtitle: { fontSize: 14, marginTop: 4, marginBottom: 12 },
  count: { fontSize: 16, marginTop: 12 },
});

module.exports = {__esModule: true, default: RnHello};
