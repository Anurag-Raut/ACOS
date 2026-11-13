# Advisor review, 2026-11-13 ~02:20Z (codex gpt-6.1-sol, high, analysis only)

Agent `be77ad75`, fresh, read-only. Asked by the orchestrator on three open calls.

| Question | Verdict | Orchestrator action |
|---|---|---|
| Q1 K5 compat changes | **Fix first.** App uid 10192: ship as default after gates. Software AndroidKeyStore: fix, then **opt-in**. Location/trust/restrictions: ship read paths after wire validation; mutation paths must fail explicitly. APK signing certs: fix first. | K5 continuation packet carries the full fix list (below). Not merged. |
| Q2 forever-redrawing screens / 117 ms ripple wait | Owner recommendation: **explicit navigation-only fast (GR3 option 3 / H15 option B)**, with a strict refresh or fresh capture before any verdict. Keep content-stable confirmation (option A) experimental, not default. | Presented as decision 4 in docs/MORNING-2026-11-13.md. |
| Q3 my guard 6301c06 | **Fix first:** `fast tap → back → assert` bypass (back kept ART's persistent fast mode and cleared the marker); pixel assertions were over-blocked. | Fixed in 95923bb (back/rotate force strict; assert_screenshot/assert_region take their own fresh strict capture). |

## Q1 detail (K5 fix list)

- **uid:** measure uid_map, uid/gid/groups/capabilities, PMS agreement, writable mounts, access to another session's files. Reject malformed overrides; never fall back silently to the host uid. No host-level isolation is gained between sessions; do not claim it.
- **Software keystore:** `KeyInfo` must report a software security level (it currently returns plain exportable keys). Honour or reject requested purposes, modes, padding, validity and randomized-encryption flags. Import must honour `ProtectionParameter`. Attestation requests must fail explicitly. Persistence failures must propagate, not log-and-succeed. Generated asymmetric keys need certificate chains. Key files survive in kept crash run trees (R3) and keep flags: define retention, use 0600 permissions, test reset/restart/crash/SIGKILL/concurrent sessions/identical aliases across packages. Label evidence "software-keystore compatibility", not device security.
- **Services:** setters and listener registration currently succeed without effect; unsupported operations must fail explicitly. Validate transaction numbers and reply shapes against the boot image, especially `restrictions.requestPermission`.
- **Signing certs:** returns the first signer's whole chain as `Signature[]` (signer certs and chain members differ). v1 picks the first cert without identifying the signer. Higher schemes can fall back silently. Signature-block decompression is unbounded on untrusted input: bound it first.
- **Acceptance:** final-tree 2-run 42-app corpus, all gates, crate tests, the security cases above, and RedReader's exact reply bytes decoded through the real `PackageInfo.CREATOR`.

## Q2 detail (if option A is ever tried)

Keep `probe_seq > floor`, guest settled and `draw_ms >= publish`. Compare the complete displayed composite (all surfaces, overlays, transforms, clip, size, rotation) plus all assertion-relevant tree state. Require continuous equality for ≥24 ms, resetting on any change, missing observation or pending work. Bind the final pixels and tree atomically to the validated draw/store. Use byte equality or a cryptographic digest, not FNV. Test same-pixel redraws, A→B→A, low-frequency animation, delayed callbacks, pixel-only and tree-only changes, dialogs/IME, dropped stores and stale probes. Gaten's pending retry queue keeps "guest settled" false, so a content hash alone cannot settle it.
