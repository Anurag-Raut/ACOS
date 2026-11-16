# MOD0 — what remains (ADR-0029 item 4, the session API)

Item 4 is the only part of the MOD0 brief this branch does not implement. It is a
plumbing task across five layers, and the framework it needs is now in place.

## The smallest next action, in order

1. **`acos-services`**: `ModuleRegistry` is already the runtime object. Add
   `SessionModules` = the registry plus `attach_session`/`attach_live`/
   `detach_live` (both exist) and `verdict_modules()` (serialise `reports()` and
   the attached set; `ModuleReport::to_json` and `status_json` exist). The
   verdict row must carry `fidelity: "stub"|"scripted"` notes from
   `ModuleReport::reason`.
2. **CLI (`acos-cli`)**: `LaunchArgs` gains `--service id[:profile]`
   (`ModuleSpec::parse` already validates the spelling), the engine passes the
   specs to the launch path instead of `registry::install_with_handles` (use
   `module::install_standard` + `attach_session`), and the launch reply gains
   `"modules": [...]` from the registry.
3. **MCP (`acos-mcp`)**: `acos_launch {services: [...]}` schema + forwarding to
   the daemon (the args are already a JSON object that is forwarded verbatim);
   a new `acos_services {op: list|status|attach|detach}` tool. `attach`/`detach`
   must call `attach_live`/`detach_live`, which already refuse `at-launch`
   modules with the launch call in the message.
4. **Scenario YAML**: the `services:` block (same shape as the launch arg), passed
   through `verify` to the same launch path.
5. **Verdicts**: `verdict.json` gains `modules: [{id, profile, fidelity}]`; an
   assertion that needs an `off` module becomes `unsupported`, never `pass`, and
   a pass that relied on `stub`/`scripted` says so.

## Also outstanding inside the finished items

* **Density (N=20) and RAM** for the kit's budget row: needs ART sessions; the
  brief says the bench owner runs timed/memory runs. `core`'s 16 MiB / 250 ms is
  an estimate, not a measurement.
* **Per-name AOSP re-decision** for the names whose client source is not in
  `.cache/aosp` (wifi, phone, bluetooth, nfc, media_session, media_router,
  search, role, carrier_config, webviewupdate). Adding those classes to the
  cache and re-running the table's rule is a data edit in `TOMBSTONES`.
* **`reference` for the tombstone binder**: today `checkService` answers a binder
  for tombstoned names, so `ServiceManager.getService` never returns `null` for
  them. Refs are not counted; if a module later needs `refCount` semantics, the
  tombstone node answers `linkToDeath` as a no-op.
