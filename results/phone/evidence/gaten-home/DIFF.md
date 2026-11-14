### gaten-home

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 1 | tap | `{"text": {"contains": "Set up Gaten"}}` | pass / - / - | pass / Set up Gaten / - | 1 / 3, Android needs Gaten as an accessibility service., Choose text recognition, Look up  | - | **text-set** |
| 2 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 2 / 3, Back, Install AnkiDroid, Look up Japanese anywhere, Look up Japanese anywhere. Step | - | **missing-on-phone** |
| 3 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 2 / 3, A separate app from Google Play., Allow access, Back, Connect AnkiDroid to review y | - | **missing-on-phone** |
| 4 | tap | `{"text": {"exact": "Skip"}}` | pass / Skip / [498,2163][582,2209] | fail / - / - | 3 / 3, Back, Connect AnkiDroid to review your decks and save words from your lookups as fl | - | **missing-on-phone** |
| 5 | wait_for | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 6 | capture | `-` | pass / - / - | pass / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **text-set** |
| 7 | assert_text | `{"text": {"contains": "Daily streak"}}` | pass / Daily streak / [150,610][764,663] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 8 | assert_text | `{"text": {"contains": "Recently looked up"}}` | pass / Recently looked up / [58,1424][1024,1482] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 9 | assert_text | `{"text": {"exact": "Anki"}}` | pass / Anki / [187,1001][766,1054] | fail / - / - | 0, Analytics, Anki, Anki Fri: study activity unknown, Anki Mon: study activity unknown, An | - | **missing-on-phone** |
| 10 | tap | `{"content_desc": {"exact": "Analytics"}}` | pass / Analytics / [58,193][854,308] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |
| 11 | wait_for | `{"text": {"contains": "Accuracy"}}` | pass / Accuracy / [58,1758][229,1811] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |
| 12 | capture | `-` | pass / - / - | pass / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **text-set** |
| 13 | assert_text | `{"text": {"contains": "Study activity"}}` | pass / Study activity / [58,1262][312,1315] | fail / - / - | 12 weeks · Anki, Accuracy, Analytics, Anki, Anki is not installed., Answer accuracy will a | - | **missing-on-phone** |

Counts: match=1, missing-on-phone=10, text-set=3
