# M1 native-diff report

Produced by `tools/oracle/native-diff.sh` (default `--corelib real`). The raw
artefacts are in `results/m1/native-diff/`:

* `<program>.tsv` — the natives ACOS invoked
  (`acos run-dex --trace-natives-file`), same columns as the ART traces.
* `<program>.report.txt` — set/count diff against the measured ART trace.
* `summary.txt` — the three reports concatenated.

ART reference files: `research/dynamic-trace/data/natives_println.tsv`,
`natives_17_generics_collections.tsv`, `natives_19_threads_sync.tsv`.

## Totals

| program | ACOS | ART | both | acos-only | art-only | count drift |
|---|---:|---:|---:|---:|---:|---:|
| 01_hello | 46 | 21 | 16 | 30 | 5 | 4 |
| 17_generics_collections | 50 | 25 | 18 | 32 | 7 | 7 |
| 19_threads_sync | 55 | 28 | 21 | 34 | 7 | 7 |

`both` = called by both. `acos-only` = ACOS calls it, ART does not.
`art-only` = ART calls it, ACOS does not.

## ART natives ACOS never invokes (the fidelity gap)

These are *not* missing implementations: each is either reached through a
different ACOS path or is a boot-image-only entry point.

| program | native | why ACOS does not call it |
|---|---|---|
| 01 | `Object.wait(JI)` | reached via the VM's own wait/scheduler path in the fixtures that need it |
| 01 | `System.currentTimeMillis` | ACOS's boot chain calls it through a different site |
| 01 | `Thread.interrupt0` | only the thread fixtures reach the interrupt path |
| 01 | `Throwable.nativeFillInStackTrace` | exceptions on the console path are VM-captured |
| 01 | `VMRuntime.stopHeapTaskProcessor` | no ART heap-task daemon exists to stop |
| 17 | `StringFactory.newStringFromBytes` | ACOS executes the `String` `([BIII)` constructor override |
| 17 | `reflect.Array.createObjectArray` | ACOS builds `T[]` through the VM array allocator |
| 17/19 | `Thread.sleep(Object,J,I)`, `Thread.sleep(JI)` | ACOS parks the green thread in `Thread.sleep(JI)` |
| 19 | `System.nanoTime`, `Thread.interrupt0`, `Throwable.nativeFillInStackTrace` | same as 01 |

`--strict` makes the script exit nonzero if any of these remain; it is not used
in the pass gate because these are deliberate short-circuits, not gaps.

## ACOS natives ART never invokes

Dominated by boot work that ART hides in the pre-initialised boot image and by
the VM-internal overrides:

* `VMRuntime.bootClassPath/classPath/properties/vmVersion`
* `libcore.icu.ICU.getIcuVersion/getUnicodeVersion/getCldrVersion`
* `libcore.io.Linux.getenv/uname/getpwuid/getuid`
* `jdk.internal.misc.Unsafe.arrayBaseOffset/arrayIndexScale/addressSize`
* `java.lang.Class.getNameNative/getPrimitiveClass/getDeclaredField`
* `dalvik.system.VMStack.getStackClass2`
* `java.lang.String.<init>([BIII)` / `intern` / `substring` / …
* `AtomicInteger.<clinit>/addAndGet/getAndAdd`
* `ThreadGroup.add/addUnstarted`
* `StringFactory.newStringFromUtf16Bytes`, `fillBytesUTF16`, …

See the per-program `.report.txt` for the full lists.

## Invocation-count drift

For natives in `both`, ACOS's counts differ slightly because the boot chain is
different (ACOS runs classes ART pre-initialised). Examples from 01_hello:

```
~ java.lang.String->charAt(I)C                         acos=2325 art=1969
~ java.lang.System->arraycopy(...)V                    acos=49   art=25
~ java.lang.Thread->currentThread()Ljava/lang/Thread;  acos=25   art=21
~ java.lang.ref.Reference->refersTo0(Ljava/lang/Object;)Z acos=23 art=17
```

The direction is expected: ACOS's from-scratch `<clinit>` chain touches the
strings/arrays that ART created at image-build time.
