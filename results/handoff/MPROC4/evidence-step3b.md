# MPROC4 step 3b — the second guest process, proven on ART

Branch `mproc4` (base `mproc3` @ `edc84490`). One run, no A/B.

## What the run did

`python3 tools/mproc4-two-process-drive.py <acos-art> proc-a.apk proc-b.apk out`

`proc-a` (`dev.acos.fixtures.proca`) is launched with `--extra-apk proc-b.apk`
(`dev.acos.fixtures.procb` installed beside it, uid 10193). The driver reads the
session's ready line, then taps **Start B** = `startActivityForResult` to
`dev.acos.fixtures.procb/.ProcBActivity`.

Run under the shared lock:

```
tools/art-runtime/run-exclusive.sh --task MPROC4 -- \
  tools/guarded.sh --mem 10G --time 500 -- \
  env ACOS_FONT_DIR=…/android-34/fonts ACOS_ART_DATA=$WT/.cache/mproc4-art-data \
  python3 tools/mproc4-two-process-drive.py .cache/mproc4-target/debug/acos-art \
    fixtures/build/apps/proc-a.apk fixtures/build/apps/proc-b.apk \
    results/handoff/MPROC4/evidence/run3
```

## Driver output (verbatim, `evidence/run3`)

```
PASS ready: {"package": "dev.acos.fixtures.proca"}
PASS no-second-process-before: {"processes": [], "requests": []}
PASS tap-start-b: {"buffer_generation": 54, "cmd": "tap", "frame_seq": 2, …}
PASS start-b-button-in-tree: {"tree_present": true}
PASS second-process-spawned: {"processes": [{"activity": "dev.acos.fixtures.procb.ProcBActivity",
     "alive": true, "package": "dev.acos.fixtures.procb", "pid": 2108577, "tag": 1}],
     "requests": [{"activity": "dev.acos.fixtures.procb.ProcBActivity",
     "component": "dev.acos.fixtures.procb/dev.acos.fixtures.procb.ProcBActivity",
     "package": "dev.acos.fixtures.procb", "pid": 2108577, "reason": "startActivity",
     "since_ms": …}]}
PASS second-process-is-proc-b: [{"activity": "dev.acos.fixtures.procb.ProcBActivity",
     "alive": true, "package": "dev.acos.fixtures.procb", "pid": 2108577, "tag": 1}]
PASS proc-b-resumed-in-its-own-process: {"lines": [
     "ACOSPROBE b created pkg=dev.acos.fixtures.procb note=none pid=2…",
     "ACOSPROBE b resumed pkg=dev.acos.fixtures.procb"]}
PASS process-table-still-has-b: {"processes": [{"tag": 1, "pid": 2108577, …}]}
FAILURES_3B []
```

The step-3c assertions in the same run (window on top, pause/stop delivery,
`onActivityResult`, the service round trip) are reported separately as
`OPEN_3C` — see `STATUS.md` §"what 3c still needs". None of them is counted as a
pass.

## Launcher evidence (`evidence/step3b-peer-lines.txt`, full log next to it)

```
acos-art: launch: extra package installed dev.acos.fixtures.procb uid=10193
    apk=/data/app/proc-b.apk native=/data/app-lib-extra/dev.acos.fixtures.procb/x86_64
acos-art: launch: started secondary process tag=1 package=dev.acos.fixtures.procb
    activity=dev.acos.fixtures.procb.ProcBActivity pid=2108577
acos-binder-peer: process tag 1 connected (dev.acos.fixtures.procb)
acos-binder-peer: process 1: launch bindApplication request stashed
acos-binder-peer: process 1: delivering bindApplication (1304 bytes, code 5)
…
acos-art: peer: processes=[] peak_connections=2 accepted=2 refused=0 closed=2
    frames=549/358 unroutable=0
acos-binder-peer: process 1 connection closed (0 java binders, 17 host binder records dropped)
```

`peak_connections=2 accepted=2` is two tagged driver connections served by one
peer/`ServiceHost`; `unroutable=0` is every delivery finding its process. The
guest's own log lines (`ACOSPROBE b created/resumed`, `pid=…`) come from the
second process, and the pid in them is not the launcher's.
