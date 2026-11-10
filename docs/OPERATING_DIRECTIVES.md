# ACOS operating directives (everything the user has said; read this after any compaction)

Last updated 2026-11-10. The user is asleep or away. Do not block on them. Decide, act, record.

## 1. What ACOS is
- A clean-sheet, agent-only Android execution OS. A Rust DEX VM runs the real Android 14 framework bytecode; only the layers below the framework are replaced in Rust.
- Purpose: AI agents develop an Android app, run it, see it (UI tree + frames), tap it and verify, fast. Agent-only: no human UX.
- It runs as an ordinary program on Linux. It is not hardware emulation and not a layer on a phone.
- Core promise (the user's words, reconfirmed): an APK developed by agents with ACOS must look and behave the same on real Android. No absolute guarantee is possible, so we build evidence:
  - run Google's real framework code
  - compare constantly with real ART and the physical phone (moto g35, adb ZA222WGJPP)
  - never fake: unsupported behaviour gives a structured UNSUPPORTED_* error
  - a parity check for every app (ACOS tree and frame vs phone: matches / list of differences)
- It must beat the legacy ACVM on every tracked benchmark metric (BENCHMARKS.md). The old architecture is not binding if a better one improves the metrics.

## 2. Roles and working style
- Claude is the orchestrator: plan, dispatch, review, verify, commit, decide. All implementation goes to Paseo subagents on provider `pi`, model `openrouter/deepseek/deepseek-v4.1-flash`, thinking high (profile "Implementer Deepseek"; confirm with list_profiles).
- Keep Claude usage LOW (the user asked explicitly):
  - do not poll
  - read only short reports
  - delegate research, review and implementation
  - the 12-minute heartbeat (id ad99c82f) does only a cheap list-agents + RAM check and is silent unless action is needed
  - if confused, ask the advisor: codex gpt-6.1-sol (analysis only)
- Decide everything yourself and do not wait for the user. Record decisions and findings in repo files.
- Ping the user via KDE Connect on "Nord 6" only if truly blocked.
- Do not get hellbent on one idea. Move fast; if a track stalls for more than about 45 minutes, re-scope (structured UNSUPPORTED, another route, ask the advisor once).
- Maximise parallelism: run as many agents as RAM allows (the user said 10-20 is fine) when tasks do not depend on each other. Target 6-8+ running while RAM stays at or under 85%.

## 3. Never stop
- The user is asleep and said: do not stop until told, keep running agents, keep improving, and do not stop after the "working version" either.
- On every agent-finished notification and every heartbeat: verify, commit, push, dispatch the next task from the queue. Never leave zero agents running.
- Keep a concise morning summary of what moved forward.
- After the first working version, keep improving without end:
  - more complex apps: lists, input, dialogs, multi-screen
  - the user's Gaten app at /home/Qui/Projects/gaten (dev branch; read-only; build copies in scratch dirs). See results/gaten/RECON.md.
  - closer phone parity
  - speed, launch time and memory
  - dogfood waves where agents build real apps with ACOS tools and the friction is fixed
  - regular honesty re-audits

## 4. Git and approvals
- Commit verified work locally and push to origin (https://github.com/Anurag-Raut/ACOS.git). Approved standing permission.
- No attribution lines in commit messages or PR descriptions.
- Commit selectively when other agents' work in progress overlaps. Avoid `git add -A` sweeping unfinished work.
- Verified means: `cargo check --workspace --all-targets` green and `tools/diff-dexprogs.sh --corelib real` all pass. Normal and GC-stress modes where relevant.

## 5. Host and device safety (hard limits)
- Host RAM must never exceed 90% (internal target 85%). Guards act below about 9 GiB MemAvailable. Use tools/guarded.sh (`--mem`, `--time`) and tools/ramguard.sh. Free resources as soon as they are not needed (delete scratch target dirs and worktrees).
- Never modify /home/Qui/Work/android-container.
- No sudo, kernel changes, swapoff, device configuration, hard resets, deleting old worktrees, or downloads over 2 GB without explicit authorisation.
- Do not run arbitrary APKs in privileged Docker or as host root.
- Downloaded and extracted archives are untrusted: put them in their own empty dir, keep scripts elsewhere, use `python -I`.
- New source is written only under ~/Projects/acos, except approved artifact paths.
- Never include the user's name, email or project details in requests to external services.
- Emulator: at most 2048 MB guest RAM, one instance, always shut down, only when MemAvailable is above 16 GiB.
- Phone (adb ZA222WGJPP, moto g35, Android 15): the user granted adb access. Install and uninstall only our own fixture apps. Do not change settings, accounts or security.
- Use the agent-device skill or MCP for device interaction, not hand-rolled adb, unless it cannot cover the task.

## 6. Agent mechanics (so dispatches stay consistent)
- Each agent: self-contained prompt with files owned, files owned by others (do not touch), acceptance command, a report deadline of about 40 minutes with a table, and "do not commit".
- Build in a clean git worktree of HEAD when the shared tree may not compile. Use CARGO_TARGET_DIR=.cache/target-<id>, CARGO_PROFILE_DEV_DEBUG=0, run via tools/guarded.sh, copy the diff back only when it applies cleanly, and delete target and worktree at the end.
- Agent names W<wave>-<letter> are only launch order. The title carries the role.
- Titles are at most 60 characters.

## 7. Current plan
- See results/NIGHT_PLAN_2026-11-10.md for the critical path (M2 startup → M3 first frame → UI tree + tap → phone parity → V1 benchmark), the parallel tracks and the queue.
- Status at the time of writing:
  - done: engine, services, input, fonts and text, annotations and Proxy (commit 66e4232), audit fixes (ccd7ee7)
  - open: M2 blocked at an ExceptionInInitializerError in application_on_create (W40-A); the `unknown service name "dev.acos.fixtures.counter"` check is also assigned to W40-A
  - running: W39-A, W40-A, W41-A, W42-A, W43-A, W44-A, W45-A
