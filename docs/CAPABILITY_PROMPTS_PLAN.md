# v0.6.1 Plan — On-Demand Capability Prompts & Hot-Attachable Settings

## Problem Statement

When a user or agent tests a real Android app (e.g., Gaten) on ACOS with default modules (`off` per ADR-0029):

1. **Missing opt-in capabilities look like dead buttons:** Tapping `"Open settings"` (sending `android.settings.ACCESSIBILITY_SETTINGS`) silently fails (`START_NOT_FOUND = -91` → `ActivityNotFoundException`, often swallowed by the app) when `system.settings` / `accessibility` are `off`. Even with `system.settings:real`, `com.android.settings` previously only handled `SETTINGS`, `WIFI_SETTINGS`, and `APPLICATION_DETAILS_SETTINGS` — not `ACCESSIBILITY_SETTINGS`.
2. **Captured external intents give no interactive feedback:** Tapping `"Install AnkiDroid"` fires `ACTION_VIEW` for `https://play.google.com/store/apps/details?id=com.ichi2.anki`, which [`intents.rs`](../crates/acos-services/src/intents.rs) captures cleanly for headless agents, but in `acos view` nothing appears on screen.

## Metric & Honesty Invariants

- **0 ms boot / 0 MiB RAM overhead when `off`:** All optional modules remain `off` by default. Capability prompts only allocate on error/capture paths.
- **Zero false passes:** Modules are **never** auto-attached. Enabling a module is always an explicit human click in `acos view` or an explicit agent call (`acos_services`), recorded honestly in `modules[]`.
- **Screenshot & UI-tree purity:** Interactive human popups live strictly in the host `acos view` web UI chrome — never injected into the Android window stack.
- **No Play Store / browser bloat:** External store links stay honestly captured with a clear notice (`--extra-apk` for multi-app testing).

## Architecture (3 Steps)

### Step 1 — Unified `CapabilityPrompt` Broker (`acos-services`, `acos-art`, `acos-cli`, `acos-mcp`)
- Add [`crates/acos-services/src/capability_prompts.rs`](../crates/acos-services/src/capability_prompts.rs) owned by `ServiceHost` and shared with `AmsState` / `AtmsState`.
- Emit bounded, structured `CapabilityPrompt` records (`seq`, `utc_ms`, `trigger`, `request`, `title`, `message`, `options[]`) from three host-side paths:
  1. `intent_not_found`: `android.settings.*` actions supported by `com.android.settings` when `system.settings` is not attached (preserving the original `IntentFacts` in a bounded replay buffer so `replay_intent` can re-fire it after attach).
  2. `service_tombstone`: unattached binder service lookups hitting `TOMBSTONES` (naming the module and its `AttachMode`).
  3. `intent_captured`: unmatched `ACTION_VIEW` / `ACTION_SEND` / `ACTION_PICK` / `ACTION_GET_CONTENT` intents captured by `IntentState`, with dedicated store-link detection (`play.google.com/store/apps/details?id=<pkg>` and `market://details?id=<pkg>`).
- **Agent surfacing:**
  - Actions (`tap`, `key`, `type_text`, `swipe`) snapshot the prompt cursor before dispatch and attach `"capability_prompts": [...]` to the reply **only when non-empty** (0 extra tokens on normal actions).
  - `{"cmd": "services", "op": "list" | "status"}` includes `"capability_prompts"` when non-empty; new `{"cmd": "services", "op": "replay_intent", "intent_id": N}` replays a saved intent after the agent/user attaches `system.settings`.
- **Live view event stream:**
  - `acos-art` pushes new `CapabilityPrompt`s into `LiveViewHandle` (`{"kind": "capability_prompt", "prompt": ...}`). With 0 subscribers, `LiveViewHandle::push_event` is a single atomic load (0 cost).

### Step 2 — `ACTION_ACCESSIBILITY_SETTINGS` Toggle + Hot-Attach `system.settings`
- Extend [`fixtures/apps/settings/`](../fixtures/apps/settings):
  - Declare `android.settings.ACCESSIBILITY_SETTINGS`, `android.settings.APP_NOTIFICATION_SETTINGS`, and `android.settings.action.MANAGE_OVERLAY_PERMISSION` (with and without `package:` scheme) in `AndroidManifest.xml`, plus `WRITE_SECURE_SETTINGS`.
  - In `SettingsActivity.java`, when opened for `ACTION_ACCESSIBILITY_SETTINGS`, list services from `AccessibilityManager.getInstalledAccessibilityServiceList()` with native `Switch` toggles that write `Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES` and `Settings.Secure.ACCESSIBILITY_ENABLED`.
  - Reuses ACOS's existing [`ServiceHost::take_accessibility_deliveries`](../crates/acos-services/src/host.rs) + [`a11y_bind.rs`](../crates/acos-services/src/a11y_bind.rs) settings observer to bind/unbind the real `AccessibilityService` live with no new host service.
- Upgrade `system.settings` from `AttachMode::AtLaunch` → `AttachMode::Hot`:
  - `SettingsModule` remains zero-cost when `off` and lazily spawned by `SecondaryPool` when `real`.
  - On live `attach` (`system.settings:real`), `acos-art` stages `settings.apk` into the session's run dir, installs it into `PMS` + `ATMS` (`ManifestDb`), and registers its `SecondarySpec` in `SecondaryPool` — zero restart of the primary app.
  - On live `detach`, `acos-art` stops any running `com.android.settings` secondary process and removes its package/manifest entries from `PMS` + `ATMS`, restoring honest `START_NOT_FOUND`.

### Step 3 — `acos view` Interactive Popup & `panel_capability` Route
- Add `panel_capability` to [`crates/acos-cli/src/engine/serve.rs`](../crates/acos-cli/src/engine/serve.rs) and `InputCommand::CapabilityAction` to [`crates/acos-cli/src/view/hub.rs`](../crates/acos-cli/src/view/hub.rs) (strictly disabled on watch-only verify sessions):
  - Executes the user's chosen option: hot-attach module(s), optional control op (e.g. `accessibility.enable`), and optional `replay_intent`.
- Update [`crates/acos-cli/src/view/page.rs`](../crates/acos-cli/src/view/page.rs):
  - Render a dismissible floating card over the viewer chrome when `capability_prompt` arrives (and log it in the activity feed).
  - Clicking an action button invokes `{"t": "capability", ...}` over the input WebSocket and clears the card on completion.
