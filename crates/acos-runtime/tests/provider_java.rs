//! W74-A: app-published `ContentProvider` end to end.
//!
//! The real `ActivityThread.installContentProviders` runs during
//! `bindApplication` (the `ProviderInfoList` argument carries the APK's
//! provider), instantiates the app's `ContentProvider`, and reports it through
//! `IActivityManager.publishContentProviders`. The AMS decodes that real
//! `List<ContentProviderHolder>` and records the authority.
//!
//! The test then calls the app provider through a real
//! `android.content.ContentProviderProxy` — i.e. it writes the AIDL request with
//! the framework's own proxy, the ACOS transport dispatches it to the app's
//! local `ContentProvider.Transport.execTransact`, and the framework's own
//! `ContentProviderNative.onTransact` parses the request and marshals the reply
//! (`Cursor` -> `BulkCursorDescriptor`). No CRUD payload is hand-written.
//!
//! Skips loudly when the fixture APK or boot-classpath cache is absent.

use std::path::{Path, PathBuf};

use acos_apk::Apk;
use acos_runtime::{ams_java, apk_loader, bind_app, Runtime};
use acos_services::pms::{AppInfo, FIXTURE_UID};
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::{CoreLib, VmError, VmOptions};

const AUTHORITY: &str = "dev.acos.fixtures.svcprovider";
const PROVIDER_CLASS: &str = "dev.acos.fixtures.svcprovider.EchoProvider";
const URI: &str = "content://dev.acos.fixtures.svcprovider/echo";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn service_provider_apk() -> Option<PathBuf> {
    for rel in [
        "fixtures/build/apps/service-provider.apk",
        "fixtures/apps/service-provider/service-provider.apk",
    ] {
        let path = repo_root().join(rel);
        if acos_test_support::have_fixture(rel) {
            return Some(path);
        }
    }
    None
}

fn new_runtime() -> Option<Runtime> {
    // `Runtime::standard_with` reports a missing cache as `BootClasspathMissing`;
    // gate that the same way so `ACOS_REQUIRE_FIXTURES` fails instead of skipping.
    let _ = acos_test_support::have_fixture(".cache/boot/android-34/BOOTCLASSPATH");
    match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(rt) => Some(rt),
        Err(VmError::BootClasspathMissing(msg)) => {
            eprintln!("SKIP provider test: {msg}");
            None
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    }
}

fn obj(v: JavaValue) -> Handle {
    match v {
        JavaValue::Object(h) if h != NULL => h,
        other => panic!("expected object, got {}", other.kind()),
    }
}

fn string(vm: &mut acos_vm::Vm, s: &str) -> Handle {
    vm.new_string_rooted(s).expect("string")
}

#[test]
fn app_provider_is_installed_published_and_reachable_through_the_binder() {
    let Some(apk_path) = acos_test_support::require_some(service_provider_apk(), "app_provider_is_installed_published_and_reachable_through_the_binder", "fixture APK not built") else {
        return;
    };
    let Some(mut rt) = new_runtime() else {
        return;
    };

    // --- boot + bind (real ActivityThread.handleBindApplication) ----------
    rt.host()
        .borrow_mut()
        .install_package_from_apk(&apk_path)
        .expect("point PMS at the fixture APK");
    let boot = rt.boot_app_process(&apk_path).expect("boot app process");
    let thread = boot.activity_thread;

    let manifest = Apk::open(&apk_path)
        .expect("open APK")
        .manifest()
        .expect("manifest");
    let mut app = AppInfo::from_manifest(&manifest, &apk_path, FIXTURE_UID);
    apk_loader::stage(rt.vm(), &apk_path, &mut app).expect("stage APK");
    let providers: Vec<bind_app::ProviderSpec> = manifest
        .providers
        .iter()
        .flat_map(|c| {
            let shared = c.permission.clone().unwrap_or_default();
            let class = acos_apk::manifest::fq_name(&manifest.package, &c.name);
            let exported = c.exported.unwrap_or(false);
            c.authorities
                .iter()
                .map(move |authority| bind_app::ProviderSpec {
                    class: class.clone(),
                    authority: authority.clone(),
                    read_permission: shared.clone(),
                    write_permission: shared.clone(),
                    exported,
                    init_order: c.init_order.unwrap_or(0),
                })
        })
        .collect();
    assert_eq!(
        providers.len(),
        1,
        "fixture declares exactly its EchoProvider"
    );
    assert_eq!(providers[0].class, PROVIDER_CLASS);
    assert_eq!(providers[0].authority, AUTHORITY);

    rt.vm_mut()
        .push_static_call("android.os.Looper", "loop", "()V", &[])
        .expect("Looper.loop");
    bind_app::deliver_bind_application(rt.vm_mut(), thread, &app, &providers)
        .expect("bindApplication");
    acos_vm::interp::run(rt.vm_mut()).expect("run bind phase");
    assert_eq!(
        rt.vm().pending_exception,
        NULL,
        "bindApplication threw; provider installation did not complete"
    );

    // --- publishContentProviders decoded the real holders -----------------
    let record = ams_java::with_state(|state| state.get_content_provider(AUTHORITY))
        .expect("publishContentProviders recorded the fixture authority");
    assert_eq!(record.class, PROVIDER_CLASS);
    assert_eq!(record.package, manifest.package);
    assert_ne!(
        record.binder, 0,
        "published holder carries the provider binder"
    );

    // --- call the provider through the framework's own proxy --------------
    let vm = rt.vm_mut();
    let remote = vm
        .new_instance_ctor(
            "android.os.BinderProxy",
            "(J)V",
            &[JavaValue::Long(record.binder as i64)],
        )
        .expect("BinderProxy");
    let proxy = vm
        .new_instance_ctor(
            "android.content.ContentProviderProxy",
            "(Landroid/os/IBinder;)V",
            &[JavaValue::Object(remote)],
        )
        .expect("ContentProviderProxy");
    let package_name = string(vm, &manifest.package);
    let attribution = vm
        .new_instance_ctor(
            "android.content.AttributionSource",
            "(ILjava/lang/String;Ljava/lang/String;)V",
            &[
                JavaValue::Int(FIXTURE_UID),
                JavaValue::Object(package_name),
                JavaValue::Null,
            ],
        )
        .expect("AttributionSource");
    let uri_text = string(vm, URI);
    let uri = obj(vm
        .call_static(
            "android.net.Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JavaValue::Object(uri_text)],
        )
        .expect("Uri.parse"));

    // insert
    let values = vm
        .new_instance_ctor("android.content.ContentValues", "()V", &[])
        .expect("ContentValues");
    let payload_key = string(vm, "payload");
    let payload_value = string(vm, "hello");
    vm.call_method(
        values,
        "put",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[
            JavaValue::Object(payload_key),
            JavaValue::Object(payload_value),
        ],
    )
    .expect("ContentValues.put");
    let inserted = obj(
        vm.call_method(
            proxy,
            "insert",
            "(Landroid/content/AttributionSource;Landroid/net/Uri;Landroid/content/ContentValues;Landroid/os/Bundle;)Landroid/net/Uri;",
            &[
                JavaValue::Object(attribution),
                JavaValue::Object(uri),
                JavaValue::Object(values),
                JavaValue::Null,
            ],
        )
        .expect("ContentProviderProxy.insert"),
    );
    let inserted_text = obj(vm
        .call_method(inserted, "toString", "()Ljava/lang/String;", &[])
        .expect("Uri.toString"));
    assert_eq!(
        vm.java_string_to_rust(inserted_text).expect("inserted uri"),
        "content://dev.acos.fixtures.svcprovider/echo/0"
    );

    // getType: `ContentProvider.Transport.getType` is reached through the
    // framework's own `ContentProviderProxy` / `ContentProviderNative` pair, and
    // the reply is the string the app's provider declares.
    let type_h = obj(vm
        .call_method(
            proxy,
            "getType",
            "(Landroid/content/AttributionSource;Landroid/net/Uri;)Ljava/lang/String;",
            &[JavaValue::Object(attribution), JavaValue::Object(uri)],
        )
        .expect("ContentProviderProxy.getType"));
    assert_eq!(
        vm.java_string_to_rust(type_h).expect("getType string"),
        "vnd.android.cursor.dir/vnd.acos.echo"
    );

    // query: the cursor is a real Java object materialised by the framework's
    // own `ContentProviderNative`/`BulkCursorDescriptor` reply.
    let id_column = string(vm, "_id");
    let payload_column = string(vm, "payload");
    let projection = vm
        .new_array(
            "Ljava/lang/String;",
            &[
                JavaValue::Object(id_column),
                JavaValue::Object(payload_column),
            ],
        )
        .expect("projection");
    let cursor = obj(
        vm.call_method(
            proxy,
            "query",
            "(Landroid/content/AttributionSource;Landroid/net/Uri;[Ljava/lang/String;Landroid/os/Bundle;Landroid/os/ICancellationSignal;)Landroid/database/Cursor;",
            &[
                JavaValue::Object(attribution),
                JavaValue::Object(uri),
                JavaValue::Object(projection),
                JavaValue::Null,
                JavaValue::Null,
            ],
        )
        .expect("ContentProviderProxy.query"),
    );
    match vm
        .call_method(cursor, "getCount", "()I", &[])
        .expect("getCount")
    {
        JavaValue::Int(1) => {}
        other => panic!("expected one cursor row, got {}", other.kind()),
    }
    assert!(matches!(
        vm.call_method(cursor, "moveToFirst", "()Z", &[])
            .expect("moveToFirst"),
        JavaValue::Boolean(true)
    ));
    let payload = obj(vm
        .call_method(
            cursor,
            "getString",
            "(I)Ljava/lang/String;",
            &[JavaValue::Int(1)],
        )
        .expect("getString"));
    assert_eq!(vm.java_string_to_rust(payload).expect("payload"), "hello");
    vm.call_method(cursor, "close", "()V", &[]).expect("close");

    // update then delete
    assert!(matches!(
        vm.call_method(
            proxy,
            "update",
            "(Landroid/content/AttributionSource;Landroid/net/Uri;Landroid/content/ContentValues;Landroid/os/Bundle;)I",
            &[
                JavaValue::Object(attribution),
                JavaValue::Object(uri),
                JavaValue::Object(values),
                JavaValue::Null,
            ],
        )
        .expect("update"),
        JavaValue::Int(0)
    ));
    assert!(matches!(
        vm.call_method(
            proxy,
            "delete",
            "(Landroid/content/AttributionSource;Landroid/net/Uri;Landroid/os/Bundle;)I",
            &[
                JavaValue::Object(attribution),
                JavaValue::Object(uri),
                JavaValue::Null,
            ],
        )
        .expect("delete"),
        JavaValue::Int(1)
    ));
}
