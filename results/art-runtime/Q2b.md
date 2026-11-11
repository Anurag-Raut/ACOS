# Q2b — the runtime side of the content-provider peer

**Status: done.** `acos-runtime::ams_java` no longer owns a provider record and no
longer refuses every `IContentProvider` transaction. `IActivityManager.getContentProvider`
and `IContentProvider.call`/`query` are answered by the `acos-services`
`ProviderRegistry` behind the runtime `ServiceHost` — the same `AtmsService` /
`ProviderService` the ART binder-peer route dispatches to, so the authority list
and the provider payloads exist in exactly one implementation. Everything else
`ams_java` owns is unchanged.

Nothing committed. No `acos-art`/`dalvikvm` process was run (task constraint), no
`crates/acos-art` file touched. One 7-line **API addition** to
`crates/acos-services/src/atms.rs` was unavoidable and is disclosed in §4.

## 1. What shipped

| path | change |
|---|---|
| `crates/acos-runtime/src/ams_java.rs` | `install_host`/`clear_host` (+ the `HOST` thread-local): the module keeps a handle on the runtime's `ServiceHost`. `getContentProvider` is dispatched to the host *before* the generic request decoding (`transact_get_content_provider`), so the host sees the untouched request parcel; the reply is the host's own API-34 `ContentProviderHolder`. The app-published fallback (`publishContentProviders`) reads `AmsState::providers` directly — the hardcoded `AmsState::get_content_provider` record and the `settings_provider_binder()` substitution are gone. `transact_content_provider` forwards the whole `IContentProvider` transaction to the host (`dispatch_to_host`) instead of raising `UNSUPPORTED_ANDROID_API`. |
| `crates/acos-runtime/src/lib.rs` | `Runtime::with_host` installs its host for `ams_java`. |
| `crates/acos-runtime/src/session_pool.rs` | `Runtime::with_vm` (the restore/template constructor used by `AgentSession`) installs it too — without this the session path had no host and `getContentProvider("settings")` fell through to a null holder (see §3). |
| `crates/acos-runtime/src/atms_boot.rs` | The attach recorder now wraps the host's **existing** ATMS state (`AtmsService::shared_state`) instead of a fresh `AtmsState`. A fresh state carried an empty `ProviderRegistry`, so every boot using the recorder returned a null `settings` holder. |
| `crates/acos-runtime/src/settings_java.rs` | `prepare_settings_reply` is a no-op when the VM has no thread (a unit test driving a transaction directly): `<clinit>` needs a frame stack, and no Java reader can consume the reply there. |
| `crates/acos-runtime/tests/ams_java.rs` | Two new acceptance tests (§2), 334 lines. |
| `crates/acos-services/src/atms.rs` | **+13 lines** (7 of code): `AtmsService::shared_state() -> Arc<Mutex<AtmsState>>`, so a decorator over the service keeps the process records *and* the provider registry. Disclosure in §4. |

Routing, in one sentence: a provider transaction now reaches `ServiceHost`
whether it came from `ServiceTransport` (the Rust VM) or from `acos-binder-peer`
(ART); `ams_java` only forwards.

## 2. Acceptance

| requirement | test |
|---|---|
| `getContentProvider` from the registry, not a hardcoded record | `content_provider_holder_comes_from_the_services_registry` — the reply is decoded with the real `ContentProviderHolder.CREATOR`; `ProviderInfo.authority/packageName/name/processName/readPermission` are the registry entry's (`settings`, `com.android.providers.settings`, `SettingsProvider`, `android.process.settings`); `holder.provider.asBinder().mNativeData` **equals** `host.binder_for_descriptor("android.content.IContentProvider")`; `connection` is a non-null `BinderProxy` whose id the host resolves to `com.android.server.am.ContentProviderConnection`; `noReleaseNeeded == false`, `mLocal == true`; `data_avail() == 0`; the registry recorded `getContentProvider/settings/hit=true/binder=<same id>` |
| `call(GET_config)` / `call(LIST_config)` return registry data through the `ams_java` path | `content_provider_call_and_query_return_registry_data_through_ams_java` — `GET_config` `ime/enabled_input_methods` → `Bundle.value = "com.android.inputmethod.latin/.LatinIME"`; `LIST_config` with `_prefix="ime/"` → `HashMap` with the three `ime/...` keys; both are byte-identical to the same request dispatched straight through `ServiceHost::transact` (the ART route) |
| a settings `query` returns registry data through the `ams_java` path | same test — `query(content://settings/secure, ["value"], {"name=?": ["android_id"]})` decoded with `cursor::read_query_reply` (`BulkCursorDescriptor` + `CursorWindow`): 1 row, column `value`, `"acos0000000000ff"`, frame exactly sized |
| keep existing behaviour | the 5 pre-existing `ams_java` tests still pass, including the null-holder miss (`get_content_provider_does_not_treat_the_calling_package_as_the_authority`) and the real-`Intent` round trip |

```text
tools/guarded.sh --mem 6G cargo test -p acos-runtime --test ams_java
running 7 tests
test get_content_provider_does_not_treat_the_calling_package_as_the_authority ... ok
test content_provider_call_and_query_return_registry_data_through_ams_java ... ok
test component_name_reply_is_a_real_component_name ... ok
test content_provider_holder_comes_from_the_services_registry ... ok
test get_content_provider_reads_the_authority_and_carries_the_provider ... ok
test unsupported_extra_type_is_a_structured_error ... ok
test intent_facts_round_trip_through_the_real_creators ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

tools/guarded.sh --mem 6G cargo test -p acos-services        # 87 tests, all pass
tools/guarded.sh --mem 6G cargo clippy -p acos-runtime -p acos-services --all-targets
   Finished (0 warnings)
```

Cross-checks (the change touches the whole app-boot path):

```text
cargo test -p acos-runtime --test m2                3 passed   (onCreate/onStart/onResume)
cargo test -p acos-runtime --test provider_java     2 passed
cargo test -p acos-runtime --test settings          0/1 FAILED — pre-existing, §5
cargo test -p acos-runtime --test app_matrix_interaction  4 passed, 2 FAILED — pre-existing, §5
cargo test -p acos-runtime --test boot_image_cache --test session_pool --test wms --test rn_product  all pass
```

## 3. What the boot path actually needed

`getContentProvider("settings")` is called by the framework during
`Activity.attach` (`PhoneWindow.<init>` → `Settings.Global.getInt`), i.e. *inside*
the first launch, so a null holder is not a slow-path miss — it is an NPE inside
`Settings$NameValueCache.getStringForUser` and the activity never reaches
`onCreate`. Two runtime-side seams had to install the host, and one had to stop
throwing the registry away:

1. `Runtime::with_host` (tests, the CLI) and `Runtime::with_vm` (the session
   pool / `AgentSession`, i.e. `m2` and `app_matrix_interaction`) are *two*
   constructors; both must call `ams_java::install_host`.
2. `atms_boot::install_recording_atms` replaced the `activity` service with a
   decorator over `AtmsState::new()`. That state owns the registry, so the M2
   lifecycle boot saw an empty provider set (evidenced by a registry
   `ProviderLookup { hit: false }` for `settings`) and failed at
   `Settings$NameValueCache.getStringForUser`. It now shares the host's state.

Decoding the framework's own disassembly (`.cache/boot/android-34/dex/framework`,
`Settings$NameValueCache.getStringForUser` position line 3331) pinned the NPE to
`IContentProvider.call` on a null `cp` — i.e. `ContentResolver.acquireProvider`
had returned null — rather than to the `Bundle`/`Cursor` reply.

## 4. Disclosed `acos-services` addition

`AtmsService::shared_state()` (7 lines of code) exposes the `Arc<Mutex<AtmsState>>`
the service already holds. It is not a behaviour change and nothing in
`acos-services` calls it; it exists because the runtime cannot otherwise seed a
decorator with the same registry. The alternative — re-registering the `settings`
`ProviderEntry` inside `atms_boot.rs` — would be exactly the second authority list
Q2 §4 warned about. No other `acos-services` file was touched.

## 5. Pre-existing failures in the tree (not caused by this change)

Both reproduce with my four files reverted to `HEAD` (`git checkout` +
re-run), so they are not Q2b regressions:

1. **`cargo test -p acos-runtime --test settings`** (Q2-owned, `acos-services`):
   `SettingsDefaults::api34()` seeds `("animator_duration_scale", "1.0")`
   (`crates/acos-services/src/provider.rs:245`, added in Q2's uncommitted diff),
   while `SettingsMain` parses that key as an `Integer` and
   `tests/data/settings.golden` expects it **unset** (`settings.animator_duration_scale=1`,
   the caller's default). Result: `NumberFormatException: For input string: "1.0"`.
   One-line fix for the Q2 owner: drop that seed (or set it to `"1"` if the golden
   is to change). I did not edit `acos-services` seeds per the task's ownership rule.
2. **`cargo test -p acos-runtime --test app_matrix_interaction`**:
   `dialogs_alert_opens_and_cancel_dismisses` (`InflateException` on
   `alert_dialog_button_bar_material`) and
   `list_scroll_button_moves_the_recycled_window` fail identically at `HEAD`.
   The other four (toast, spinner dropdown, popup menu, fragments nav) pass.

## 6. Limits / not done

- **App-published providers still come from `AmsState`.** `publishContentProviders`
  records the app's own `ContentProvider` binders in the VM-free AMS state, not in
  the `ProviderRegistry` (which would need a `ContentProviderConnection` token per
  published provider — an `acos-services` feature, not a runtime one). So the
  registry is authoritative for Rust-served providers and `AmsState` for the
  app's own; the two sets are disjoint by construction and the `settings`
  authority now has exactly one owner.
- `AmsState::get_content_provider` (the hardcoded `authority == "settings"`
  record in `acos-services/src/ams.rs`) is now unused by the runtime but still
  present (and still used by `AmsState`'s own `call` handler). Deleting it is a
  `acos-services` change for the Q2 owner.
- `insert`/`update`/`delete`/`bulkInsert` remain a structured
  `UNSUPPORTED_ANDROID_API` — the host answers them, no runtime hand-writing.
- No ART/`dalvikvm` evidence: the acceptance here is the Rust VM path plus the
  byte-identity with the `ServiceHost` path the peer uses.

## 7. Reproduction

```bash
tools/guarded.sh --mem 6G -- cargo test -p acos-runtime --test ams_java
tools/guarded.sh --mem 6G -- cargo test -p acos-runtime --test m2
tools/guarded.sh --mem 6G -- cargo test -p acos-services
tools/guarded.sh --mem 6G -- cargo clippy -p acos-runtime -p acos-services --all-targets
```
