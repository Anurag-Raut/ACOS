# gaten-home — phone visible text dumps

## step 0 launch [pass]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See a word you don't know", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 1 tap [pass]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
target: {"text": "", "content_desc": "Set up Gaten", "identifier": "", "enabled": true, "checked": null, "clickable": null, "bounds": null}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Open Gaten", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 2 tap [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Circle it", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 3 tap [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Circle it", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 4 tap [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Add to Anki", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See what it means", "Set up Gaten", "understanding, consent", "がいった。", "がてん", "なるほど、", "合点"]

## step 5 wait_for [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Add to Anki", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See what it means", "Set up Gaten", "understanding, consent", "がいった。", "がてん", "なるほど、", "合点"]

## step 6 capture [pass]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Add it to Anki", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "understanding, consent", "✓ Added", "がいった。", "がてん", "なるほど、", "合点"]

## step 7 assert_text [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
assertion: {"kind": "text", "expected": "Daily streak", "mode": "contains", "passed": false, "expected_bounds": null}
labels: ["+1", "Add it to Anki", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 8 assert_text [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
assertion: {"kind": "text", "expected": "Recently looked up", "mode": "contains", "passed": false, "expected_bounds": null}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See a word you don't know", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 9 assert_text [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
assertion: {"kind": "text", "expected": "Anki", "mode": "exact", "passed": false, "expected_bounds": null}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See a word you don't know", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 10 tap [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Open Gaten", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 11 wait_for [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Circle it", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 12 capture [pass]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
labels: ["Circle it", "Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "Set up Gaten", "がいった。", "なるほど、", "合点"]

## step 13 assert_text [fail]
screen: {"activity": "com.gaten/com.gaten.app.MainActivity", "package": "com.gaten", "app_bundle_in_snapshot": "com.gaten"}
assertion: {"kind": "text", "expected": "Study activity", "mode": "contains", "passed": false, "expected_bounds": null}
labels: ["Gaten looks up words you circle on screen and saves them to Anki.", "Look up Japanese in any app, then study it.", "See what it means", "Set up Gaten", "がいった。", "なるほど、", "合点"]
