# MERGE040J (evidence dir kept as MERGE040I per the brief)

Branch `merge040j` = `merge040i` (963c6bd09) + `gatenwordc` (6540abbdc).

- Conflict: `launch.rs` only (both sides added the same `final_logd_tests` module) -> one copy kept.
- Layer handles: BOTH kept — metro2c recycling (FIFO + tombstone) and gatenwordc `LAYER_HANDLE_POOL = 128`.
  Tests: `destroyed_layers_return_their_handles_so_a_long_session_never_exhausts_the_pool`,
  `a_session_can_create_many_more_layers_than_sixteen`, new
  `a_session_can_create_and_destroy_over_a_thousand_windows` (1200 windows + 4 long-lived layers).
- Compose tree fix (`LaunchMain.enumerateProviderTree`) and scenario `gaten-word-lookup` kept.
- `cargo test -p acos-services` 737 passed; `-p acos-art -p acos-cli` 569 passed (needs fixtures/build dexprogs and
  `ACOS_NET_PASTA=.cache/pasta-pinned/pasta`, sha256 314c37e8…, for spike_net_boundary).
- ART scenarios, all PASS (verdict.json per dir; `run-arm.sh` = driver): gaten-word-lookup, gaten-bubble-overlay,
  gaten-bubble, gaten-backstack, gaten-lesson, counter, form-validation, webview-probe, a11y-names (a11y-names-probe),
  a11y-probe, rot1-rotate, sysui1-shade, back-layers, w15-rn-hello (the RN fixture).
  webview-probe needs `acos-webview` built with the same build stamp and its pinned Chromium cached (first connect downloads it).
- Tree-dump diff (counter, form-validation; `before-*` = LaunchMain.java of merge040i, same engine otherwise):
  after normalising window ids and timestamps the trees are identical, except one form-validation tree's `content_hash`
  (12.json), which flips between runs of the SAME binary (rerun-a/b hold both values) — run noise, not the change.
