# K5C handoff — opaque AndroidKeyStore keys + AOSP refusals, cross-session green

**Status:** goal 1 PASS, goal 2 PASS, goal 3 PASS, goal 4 PARTIAL (crate tests +
gate matrix not completed: host ART lock saturated). Checkpoint commit `d1a35b7`
on `compat-k5-keystore`; docs commit on top.

**Tree:** `/home/Qui/.paseo/worktrees/3oxa6xao/k5`, branch `compat-k5-keystore`,
target dir `WT/.cache/k5-target` (release binaries from K5b reused — the change
is guest Java, compiled into the launch dex at run time).

## What changed

| file | change |
|---|---|
| `crates/acos-art/guest/java/dev/acos/art/SoftwareKeyStore.java` | `StoreSecretKey`/`StorePrivateKey` are opaque (`getEncoded()`/`getFormat()` → `null`, alias only); `KeystoreRefusal extends Error` deleted in favour of AOSP's types/messages; `GuardService.supportsParameter` → `SoftwareKey` only (AOSP `SupportedKeyClasses`); new `materialSecret`/`materialPrivate`; new `GuardSignature` SPI; `engineSetKeyEntry` rejects opaque keys |
| `fixtures/apps/keystore-probe/src/.../KeystoreProbe.java` | refusal checks assert AOSP types/codes; +8 checks (opacity, no-foreign-provider, control, stability); `create`/`fidelity` hold so the host can snapshot the live run root |
| `tools/keystore-k5b/run.sh` | xsession copies A's `app-data` out **inside the lock while A is alive** and runs B concurrently; fidelity measures the keystore file while the guest is alive; `ACOS_ART_DATA` pinned |
| `results/compat/K5.md` | new K5c section (mechanism, evidence table, follow-up) |
| `EXPERIMENTS.md` | `### K5c` under the K5 section |

## Evidence (all real ART, under `run-exclusive.sh` + `guarded.sh`)

`results/compat/K5C-evidence/`:

- `fidelity.jsonl` — 38/38 checks pass (`fidelity: 38/38 checks pass`).
  Key lines:
  `InvalidKeyException(Keystore operation failed) cause=KeyStoreException/code=-3/key "k_encrypt_only" does not have PURPOSE_DECRYPT`,
  `InvalidAlgorithmParameterException(Caller-provided IV not permitted)`,
  `KeyNotYetValidException(Key not yet valid)`, `KeyExpiredException(Key expired)`,
  `Cipher.getProvider()=AndroidKeyStoreGuard` for every refusal,
  `fidelity.refusal.control_foreign_provider_visible :: plain SecretKeySpec performed by AndroidOpenSSL`,
  `fidelity.opaque.secret :: encoded=null format=null`,
  `fidelity.opaque.public_x509 :: format=X.509`.
- `probe.jsonl` — xsession 8/8 (`k_a absent`, four paths, controls).
- `summary.tsv` — `xsession-a-live-copy 0`, `xsession-a-keystore-mode 600`,
  `xsession-a-alive-during-b yes`, `fidelity PASS`, `xsession PASS`,
  `default_off PASS`.
- `default_off.jsonl` — `java.security.KeyStoreException: AndroidKeyStore not found`,
  `providers=AndroidNSSP,AndroidOpenSSL,CertPathProvider,BC,HarmonyJSSE`.

## Reproduce

```sh
cd /home/Qui/.paseo/worktrees/3oxa6xao/k5
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/keystore-k5b/run.sh .cache/k5c/keystore-suite all   # takes the lock itself
```

## Not done (next actions, smallest first)

1. **Gate matrix** (11 gates + 5 Gaten flows + 5 negatives) on this tree — copy
   `/home/Qui/Projects/acos/.cache/wt-int/.cache/int/run_suite.sh` into
   `WT/.cache/k5c/` and repoint target/output; the change is opt-in so no gate
   is expected to move, but it is unmeasured. Both of my acquisitions timed out
   in the lock queue (`int` bench + `G3F1` loop held both slots).
2. **Crate tests** `acos-services`, `acos-art`
   (`--test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created`, then
   that test alone with a cold `ACOS_ART_DATA`), `acos-aidl`, `acos-cli`,
   `acos-verify`. `acos-apk --lib` re-ran green (18/18) after the change.
3. **Element / dot.gallery** keystore-on spot check under the opaque keys: their
   `KeyStore.getKey(...)` now returns an opaque key, so any app path that called
   `getEncoded()` on it changes behaviour (that is AOSP-correct, but the funnel
   numbers in K5.md were taken with exportable keys).
4. Follow-up (recorded, out of scope): per-request keystore mode through
   `acos serve` — the daemon fixes the guest environment at start.
