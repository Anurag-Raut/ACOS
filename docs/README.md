# ACOS Documentation

`docs/` is the **single source of truth** for ACOS's current architecture, agent contracts, and maintainer guides. Historical pre-ART and dated planning documents live separately in [`../legacy/docs/`](../legacy/docs/) and should not be used for current development.

## Architecture & Internals (`docs/internals/`)

Start here to understand how ACOS works, what is shared on the Host vs private per Session, and which architectural decisions govern the codebase:

- **[Architecture Overview](internals/overview.md)** — AOSP vs ACOS Binder cut, Host vs Session vs Session Controller ownership, measured RAM & N=20 scaling, Host-shared services concurrency (`acos-netd`, `acos-webview`, read-only page pool, software GLES, Host Zygote), attachable modules, and settle/verification rules.
- **[Glossary](internals/glossary.md)** — Canonical vocabulary (**Host**, **Session**, **Session Controller**, **Guest App Process**, **Module**, **Profile**, **Settle**, **Verdict**, **Fidelity**).
- **[Active Architectural Decisions](internals/decisions.md)** — Curated summary of active ADRs by subsystem (links to the full log in [`../DECISIONS.md`](../DECISIONS.md)).
- **[AOSP vs ACOS Resource Diagram (Excalidraw)](internals/diagrams/aosp-vs-acos-resources.excalidraw)** — Editable visual breakdown of Host-shared vs Session-private components and RAM budgets.

## Agent & User Contracts

- **[Agent Guide](AGENT_GUIDE.md)** — Authoritative guide for AI agents driving ACOS (embedded into `acos-mcp` and served via `acos_help`).
- **[Agent Quickstart](AGENT_QUICKSTART.md)** — Practical launch → observe → act → verify walkthrough.
- **[Agent API Contract](AGENT_API.md)** — Full V1 JSON/MCP protocol specification, envelopes, verdicts, and host ceilings.
- **[MCP Tool Reference](MCP.md)** — Generated closed-schema MCP tool table (`acos_*`).
- **[Service Modules](MODULES.md)** — Generated registry of `core` and all 23 attachable modules, profiles, budgets, and control operations.
- **[Error Codes](ERROR_CODES.md)** — Canonical `ErrorCode` taxonomy and exit statuses.
- **[Installation](INSTALL.md)** — Installing ACOS, `ACOS_HOME`, boot image extraction, `pasta`, and `chrome-headless-shell`.
- **[Live View](LIVE_VIEW.md)** — Protocol v1 stream and `acos view` browser hub.
- **[UI-Tree Parity](PARITY.md)** — Running ACOS vs physical phone scenario parity checks.

## Security & Confinement

- **[Security & Env Knobs](SECURITY.md)** — Confinement invariants (Landlock, seccomp, namespaces, rlimits) and what `ACOS_*` env vars can and cannot change.
- **[Sandbox Certification](SANDBOX_CERTIFICATION.md)** — Certification gates, provenance labels, and untrusted APK policy.

## Maintainer & Contributor Reference

- **[Development Workflow](DEV_WORKFLOW.md)** — Building, running, testing, and using the shared `sccache` compiler cache (§14).
- **[Engineering Policy](ENGINEERING_POLICY.md)** — AOSP-first reuse ladder and concurrency/memory efficiency rules.
- **[AOSP Source Reference](AOSP_REFERENCE.md)** — Local sparse AOSP `android-14.0.0_r75` checkout for checking wire formats before guessing.
- **[Image ABI Notebook](IMAGE_ABI_NOTEBOOK.md)** — Verified API-34 binary, Binder, Parcel, and `BLASTBufferQueue` ABI facts.
- **[Window Layers](WINDOW_LAYERS.md)** — Generated AOSP window z-order table used by the C1 compositor.
- **[Findings Log](FINDINGS.md)** — Append-only record of verified low-level findings and intentional AOSP deviations.
- **[React Native Metro Plan](METRO_PLAN.md)** — v0.5.0 RN dev-loop & Fast Refresh architecture.

## Root Logs & Ledgers

A small set of root files remain at repo root because tooling or tests reference them directly:

- [`../AGENTS.md`](../AGENTS.md) — Agent entrypoint, crate map, and documentation rules.
- [`../DECISIONS.md`](../DECISIONS.md) — Full append-only ADR log (ADR-0001 through ADR-0032+).
- [`../BENCHMARKS.md`](../BENCHMARKS.md) — Benchmark methodology, metric definitions, and tracked baselines.
- [`../EXPERIMENTS.md`](../EXPERIMENTS.md) — Experiment log.
- [`../COMPATIBILITY.md`](../COMPATIBILITY.md) — Compatibility ledger (read by `acos-mcp`).
- [`../STATUS.md`](../STATUS.md) — Historical milestone status log (checked by `tools/verify/status-check.sh`).
