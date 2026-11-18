# MERGE040F/G handoff

Final verification of merge040g (= merge040e + visible `hardware_renderer_fallback` note + ENV_LOCK in the
sandbox command tests + parityfix 7d2b9e324), run on ART with one consistent release build
(`acos`, `acos-art`, `acos-netd`, `acos-webview` built in the same tree state; a mixed clean/dirty build makes the
WebView gate refuse the broker as "built from other code").

`evidence/<scenario>.json` is the verdict envelope for each scenario (status pass for all nine):
gaten-bubble-overlay, gaten-bubble, counter, form-validation, webview-probe, gles-software, a11y-probe,
a11y-names, rot1-rotate. gaten-bubble-overlay carries the `hardware_renderer_fallback` note; counter does not.
The bulky per-scenario artifact trees (ui trees, frames, logs) were trimmed; rerun `run-batch.sh <scenario>...`.

Test env notes: `ACOS_SPIKE_NET_PASTA=.cache/pasta-pinned/pasta ACOS_NET_ARM=base` for `spike_net_boundary`;
`tools/build-fixtures.sh` (dex corpus) for `cleanup`; `.cache/webview` symlink + built `acos-webview` for webview-probe;
`ACOS_ART_DATA` seeded from `.cache/art/data`.
