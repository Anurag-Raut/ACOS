# HEAD gate: 70bc624

Clean detached worktree of committed HEAD (`git worktree add --detach`). No source edits, no commits, no stash.
Build env: `CARGO_TARGET_DIR=.cache/target-verify CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2`, all runs via `tools/guarded.sh --mem 6G --time 3000`.

Setup note: the worktree does not contain gitignored `fixtures/build/` (642 MB). The first pass ran without it, so the dexprogs runs and most acos-verify tests failed with "fixtures absent". Results below are from after copying the main checkout's `fixtures/build/` into the worktree.

| Command | Result | Notes |
|---|---|---|
| `cargo check --workspace --all-targets` | PASS | exit 0 |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | PASS | exit 0 |
| `tools/diff-dexprogs.sh --corelib real` | PASS | 27/27 pass, 0 fail, 0 unsupported |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | PASS | 27/27 pass (GC stress) |
| `cargo test -p acos-runtime -p acos-verify -p acos-sandbox -p acos-render --no-fail-fast` | FAIL | 283 passed, 11 failed (exit 101) |

Failing tests (11):

acos-render (8):
- `text::freetype::tests::variable_weight_masks_differ_and_are_monotonic`: wght 200 coverage equals previous (55986), variable weight not applied (pre-existing per brief).
- `text::freetype::tests::variable_instance_cache_keys_do_not_collide`: variable instances produce identical cache keys (pre-existing per brief).
- `fixture_scenes_match_art_reference` (corpus.rs): hello-views MAE 1.2549 > 1.0.
- `hello_views_text_metrics_vs_art`: title width delta 21.4%.
- `hello_views_scene_diff_vs_art_reference`: mean SSIM 0.9807 < 0.99.
- `text_coverage_matches_device_reference`: per-glyph coverage error 0.196 > 0.02.
- `text_metrics_parity_vs_art`: Hello ACOS advance delta 0.206.
- `variable_weight_renders_strictly_heavier`: wght 400 ink equals wght 100 ink (1139); variable axes not applied.

acos-runtime (3):
- `rn_platform_launch_probe`: APK_IO "Permission denied" extracting native libs under the confined session dir `.cache/sessions/.../lib/x86_64`. Cause not determined; looks like an environment or confinement issue, not checked further.
- `rn_product_launches_observes_and_taps`: UNSUPPORTED_RN_DRIVER. `android/util/Log.i(String,String)I` is not implemented; `acos-vm/src/natives/os_log.rs` registers only `println_native`, `isLoggable`, `logger_entry_max_payload_native`.
- `rn_bridge_runs_on_a_real_runtime_session_and_taps_the_button`: same missing `Log.i` native; exit 70 with 0 UIManager mount ops.

Not failing in this run: acos-verify (all scenarios pass with fixtures present) and acos-sandbox. The brief's other named tests were not in this run: `inflate_layout` lives in `acos-vm`, and `native_launch::wrong_abi` in `acos-cli`, both outside the selected crates, so they were not executed. `gfx_natives` was not found as a test name in the selected crates.

Not verified: whether the render and rn failures are regressions introduced at HEAD or were already present earlier. The brief lists `text::freetype` as pre-existing; the others were not compared against an older commit.
