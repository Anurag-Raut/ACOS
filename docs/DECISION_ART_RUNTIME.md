# Decision 2026-11-11: real ART as the execution engine (architecture 2)

User decision: use real ART with lightweight Rust infrastructure instead of extending the Rust DEX VM.
Evidence: results/art-feasibility/E1A.md (ART runs 27/27, 10-100x faster, ~16-21 MB/proc),
E1B.md (libart in our loader: possible but 34-63 agent-days, rejected for now),
E1C.md (Rust JNI + parcel services + fork sharing work), E3-M9-peak-pss.md (Rust VM ~525 MiB/session).

Rules:
- Route: bwrap/sandbox process around dalvikvm64/app_process from the API-34 image (route A). Not libart-in-our-loader.
- Rust VM crates stay in the tree unchanged (no deletion, no migration of working code) until ART reaches V1 parity; they remain the fallback and deterministic reference.
- Rust owns: services (ServiceHost, parcel codec), binder substitute, window manager, renderer, session management, verify tooling.
- Give up (accepted): whole-heap snapshots, virtual-time determinism on the ART path. Mitigate with fresh/forked processes.
- Milestones: M-A framework Java classes run on ART; M-B counter.apk first frame; M-C tap + UI tree; M-D V1 verify items + M1-M11 vs ACOS-VM.
- Paused JNI-fidelity work (W188/W197/W199) stays paused.

Update 2026-11-11: no time-based abandonment of ART; see ENGINEERING_POLICY.md fallback policy.
