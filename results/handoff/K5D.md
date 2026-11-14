# K5D handoff — two K5 review findings fixed; keystore suite green on the fixed tree

**Status:** F1 PASS, F2 PASS, F4 PASS, all three acceptance gates PASS.
Branch `compat-k5-keystore` (base `3215be3`).

**Tree:** `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`, target
`WT/.cache/s3ab-target` (release `acos`/`acos-art` rebuilt).

## F1 — host memory DoS via uncapped v1 signature blocks (`acos-apk`)

`Apk::from_bytes` read every `META-INF/*.RSA|DSA|EC` block that paired with a
`.SF`, each capped at 1 MiB (ratio ≤ 200) but with **no cap on how many**: a
~50 MB APK with ~10k pairs made the host allocate ~10 GB.

`crates/acos-apk/src/signing.rs`:

- `MAX_V1_SIGNATURE_BLOCKS = 16` — candidate blocks per APK;
- `MAX_TOTAL_V1_SIGNATURE_BYTES = 4 << 20` — summed, declared *and* actual.

`crates/acos-apk/src/apk.rs`: candidates are filtered/pair-checked first, then
the count and the **sum of declared sizes** are checked *before any read*. The
bytes actually read are summed too, and reading stops the moment the total cap
is passed. A violation sets a structured `APK_SIGNING_MALFORMED` in
`signing_certificates_error`, clears the blocks and skips
`signing::certificates()`, so no partial signer set can be parsed (a subset
could change which signer wins). Per-block errors keep their previous tolerant
behaviour — only the caps are hard.

Tests (in `apk.rs::signature_cap_tests`, stored/uncompressed zips so the ratio
cap cannot mask the cap under test):

- `too_many_v1_signature_blocks_is_rejected` — 17 pairs → error, no certs;
- `total_v1_signature_bytes_over_cap_is_rejected` — 5 × 900 KiB (> 4 MiB, each
  block under 1 MiB and the count under 16) → total-cap error, no certs;
- `one_normal_v1_block_still_yields_its_signer` — a real `.SF` + `.RSA` pair
  from `tests/data/hello-jni.apk` re-zipped alone: no error, exactly one signer,
  byte-identical to the direct uncapped `signing::certificates(&[], &[rsa])`.

`cargo test --release --offline -p acos-apk -j 8` → **21/21 lib** (18 + 3) and
every integration suite green (`differential_against_aapt2` 1, `fixture_apk` 5,
`fuzz_mutation` 2, `native_extract` 5, `w31_manifest_flags` 4, doctest 1).

## F2 — fail-open reflection in `SoftwareKeyStore` (`acos-art` guest Java)

`specMethod()` / `call()` returned `null` on any reflection failure, which
silently dropped the validity-window, attestation, purposes and
randomized-encryption checks.

Now: a **lookup/access** failure throws
`ProviderException("ACOS keystore: cannot read KeyGenParameterSpec.<name>: …")`
(cause attached), and a non-`IllegalStateException` invoke failure does the
same (InvocationTargetException unwrapped). `IllegalStateException` is kept at
its previous meaning (`null` → caller's default) because AOSP's getters throw it
for *not specified* (`getDigests`, `getBlockModes`, `getSignaturePaddings`,
`getEncryptionPaddings`).

**The first live run of the strict version failed fidelity, correctly**:
`getDigests` → `IllegalStateException("Digests not specified")` surfaced as a
`ProviderException` and the fidelity harness aborted
(`fidelity.harness ok:false … cannot read KeyGenParameterSpec.getDigests`).
That is the getter working, not a reflection failure, hence the narrowing
above. Behaviour when reflection works is unchanged; a genuine reflection
failure can no longer silently relax a key.

## F4 — threat-model line in `results/compat/K5.md`

New K5c §6: the software keystore keeps key material in the app's own
per-session data dir (`app-data/<pkg>/files/acos-keystore/entries.properties`,
`0600`), so the app process can read it — inherent to a software keystore, which
is why it is opt-in only (`ACOS_ART_KEYSTORE=on`); the default keeps the honest
`AndroidKeyStore not found`.

## Evidence (real ART, `run-exclusive.sh --task K5D` + `guarded.sh`)

`results/compat/K5D-evidence/` (`run.log`, `fidelity.jsonl`, `probe.jsonl`,
`default_off.jsonl`, `summary.tsv`, `progress.log`):

- `[k5b] fidelity: 38/38 checks pass`, `RESULT (fidelity): PASS`
- `xsession` `PASS`: `xsession-a-live-copy 0`, `xsession-a-keystore-mode 600`,
  `xsession-a-alive-during-b yes`, 8/8 probe checks
- `default_off` `PASS` 2/2: `java.security.KeyStoreException: AndroidKeyStore
  not found`, `providers=AndroidNSSP,AndroidOpenSSL,CertPathProvider,BC,HarmonyJSSE`
- `[k5b] RESULT: PASS`
- `cargo build --release --offline -p acos-art -p acos-cli` → `Finished` (the
  guest Java itself is compiled by the probe harness/launcher, so the suite run
  is the compile proof too).

## Reproduce

```sh
cd /home/Qui/.paseo/worktrees/3oxa6xao/s3ab
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export CARGO_TARGET_DIR=$PWD/.cache/s3ab-target
tools/guarded.sh --mem 12G --time 900 -- cargo test --release --offline -p acos-apk -j 8
tools/guarded.sh --mem 12G --time 900 -- cargo build --release --offline -p acos-art -p acos-cli
bash .cache/k5d/run.sh .cache/k5d/keystore-suite all   # takes the lock per phase
```

`tools/keystore-k5b/run.sh` hardcodes the K5 worktree and re-invokes itself
through it, so it cannot drive this tree as-is. `.cache/k5d/run.sh` is a
byte-for-byte copy of it with **only** these lines changed: `WT`, `OUT`,
`ACOS`/`ACOS_ART` (target dir), `PROBE_DEX`, `ACOS_ART_DATA`, the lock script
(main checkout's absolute path, `--task K5D`) and the self re-invocation
(`bash "$SELF_SCRIPT"`). `diff tools/keystore-k5b/run.sh .cache/k5d/run.sh`
shows exactly that; no check was weakened.

## Not done / follow-ups

1. Make `tools/keystore-k5b/run.sh` relocatable (`WT` from `BASH_SOURCE`, lock
   from the main checkout) instead of needing a scratch copy — smallest next
   action, pure harness.
2. The gate matrix and the other crate suites were not re-run here (guest-Java +
   ingest change, provider opt-in); K5c's list still stands.
3. First suite attempt in this worktree (`ACOS_ART_DATA` under `.cache/k5d`)
   failed fidelity because of the F2 regression above, not the harness; the
   failing artifacts were overwritten by the green run but the finding is
   recorded in `EXPERIMENTS.md` (K5d).
