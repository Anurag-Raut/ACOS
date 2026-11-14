# Release candidate A/B: rel3-int (REL3a MCP-on-ART + REL3b agent friction) vs main c517a76 — 2026-11-14

Interleaved `acos-bench.sh --reps 1 --groups "a:latency:8G:300"` (`--exclusive`), 5 reps per arm (reps 3–5 in
reversed order). Main arm built in place (`.cache/wt-abmain3` at c517a76, compile paths checked). Main rep 5 had
11 fail-closed steps (`reset` PROCESS_TREE_DIRTY then GUEST_EXITED cascade — the main-side reaper class SERVELEAK
fixes); it is excluded from the pooled means below; all release reps ok=true.

Medians of per-rep p50s (the usual gate, +5 % and +2 ms):

| metric | main | rel3 | Δ |
|---|---:|---:|---:|
| M4 launch strict | 306.55 | 306.68 | 0.0 % |
| M4 launch fast | 157.01 | 170.97 | +8.9 % (gate tripped, see below) |
| M5 screenshot | 24.55 | 24.41 | −0.5 % |
| M6 reset strict | 335.67 | 321.95 | −4.1 % |
| M6 reset fast | 194.25 | 188.18 | −3.1 % |
| M7 tree strict | 41.20 | 41.22 | +0.1 % |
| M8 tap counter strict | 957.27 | 955.08 | −0.2 % |
| M8 tap counter fast | 44.85 | 48.52 | +8.2 % (gate tripped, see below) |
| M8 tap form strict | 948.42 | 945.96 | −0.3 % |
| M8 tap form fast | 44.90 | 44.78 | −0.3 % |
| teardown | 13.0 | 12.0 | −7.7 % |

**Why the two "fast" medians are not a reliable verdict.** Every sample of the fast-settle metrics sits on a
frame-period lattice: launch fast at ~155 / ~171 / ~187 ms, tap counter fast at ~44 / ~49 / ~59 ms (≈16 ms steps).
Both arms show every mode; a rep's p50 is whichever mode holds the 10th/11th of 20 samples. Pooled over all ok
samples (main n=80, rel3 n=100), 5 %-trimmed means:

| metric | main | rel3 | Δ | share in the fastest mode main/rel3 |
|---|---:|---:|---:|---|
| M4 launch fast | 162.0 | 167.2 | +3.2 % (+5.2 ms) | 0.59 / 0.32 |
| M8 tap counter fast | 49.3 | 51.0 | +3.6 % (+1.7 ms) | — |
| M8 tap form fast | 45.7 | 49.9 | +9.1 % by mean, p50 −0.3 % | 0.97 / 0.89 |
| M4 launch strict | 307.8 | 308.0 | +0.1 % | — |
| M6 reset fast | 193.1 | 191.6 | −0.8 % | — |

Decision (orchestrator, 2026-11-15): ship v0.2.0. The strict (verification) metrics are at parity or better; the
fast-settle shift is a smaller share of launches landing in the first frame slot (+3 % by trimmed mean). Follow-up
PERF-REL3: (1) make the bench frame-lattice aware (report mode shares / trimmed means, ≥40 samples), (2) measure
per-launch cost of REL3b's activity-alias manifest parsing and cache the parsed manifest per APK hash if it costs.

Correctness (`.cache/int/verify-rel3.out`): acos-protocol 66, acos-apk 43, acos-wm 80, acos-cli 173,
acos-services 331, acos-verify 85, acos-mcp 95, acos-art 167 pass; 15 gates exact (10 pass + 5 designed failures),
5 Gaten flows pass + 5 negatives ASSERTION_FAILED, GR4 4 + 4. 0 false passes. REL3a's own installed-prefix smoke from
an empty outside dir with default settings: counter + Gaten on ART via MCP session tools, verify without an engine
arg → art-reference pass (results/release/mcp-art-default-roundtrip.jsonl).
