# ACOS fixtures

This directory holds the executable fixture corpus used to build and
differentially verify the ACOS Rust DEX/APK tooling.

Everything here is synthetic and authored for ACOS (Apache-2.0); no
third-party APKs are redistributed. The legacy APKs under
`/home/Qui/Work/android-container/helper/build/` are referenced
read-only by env-gated tests and are never copied into the repo.

## Layout

| Path | Committed | Purpose |
|---|---|---|
| `dexprogs/<name>/Main.java` | yes | Graded, pure-Java console programs compiled to DEX. Differential corpus for the Rust DEX interpreter. |
| `expected/dexprogs/<name>.stdout` | yes | Reference stdout captured from the host JVM. |
| `expected/dexprogs/<name>.exit` | yes | Reference process exit code. |
| `expected/dexprogs/<name>.stderr` | yes (only when non-empty) | First stderr line for expected-failure programs (JVM-specific). |
| `apps/hello-views/` | yes | Source for a gradle-free View-based APK. |
| `apps/hello-jni/` | yes | Source + JNI C for a gradle-free NDK APK. |
| `build/` | no (gitignored) | All generated classes, DEX, APKs, keystores. |

## Fidelity labels

Expected outputs in `expected/` are labelled **`jvm-reference`**:
they were produced by running the compiled classes on the **host OpenJDK 17**
(`javac --release 11`). They are **NOT** Android/ART-verified.

Consequences to keep in mind when a Rust DEX runtime is compared against them:

- `System.out.println` formatting of `double`/`float` follows the
  JVM's shortest round-trip algorithm, which is also specified for Android.
- JVM-specific `Throwable.getMessage()` strings on VM-thrown exceptions
  (NPE help messages, `Index n out of bounds for length m`, ...) are
  deliberately **not** asserted: the fixtures print only
  `getClass().getName()` for those. Self-thrown exceptions do print
  their message.
- Program `20_uncaught` exits nonzero; its stderr first line is recorded
  in `expected/dexprogs/20_uncaught.stderr` and marked JVM-specific.
- Host `hashCode()` values for `String` are stable (specified), so
  `09_strings` asserts them.
- `10_objects_fields` asserts `Object.toString()`'s class prefix only,
  never its identity hash.

Regenerate everything with:

    tools/build-fixtures.sh
    tools/build-apps.sh

Both scripts are idempotent and never touch the read-only legacy tree.
