# gaten-home — acos visible text dumps

## step 0 launch [pass]
screen: {}
labels: []

## step 1 tap [pass]
screen: {"package": "com.gaten", "frame_seq": 3, "content_hash": "e00297bde9285646332a05d11bd0c385"}
labels: ["1 / 3", "Android needs Gaten as an accessibility service.", "Choose text recognition", "Look up Japanese anywhere", "Look up Japanese anywhere. Step 1 of 3.", "Look up Japanese words and kanji while you read in other apps. Enable lookup mode to get started.", "Skip", "Skip lookup mode setup", "Standard or Enhanced", "Step 1 of 2, Turn on lookup mode, current", "Step 1 of 3", "Step 2 of 2, Choose text recognition, not started", "Turn on lookup mode"]

## step 2 tap [pass]
screen: {"package": "com.gaten", "frame_seq": 4, "content_hash": "f41c974b4d2e34f1e8767b7d29614c29"}
target: {"text": "Skip", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [498, 2163, 582, 2209]}
labels: ["2 / 3", "Back", "Install AnkiDroid", "Look up Japanese anywhere", "Look up Japanese anywhere. Step 1 of 3.", "Look up Japanese words and kanji while you read in other apps. Enable lookup mode to get started.", "Skip", "Skip Anki setup", "Step 2 of 3"]

## step 3 tap [pass]
screen: {"package": "com.gaten", "frame_seq": 5, "content_hash": "7cce30270dd22b96d45ac2617509b813"}
target: {"text": "Skip", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [498, 2163, 582, 2209]}
labels: ["2 / 3", "A separate app from Google Play.", "Allow access", "Back", "Connect AnkiDroid to review your decks and save words from your lookups as flashcards.", "Install AnkiDroid", "Read decks, record answers, add words.", "Skip", "Skip Anki setup", "Step 1 of 2, Install AnkiDroid, current", "Step 2 of 2, Allow access, not started", "Step 2 of 3", "Study with Anki", "Study with Anki. Step 2 of 3."]

## step 4 tap [pass]
screen: {"package": "com.gaten", "frame_seq": 6, "content_hash": "d86da439c94ec50294bce5985a8a6405"}
target: {"text": "Skip", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [498, 2163, 582, 2209]}
labels: ["3 / 3", "Back", "Connect AnkiDroid to review your decks and save words from your lookups as flashcards.", "Connect WaniKani", "Skip", "Skip WaniKani setup and finish", "Step 3 of 3", "Study with Anki", "Study with Anki. Step 2 of 3."]

## step 5 wait_for [pass]
screen: {"package": "com.gaten", "frame_seq": 7, "content_hash": "0896e038130f9779ff5f259298fde45c"}
target: {"text": "Recently looked up", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [58, 1424, 1024, 1482]}
labels: ["0", "Analytics", "Anki", "Anki Fri: study activity unknown", "Anki Mon: study activity unknown", "Anki Sat: study activity unknown, today", "Anki Sun: upcoming", "Anki Thu: study activity unknown", "Anki Tue: study activity unknown", "Anki Wed: study activity unknown", "Anki, Not connected, Open Anki", "Connect", "Daily streak", "Daily streak. Anki study days follow your collection's rollover.", "Fri", "Gaten", "Home", "Lookup mode off. Open screen lookup settings.", "Mon", "No recent searches found", "Not connected", "Open Anki", "Recently looked up", "Sat", "Search Japanese dictionary", "Settings", "Sun", "Thu", "Tue", "WaniKani", "WaniKani, Not connected, Connect", "Wed", "days", "猫, ねこ, neko..."]

## step 6 capture [pass]
screen: {"package": "com.gaten", "frame_seq": 8, "content_hash": "0896e038130f9779ff5f259298fde45c"}
labels: ["0", "Analytics", "Anki", "Anki Fri: study activity unknown", "Anki Mon: study activity unknown", "Anki Sat: study activity unknown, today", "Anki Sun: upcoming", "Anki Thu: study activity unknown", "Anki Tue: study activity unknown", "Anki Wed: study activity unknown", "Anki, Not connected, Open Anki", "Connect", "Daily streak", "Daily streak. Anki study days follow your collection's rollover.", "Fri", "Gaten", "Home", "Lookup mode off. Open screen lookup settings.", "Mon", "No recent searches found", "Not connected", "Open Anki", "Recently looked up", "Sat", "Search Japanese dictionary", "Settings", "Sun", "Thu", "Tue", "WaniKani", "WaniKani, Not connected, Connect", "Wed", "days", "猫, ねこ, neko..."]

## step 7 assert_text [pass]
screen: {"package": "com.gaten", "frame_seq": 8, "content_hash": "0896e038130f9779ff5f259298fde45c"}
target: {"text": "Daily streak", "content_desc": "Daily streak. Anki study days follow your collection's rollover.", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [150, 610, 764, 663]}
assertion: {"kind": "text", "passed": true, "expected": {"contains": "Daily streak"}, "message": null}
labels: ["0", "Analytics", "Anki", "Anki Fri: study activity unknown", "Anki Mon: study activity unknown", "Anki Sat: study activity unknown, today", "Anki Sun: upcoming", "Anki Thu: study activity unknown", "Anki Tue: study activity unknown", "Anki Wed: study activity unknown", "Anki, Not connected, Open Anki", "Connect", "Daily streak", "Daily streak. Anki study days follow your collection's rollover.", "Fri", "Gaten", "Home", "Lookup mode off. Open screen lookup settings.", "Mon", "No recent searches found", "Not connected", "Open Anki", "Recently looked up", "Sat", "Search Japanese dictionary", "Settings", "Sun", "Thu", "Tue", "WaniKani", "WaniKani, Not connected, Connect", "Wed", "days", "猫, ねこ, neko..."]

## step 8 assert_text [pass]
screen: {"package": "com.gaten", "frame_seq": 8, "content_hash": "0896e038130f9779ff5f259298fde45c"}
target: {"text": "Recently looked up", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [58, 1424, 1024, 1482]}
assertion: {"kind": "text", "passed": true, "expected": {"contains": "Recently looked up"}, "message": null}
labels: ["0", "Analytics", "Anki", "Anki Fri: study activity unknown", "Anki Mon: study activity unknown", "Anki Sat: study activity unknown, today", "Anki Sun: upcoming", "Anki Thu: study activity unknown", "Anki Tue: study activity unknown", "Anki Wed: study activity unknown", "Anki, Not connected, Open Anki", "Connect", "Daily streak", "Daily streak. Anki study days follow your collection's rollover.", "Fri", "Gaten", "Home", "Lookup mode off. Open screen lookup settings.", "Mon", "No recent searches found", "Not connected", "Open Anki", "Recently looked up", "Sat", "Search Japanese dictionary", "Settings", "Sun", "Thu", "Tue", "WaniKani", "WaniKani, Not connected, Connect", "Wed", "days", "猫, ねこ, neko..."]

## step 9 assert_text [pass]
screen: {"package": "com.gaten", "frame_seq": 8, "content_hash": "0896e038130f9779ff5f259298fde45c"}
target: {"text": "Anki", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [187, 1001, 766, 1054]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Anki"}, "message": null}
labels: ["0", "Analytics", "Anki", "Anki Fri: study activity unknown", "Anki Mon: study activity unknown", "Anki Sat: study activity unknown, today", "Anki Sun: upcoming", "Anki Thu: study activity unknown", "Anki Tue: study activity unknown", "Anki Wed: study activity unknown", "Anki, Not connected, Open Anki", "Connect", "Daily streak", "Daily streak. Anki study days follow your collection's rollover.", "Fri", "Gaten", "Home", "Lookup mode off. Open screen lookup settings.", "Mon", "No recent searches found", "Not connected", "Open Anki", "Recently looked up", "Sat", "Search Japanese dictionary", "Settings", "Sun", "Thu", "Tue", "WaniKani", "WaniKani, Not connected, Connect", "Wed", "days", "猫, ねこ, neko..."]

## step 10 tap [pass]
screen: {"package": "com.gaten", "frame_seq": 10, "content_hash": "d96cc99ed8ac8bd19d69eed4aedac1fb"}
target: {"text": "", "content_desc": "Analytics", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [58, 193, 854, 308]}
labels: ["12 weeks · Anki", "Accuracy", "Analytics", "Anki", "Anki is not installed.", "Answer accuracy will appear here.", "Collection", "Connect", "Connect to see WaniKani progress.", "Home", "Install", "Not connected", "Open the Anki installation flow.", "Progress", "Refresh the numbers", "Settings", "Study activity", "Study history will appear here.", "WaniKani"]

## step 11 wait_for [pass]
screen: {"package": "com.gaten", "frame_seq": 11, "content_hash": "d96cc99ed8ac8bd19d69eed4aedac1fb"}
target: {"text": "Accuracy", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [58, 1758, 229, 1811]}
labels: ["12 weeks · Anki", "Accuracy", "Analytics", "Anki", "Anki is not installed.", "Answer accuracy will appear here.", "Collection", "Connect", "Connect to see WaniKani progress.", "Home", "Install", "Not connected", "Open the Anki installation flow.", "Progress", "Refresh the numbers", "Settings", "Study activity", "Study history will appear here.", "WaniKani"]

## step 12 capture [pass]
screen: {"package": "com.gaten", "frame_seq": 12, "content_hash": "d96cc99ed8ac8bd19d69eed4aedac1fb"}
labels: ["12 weeks · Anki", "Accuracy", "Analytics", "Anki", "Anki is not installed.", "Answer accuracy will appear here.", "Collection", "Connect", "Connect to see WaniKani progress.", "Home", "Install", "Not connected", "Open the Anki installation flow.", "Progress", "Refresh the numbers", "Settings", "Study activity", "Study history will appear here.", "WaniKani"]

## step 13 assert_text [pass]
screen: {"package": "com.gaten", "frame_seq": 12, "content_hash": "d96cc99ed8ac8bd19d69eed4aedac1fb"}
target: {"text": "Study activity", "content_desc": "", "identifier": "", "enabled": true, "checked": false, "clickable": false, "bounds": [58, 1262, 312, 1315]}
assertion: {"kind": "text", "passed": true, "expected": {"contains": "Study activity"}, "message": null}
labels: ["12 weeks · Anki", "Accuracy", "Analytics", "Anki", "Anki is not installed.", "Answer accuracy will appear here.", "Collection", "Connect", "Connect to see WaniKani progress.", "Home", "Install", "Not connected", "Open the Anki installation flow.", "Progress", "Refresh the numbers", "Settings", "Study activity", "Study history will appear here.", "WaniKani"]
