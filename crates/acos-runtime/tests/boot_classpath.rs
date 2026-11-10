//! Boot-classpath coverage: `framework.jar` references classes that Android 14
//! moved into sibling APEX jars, so the boot classpath must include every
//! APEX `bootclasspath.pb` fragment.
//!
//! `android.net.TetheringManager` lives in `framework-connectivity.jar` inside
//! `com.google.android.tethering.apex`, not in `framework.jar`. Before
//! `tools/extract-bootclasspath.sh` merged the APEX fragments, the M2 chain
//! stopped at `LoadedApk.makeApplication` with
//! `Landroid/net/TetheringManager; is not implemented`. This pins the real
//! VM-level presence, not just a filename in `BOOTCLASSPATH`.

mod common;

use common::new_runtime;

/// Classes that must resolve from the merged boot classpath, with the APEX jar
/// that defines them (from the API-34 image's `bootclasspath.pb` protos).
const APEX_BOOT_CLASSES: &[(&str, &str)] = &[
    ("Landroid/net/TetheringManager;", "framework-connectivity.jar"),
    ("Landroid/net/ConnectivityManager;", "framework-connectivity.jar"),
    (
        "Landroid/net/ipsec/ike/IkeSession;",
        "android.net.ipsec.ike.jar",
    ),
    (
        "Landroid/net/wifi/WifiManager;",
        "framework-wifi.jar",
    ),
    (
        "Landroid/permission/PermissionState;",
        "framework-permission.jar",
    ),
    (
        "Landroid/adservices/AdServicesState;",
        "framework-adservices.jar",
    ),
];

#[test]
fn boot_classpath_resolves_classes_from_apex_jars() {
    let Some(mut rt) = new_runtime() else {
        return;
    };
    for (desc, jar) in APEX_BOOT_CLASSES {
        match rt.vm_mut().load_class(desc) {
            Ok(_) => {}
            Err(e) => panic!(
                "{desc} (from {jar}) did not resolve from the boot classpath: {e}\n\
                 re-extract with tools/extract-bootclasspath.sh --force"
            ),
        }
    }
}
