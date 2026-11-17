# SPIKE-WV — prototype: host Chromium as the ACOS WebView engine

Spike code for the ADR-0031 two-day gate. **Not release code.** Nothing here is
loaded by a product build except the two hooks listed in
`results/handoff/SPIKE-WV/REPORT.md` §6.

* `LOG.md` — every attempt, pass or fail, with the exact command and outcome.
* `broker/cdp.py` — Chrome DevTools Protocol client over
  `--remote-debugging-pipe` (fds 3/4, NUL-terminated JSON, stdlib only). No TCP
  socket, no port, no websocket: the guest never sees DevTools.
* `broker/wv_broker.py` — the host broker: one Chromium per host, one browser
  context per session, an ACOS-owned framed JSON protocol
  (`{"id","op","session",…}`) on a Unix socket, deterministic frames
  (`HeadlessExperimental.beginFrame`), accessibility tree, touch input.
* `broker/measure.py` — the gate harness (items a–e of ADR-0031).
* `broker/sigs.py`, `dexsec.py` — extract a class section / its method
  signatures from a `dexdump -d` dump of the API-34 image's `framework.jar`.
* `provider/METHODS.md` — the method surface the framework will call.
* `scenarios/webview-probe.yaml` — the probe scenario.
* `evidence/` — raw gate evidence (`gates.json`, `gate-*.json`, PNGs,
  `SUMMARY.md`) and `probe-run/` (a real `acos verify --engine art` run of the
  probe app, including the exact today-failure).

## Reproduce

```sh
WT=/path/to/worktree; cd "$WT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts

# 1. pinned engine (122 MiB, official Chrome for Testing URL)
mkdir -p .cache/spike-wv/cft && cd .cache/spike-wv/cft
curl -O https://storage.googleapis.com/chrome-for-testing-public/157.0.8103.78/linux64/chrome-headless-shell-linux64.zip
sha256sum chrome-headless-shell-linux64.zip    # f9032972…d153a
unzip -q chrome-headless-shell-linux64.zip && cd "$WT"

# 2. gate measurements (host broker, ~40 s)
ACOS_SPIKE_WV_SCRATCH=$PWD/.cache/spike-wv \
  python3 research/spike-wv/broker/wv_broker.py --selfcheck \
  --out research/spike-wv/evidence

# 3. today's guest failure (real ART session)
ACOS_APP_ONLY=webview-probe tools/build-apps.sh
tools/art-runtime/run-exclusive.sh --task SPIKE-WV -- tools/guarded.sh \
  --mem 8G --time 420 -- env ACOS_ART_BIN=$PWD/.cache/spike-wv-target/release/acos-art \
  ./.cache/spike-wv-target/release/acos verify --engine art \
  research/spike-wv/scenarios/webview-probe.yaml \
  --artifacts research/spike-wv/evidence/probe-run

# 4. broker as the guest will see it (Unix socket, no DevTools access)
ACOS_SPIKE_WV_SCRATCH=$PWD/.cache/spike-wv \
  python3 research/spike-wv/broker/wv_broker.py --serve .cache/spike-wv/wv.sock &
```

The engine path can be overridden with `ACOS_CHROME_HEADLESS_SHELL`; scratch
(profile dirs) with `ACOS_SPIKE_WV_SCRATCH`. The broker always creates a
throwaway `--user-data-dir`; it never reads a browser profile.
