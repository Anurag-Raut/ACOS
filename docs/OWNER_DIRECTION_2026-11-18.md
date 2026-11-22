# Owner direction 2026-11-18

Additions to `docs/OWNER_DIRECTION_2026-11-11.md` and `docs/ENGINEERING_POLICY.md`. Where they disagree, this file wins.

1. **Compatibility target: almost 100%.** An app developed with ACOS must look and behave like it does on real Android. "Highest practical compatibility" is not the bar; near-total compatibility is. Unsupported behaviour still answers a structured `UNSUPPORTED_*` error, never a fake.
2. **Better than the stock Android emulator on every tracked metric.** The stock emulator is a tracked baseline (boot, launch, tap-to-result latency, UI-tree read, screenshot, install, memory per instance, CPU, and verified tasks per GiB-minute at N concurrent instances). Every release gate compares against it and flags any metric where ACOS is worse. (Benchmark track: EMUBENCH; ADR-0015's one-emulator rule is relaxed for this benchmark to N=1/2/4 under the 90% RAM ceiling.)
3. **Building blocks.** Agents attach and detach the Android services they need (ADR-0029 modules: off / stub / scripted / real); unattached services answer `UNSUPPORTED_ANDROID_API` by name.
4. **Parallelism.** Many agents run sessions concurrently; the primary metric stays correctly verified tasks per GiB-minute at N=10.
5. **Graphics.** Software GL (ANGLE on SwiftShader) is the default, shared per host and loaded only when an app uses GL, so ACOS works on machines with no GPU. A real host GPU (AMD/Intel via the Waydroid method: Mesa for Android + minigbm) is an optional speed-up, never a requirement; results must not depend on the host GPU.
6. **Reuse first, smartly.** Prefer copying prebuilt Google/Waydroid/AOSP pieces, or using them as the reference, over writing new code; save both host resources and development effort.
7. **Models.** Routine implementation on DeepSeek v4.1 flash; hard root-cause work on Claude Sonnet 5.5; menial work on Claude Haiku 5.5.
