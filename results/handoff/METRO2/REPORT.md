# METRO2c — why a Metro reload showed the OLD tree twice (8 → 16 nodes)

**Branch** `metro2c` (from `metro2b` c6d9921b5 + `modules` 16ba6a6e7). **Status: PARTIAL.**
Root cause found and split into three separate things; one ACOS bug fixed (with a
test that fails before / passes after), the RN-side race documented with
measurements, and RN's recommended dev path (Fast Refresh) proven 5/5. The
`scenarios/metro-reload.yaml` file is **not** created — see §6.

Evidence index (all under `results/handoff/METRO2/`, 0.9 MB; the METRO2b `rep0*`
dirs held 34 MB, mostly 4.9 MB bundle copies, and were replaced by this set):
`run3` `run4` `run6..run11` = single runs, `hmr1..hmr5` = the 5/5 repeats.

## 1. Three different things were mixed up in the METRO1 symptom

| # | symptom | cause | owner |
|---|---|---|---|
| a | the guest re-ran the **old text** after `/reload` | the test reloaded *before Metro had rebuilt*: the edit and `POST /reload` were back to back, so the app fetched (and faithfully re-ran) the **old** bundle. The host curl 90 s later saw the new text — that is why METRO1 concluded "the server is right". Fixed in the driver: wait until Metro itself serves the new text, then reload. | test driver |
| b | the tree **doubles** (8 → 16 → 24 …) and `Trying to add a root view with an explicit id (11) already set` | RN 0.81 **legacy-architecture** reload race, §2. Not a window/ViewRootImpl/Looper/id-reuse problem. | React Native |
| c | the guest **aborts on the 4th reload** (`SurfaceComposerClient::createSurface error Invalid argument` → `BLASTBufferQueue: mSurfaceControl must not be NULL` → SIGABRT) | a real ACOS bug: `SurfaceState::destroy_layer_tree` never gave a layer's handle binder back to the **16-entry pool**, so a session could create only 16 layers *in total*. Each reload shows and dismisses a loading popup window = 2 layers. Fixed (§3). | ACOS |

(c) was hiding behind (b): in run6 and run7 the session died at reload #4 both times.

## 2. (b) the doubling is RN's own legacy reload race

Read from `.cache/rn-0.81.4` (ReactInstanceManager, CatalystInstanceImpl,
UIImplementation, UIViewOperationQueue, NativeViewHierarchyManager):

* `recreateReactContextInBackground` → `tearDownReactContext` (UI thread):
  `detachRootViewFromInstance` only **asks the old JS** to
  `AppRegistry.unmountApplicationComponentAtRootTag(tag)`. For Paper nothing on the
  Java side clears the `ReactRootView` (`clearReactRoot` — `removeAllViews` +
  `setId(NO_ID)` — runs only for Fabric and on the very first attach).
* The old tree is removed only if, **before the new context's `setupReactContext`
  calls `addRootView`** on the same `ReactRootView`, the old JS thread ran the
  unmount, the old native-modules thread forwarded `removeRootView`, and the UI
  thread ran `RemoveRootViewOperation` (`dropView(root)` = `removeAllViews`, then
  `setId(NO_ID)`). `addRootView` runs on the new native-modules thread (the source
  comment says "horribly not-thread-safe") and logs
  `...explicit id (11) already set` when it wins the race.
* When the old chain loses: the old children stay attached, the new context mounts
  next to them (8 → 16), and the late `RemoveRootViewOperation` of the *old*
  `NativeViewHierarchyManager` can then wipe/reset the new root (`16 → 24 → … → 8`
  in `run6`). `CatalystInstanceImpl.destroy()` also kills the old `js` queue, which
  is the `Tried to enqueue runnable on already finished thread: 'js…'` line (it
  appears in every reload, clean or not, so it is a symptom, not the cause).

### Measurements (all on real ART, one session per run)

Old JS unmount runs **+4 ms** after the new context's creation thread starts
(`M2T unmount` vs `M2T create#n start`, `run8`/`run9`). The UI thread is *not*
stalled by ACOS in that window (40 markers 2 ms apart, only 5–7 ms latency twice;
`run9`). So ACOS is not slowing the old chain; the new chain is simply fast.

| extra delay before the new context's executor is created (TEMP probe, removed) | reloads | clean (8 nodes, no `explicit id`) |
|---|---|---|
| 0 ms (stock) | `run3`,`run6`,`run9`,`run10` (14 reloads) | **3 of 14 ended at 8 nodes**, 11 grew the tree (`run10`: 16,24,32,40,48,8,16 — the lone 8 is the late old-root removal wiping the stack, not a clean replace) |
| 40 / 80 / 120 ms (stepped) | `run8` | **3 / 3** |
| 400 ms | `run7` | **3 / 3** (4th reload hit the pool bug, §3) |

So the race margin on ACOS is a few ms; on a phone `createReactContext` takes
hundreds of ms, which is why the same app reloads cleanly there. ACOS does not need
to (and must not) slow itself down to match.

## 3. (c) the ACOS bug: layer-handle pool leak — fixed

`crates/acos-services/src/surface_client.rs`: `destroy_layer_tree` removed layers
and tombstoned their handles but never returned the handle binders to
`handle_pool` (`LAYER_HANDLE_POOL = 16`, pre-allocated because `createSurface`
cannot mint a node). The doc comment on `destroyed` already described reissue
("removed when the handle is handed out again") — the reissue path did not exist.
Now each destroyed handle goes back **FIFO** (inserted at the head, `take_handle`
pops the tail) and the tombstone stays until `create_layer` reissues it, so a late
transaction naming a dead layer is still refused.

* Test `destroyed_layers_return_their_handles_so_a_long_session_never_exhausts_the_pool`
  (`crates/acos-services/tests/w42_layer_lifetime.rs`): **fails before**
  (`createSurface: BadRequest ... layer-handle pool exhausted (16 handles)`), passes
  after; it also pins that the *live* set stays bounded (pool+1 live layers refused).
* ART: before — dead at reload #4 in 2/2 runs (`run6`, `run7`); after — `run10`
  survived 7 consecutive reloads.

## 4. RN's recommended path: Fast Refresh — deterministic, 5/5

A *component* edit does not need a context restart. With the fixture laid out like
RN's own template (`App.js` exports the component, `index.js` registers it —
`tools/metro/metro2-prepare-app.sh`), editing `App.js` is applied over the
`/hot` websocket through the same host-port forwarder (no new network work):
same JS context, no teardown race, no stale root.

```
tools/metro/metro1-dev-loop.sh   with METRO_EDIT_FILE=App.js METRO_NO_RELOAD=1
tools/metro/metro2-verdict.py    -> {reload:{mode, tree_nodes_before/after, new_text_copies,
                                             old_text_copies, stale_roots, js_contexts_started,
                                             explicit_id_errors}, pass}
```

`hmr1..hmr5/verdict.json` — five fresh sessions, each:
`mode=fast-refresh context=same tree 8→5 new_text_copies=1 old_text_copies=0
stale_roots=0 js_contexts_started=1 explicit_id_errors=0 pass=true`. The 8→5 is
the LogBox "legacy architecture" banner (a group + 2 texts) being dismissed by the
update, not content; the verdict requires `after <= before`.

An **explicit** full reload (`/reload`, `r`, dev-menu Reload; also what an edit to a
non-refreshable module such as `index.js` triggers) takes RN's legacy teardown and
is racy on ACOS as measured above. The verdict names it: `stale_roots` / `explicit_id_errors`
fail the run, never pass it.

## 5. Environment findings the orchestrator should know

* **New worktree path ⇒ every ART build-cache key misses** (`file_id` hashes the
  absolute path). On the first run here the regenerated `linkerconfig` is the strict
  `dir.system` config; with it `derive_classpath` failed (`library
  "/data/shim/libartshim.so" … not accessible for the namespace`) and, once that was
  bypassed, `dalvikvm64` failed (`library "libnativehelper.so" not found`).
  The persistent `.cache/art/data` carries the legacy (`dir.legacy`) config and
  classpath. I did not root-cause why a fresh regeneration differs (suspect: the
  `ro.vndk.version=34` that `props.rs` gained in `modules`, but unverified); my runs
  seed `linkerconfig/` + `system/environ/classpath` from the persistent data dir
  inside the `run-exclusive.sh` wrapper (`.cache/m/seed-run.sh`, not committed).
  Any agent whose first ART run in a *new* worktree path regenerates these will hit it.
* `tools/build-dogfood-apps.sh` is needed for `acos-services`' `pms`/`apk_facts_cache`
  tests (not a regression; they fail with a missing-prerequisite message otherwise).
* the guest logcat timestamp column is constant for a whole session (all lines carry
  the session start) — timing in this report therefore comes from `System.currentTimeMillis()`
  strings in the probe, not from logcat.

## 6. Acceptance

| criterion | verdict | evidence |
|---|---|---|
| root-cause the stale/doubled root | **PASS** | §1–§3; RN source + measurements |
| fix the ACOS-side root cause | **PASS** for the ACOS bug found (pool leak); the doubling itself is RN's, documented | `surface_client.rs`, test, `run10` |
| use RN's recommended path, no fixture hack | **PASS** — Fast Refresh | `hmr1..5`, no fixture/app code changed (TEMP probe removed; `MainApplication.java` restored to its METRO1 form) |
| one process, `assert_text` new string, no stale copy, tree not grown, 5/5 | **PASS for Fast Refresh** (`hmr1..5`); **NOT achievable for a full `/reload`** (race: ~20 % end at one tree) | `hmr*/verdict.json`, `run10` |
| `scenarios/metro-reload.yaml`, `needs_env: [pasta, metro]` | **NOT DONE** (blocked) | below |
| cargo test, touched crates | **PASS** `acos-services` (all targets, after building the app fixtures); `w42_layer_lifetime` 5/5 | session log |
| evidence < 10 MB | **PASS** 0.9 MB | |

**Why no scenario.** A reload scenario has to edit a host file and trigger the
reload *between* guest assertions, and the scenario step vocabulary has no host
dev-server step (and `KNOWN_NEEDS_ENV` has no `metro`). A file that cannot run is
worse than none (0 false passes). Smallest honest next step: a `dev_server` step
(`op: edit {file, replace}` / `op: reload` / `op: wait_serves {text}`) run by the
scenario executor against a Metro it owns (start/stop with the session, same
host-port opt-in as METRO1), plus `metro` in `KNOWN_NEEDS_ENV` / `run-matrix.sh`
(`node` + the RN project cache present). The scenario then uses **Fast Refresh** for
the pass arm and a separate `expectation`-pinned arm for explicit reload.

## 7. Decision for the owner (architectural — I did not pick)

Full `/reload` on the legacy architecture cannot be made deterministic from the
ACOS side without changing RN's behaviour. Options:

* **A. Fast Refresh is the dev-loop default** (done here); document that an explicit
  full reload is best-effort on the legacy arch and report `stale_roots` honestly.
  Cheapest, matches RN's own guidance. *Recommended.*
* **B. ACOS reload verb with verify+retry** (reload, observe, reload again while
  `stale_roots>0`, report `attempts`). Converges (a later clean reload drops every
  child of the shared root) but is probabilistic and noisy.
* **C. Move the debug fixture to the New Architecture.** Fabric's
  `detachRootViewFromInstance` is synchronous (`stopSurface` + `clearReactRoot`), so
  reload is deterministic; costs the `libappmodules` entry point the fixture lacks
  today, and the legacy arch is deprecated anyway.
* **D. Slow context creation in the guest** — rejected: fakes timing, hides real races.
