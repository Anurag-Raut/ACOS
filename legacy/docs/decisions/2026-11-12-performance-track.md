# Decision 2026-11-12: performance track (agent-facing latency)

This is a separate track from the Zygote architecture work (2026-11-12-zygote-shared-art.md).

**Goal:** common agent operations take tens of milliseconds, ideally under 50 ms. These are targets; they never permit stale, incomplete or fabricated results.

## Targets (p50 and p95 reported)

| Operation | Target |
|---|---|
| Tap acknowledgement | ≤ 20–30 ms |
| Tap → usable updated state | ≤ 50 ms ideal, < 100 ms for simple apps |
| Fresh screenshot | ≤ 30 ms ideal, < 50 ms |
| Fresh UI tree with text | < 50 ms ideal, < 100 ms |
| Reset → first correct screen | < 300 ms initially |
| Warm launch and end-to-end verification | minimised and benchmarked separately |

## Owner approvals in this decision

- **Fast observation and strict observation are separate modes.**
  - Agents may navigate using fresh but possibly mid-animation observations, clearly labelled as such.
  - Strict settled-frame verification is preserved and stays the default for assertions and pixel checks.
  - Acknowledgement and observed state must remain distinct; success is never reported on acknowledgement alone.
- **Independent low-risk optimisations are approved.**
- **Still need escalation:** architectural changes, and changes to security or verification semantics.

## Rules

- Keep the ART engine intact and reuse AOSP.
- Never disable real Android animations to improve a benchmark.
- Do not overlap code with the Zygote track.
- Isolate measurements from heavy concurrent load.
- At most 5 agents; each task is one hour or less.
- Keep a change only if it shows a measured benefit with no regressions and zero false passes.

## Order of work

1. M5 screenshot and M8 input first, since their bottlenecks are known.
2. Then M7 UI tree and M6 reset.
3. Then the end-to-end loop, measured as verified tasks per second and per GiB-minute.
