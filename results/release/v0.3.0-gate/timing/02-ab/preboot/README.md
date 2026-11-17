# Pre-reboot A/B attempt (NOT the record; kept for provenance)

The host rebooted at 10:36Z (16:06 local) mid-run; the driver was killed during
arm B rep 3. What completed before the reboot: reps 1-2 for both arms (latency,
cold, memory) and rep 3 arm A (v0.2.2). Arm B rep 3 latency+cold was run after
the reboot and is therefore *not* in one lattice with the rest.

Per the orchestrator's note this set is **not** used for the gate verdict: the
A/B was redone from scratch after the reboot (`../run-v022`, `../run-v030`).
