# Night plan — 2026-11-10 → 11-11 (orchestrator decision record)

Goal by morning: counter.apk starts, draws a first frame, responds to a tap, and the result is compared with the phone. Speed and memory beat the legacy baseline wherever measured.

Rule: don't get stuck on one approach. If a track stalls for more than ~45 min on one blocker, re-scope (structured UNSUPPORTED plus move on, or a different route) and ask the advisor (gpt-6.1-sol) once.

## Critical path (serial, always staffed)
1. M2 startup: counter.apk to Activity.onCreate/onResume (W40-A, then follow-up drivers)
2. M3 first frame through ViewRootImpl → HWUI → acos-render → FrameStore, frame-diff vs reference (W41-A)
3. UI tree (`acos observe --tree`) and tap round-trip on the live app (W42-A, then an interaction-replay agent)
4. Parity: same APK on the phone (ZA222WGJPP) vs ACOS: tree + screenshot diff (W42-A tool)
5. V1 benchmark run vs legacy baseline and STATUS refresh

## Parallel tracks (independent, launched now)
- Perf: IR tier field ops + inline caches (W39-A); launch latency + framework-init snapshot (W45-A)
- Resources/layout inflation + res_natives failures (W43-A), which feeds steps 2–3
- Gaten recon (W44-A, read-only), which yields the dogfood target list

## Queue (dispatch as agents finish, keep 6–8 running, RAM ≤85%)
- More fixture apps through launch (hello-views, list/scroll, text input, dialogs) to surface blockers in parallel
- Interaction replay (M5), text input via IME path
- JNI CallNonvirtual/DefineClass; native-lib gaps from Gaten recon
- Dogfood wave: an agent develops a new small app only via MCP/`acos` tools and records friction
- Honesty re-audit of the night's work before the morning summary

## Integration loop (orchestrator)
On each finish: cargo check + diff-dexprogs; selective commit + push; dispatch the next item from the queue.
