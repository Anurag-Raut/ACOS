# Decision 2026-11-12: move toward a Zygote-style shared ART core (experiments first)

The owner approved the direction: one pre-initialised ART core that forks many independent,
lightweight app sessions, reusing Google's actual ART and Zygote mechanisms
(ZygoteHooks, `-Xzygote`, preload, fork/specialise) rather than re-implementing them in Rust.

Evidence behind it:
- results/art-runtime/D-M2-MEASURE.md
- D-M2-INVENTORY.md
- D-M5.md and D-M6.md
- the advisor review summarised in the owner report on 2026-11-12

## Approved now
1. A standalone Zygote prototype: init ART once, preload, fork children with Android's hooks, CoW.
2. Real memory measurement: N=1/5/10/20, shared vs private, the Zygote process included, plus
   launch speed and CPU.
3. Correctness and isolation validation: per-child heap, app data, Binder identity and UI state; GC,
   threads, repeated launches and teardown; sandbox isolation without losing CoW.
4. Real apps: counter first, then form, dialog and Gaten. Real rendering, input, screenshots and
   tree verification; no fake successes and no reduced verification.
5. A three-arm comparison:
   (a) current ART with the low-risk optimisations;
   (b) a shared boot image without Zygote;
   (c) Zygote CoW.

## Constraints
- The existing ART engine keeps working throughout.
- No unnecessary redesign of system services or C1.
- Never put independent apps inside one shared Java process.
- Reuse AOSP wherever practical.
- At most 5 agents, and tasks of 60 minutes or less.
- Untrusted APKs stay on the existing exec path until a new sandbox passes security review.
- No production migration without measured evidence and owner approval.

## Gate
Production integration is recommended only if the result is a 25% or larger whole-system
improvement over the *optimised* current implementation, with no loss of correctness,
security or compatibility. Before any final change, present the advantages, disadvantages,
risks and measured gains.

## Implied approval
Arm (a) needs the low-risk optimisations, so Phase-1 tasks 1–6 from the owner report are in
scope:
- PNG checksums;
- daemon frame retention;
- the heap facts;
- the uncompressed boot image test;
- the resume sleep;
- the stop wait.

Task 7 (fast vs strict observation) still waits on the owner's answer. Only its
ack-matching bug fix proceeds.
