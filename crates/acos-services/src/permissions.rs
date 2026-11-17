//! The one runtime-permission table (MODNOTIF2, ADR-0029 amendment).
//!
//! Before this module every permission answer came from the APK manifest:
//! `PmsState::check_permission` and `AtmsState::check_permission` each kept a
//! copy of the install-time `granted_permissions` list, and
//! `IPermissionManager` threw `UNSUPPORTED_ANDROID_API` at
//! `grantRuntimePermission`. That is three tables for one question, so
//! "grant changes `checkSelfPermission`" was impossible by construction.
//!
//! There is now **one** table. `registry::install` creates it and hands the
//! same `Arc` to `PmsState`, `AtmsState` and [`crate::host::ServiceHost`]; the
//! `permissions` module attaches to the same slot. Every read path answers
//! from it:
//!
//! | path | AOSP source |
//! |---|---|
//! | `IPackageManager.checkPermission` | `PackageManager.checkPermission` |
//! | `IActivityManager.checkPermission` | `Context.checkPermission` |
//! | `IPermissionManager.shouldShowRequestPermissionRationale` | `Activity.shouldShowRequestPermissionRationale` (`base/core/java/android/app/Activity.java:5761`) |
//! | `IPermissionManager.getPermissionFlags` | `PackageManager.getPermissionFlags` |
//! | `NotificationManager.areNotificationsEnabled` (via POST_NOTIFICATIONS) | `PermissionPolicyService` (`targetSdk >= 33`) |
//!
//! ## Semantics (AOSP, not invented)
//!
//! * **grant** — `grantRuntimePermission`: granted, `FLAG_PERMISSION_USER_SET`.
//! * **deny** — the user answered "Don't allow": not granted,
//!   `FLAG_PERMISSION_USER_SET`; the next `requestPermissions` may ask again.
//! * **deny, never ask again** — "Don't allow" + `FLAG_PERMISSION_USER_FIXED`:
//!   the dialog must not be shown again (`shouldShowRequestPermissionRationale`
//!   answers `false`, which is what the documentation says:
//!   "false if the user has previously denied the request and selected
//!   'don't ask again'").
//! * **revoke** — `revokeRuntimePermission`: back to the install-time answer,
//!   with the user-set/fixed flags cleared (a fresh install state).
//! * **rationale** — `true` only when the app asked, the user denied, it is not
//!   `USER_FIXED` and it is still not granted: the documented contract of
//!   `Activity.shouldShowRequestPermissionRationale`.
//!
//! A permission that has no entry in the table answers from the manifest
//! (install-time grants), exactly as before, so a normal permission keeps
//! working and a dangerous one starts *not* granted — which is what a phone
//! does (manifest `<uses-permission>` is a declaration, not a grant).

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// `PackageManager.PERMISSION_GRANTED`.
pub const PERMISSION_GRANTED: i32 = 0;
/// `PackageManager.PERMISSION_DENIED`.
pub const PERMISSION_DENIED: i32 = -1;

/// `PackageManager.ACTION_REQUEST_PERMISSIONS`
/// (`base/core/java/android/content/pm/PackageManager.java:4998`).
pub const ACTION_REQUEST_PERMISSIONS: &str = "android.content.pm.action.REQUEST_PERMISSIONS";
/// `PackageManager.EXTRA_REQUEST_PERMISSIONS_NAMES` (`:5021`).
pub const EXTRA_REQUEST_PERMISSIONS_NAMES: &str =
    "android.content.pm.extra.REQUEST_PERMISSIONS_NAMES";
/// The package `PackageManager.getPermissionControllerPackageName()` answers
/// (`PackageManager.java:7049` -> `setPackage(...)`).
pub const PERMISSION_CONTROLLER_PACKAGE: &str = "com.android.permissioncontroller";
/// `PackageManager.EXTRA_REQUEST_PERMISSIONS_RESULTS` (`:5047`): the `int[]`
/// grant results the controller returns (`PERMISSION_GRANTED`/`PERMISSION_DENIED`).
pub const EXTRA_REQUEST_PERMISSIONS_RESULTS: &str =
    "android.content.pm.extra.REQUEST_PERMISSIONS_RESULTS";
/// ACOS system-app result contract: the controller's "Don't ask again" answer
/// (`FLAG_PERMISSION_USER_FIXED`). AOSP's controller calls
/// `IPermissionManager.updatePermissionFlags(… FLAG_PERMISSION_USER_FIXED …)`
/// directly; the ACOS guest app cannot reach the hidden `IPermissionManager`
/// stub, so it says so in the result Intent and the host applies the same flag.
pub const EXTRA_REQUEST_PERMISSIONS_NEVER_ASK_AGAIN: &str =
    "android.content.pm.extra.REQUEST_PERMISSIONS_NEVER_ASK_AGAIN";
/// ACOS system-app launch contract: permissions already granted when the
/// request was routed (the controller skips them, like
/// `GrantPermissionsActivity.allRequestedPermissionsGranted`).
pub const EXTRA_GRANTED: &str = "acos.permission.GRANTED";
/// ACOS system-app launch contract: permissions denied with
/// `FLAG_PERMISSION_USER_FIXED` ("don't ask again"), which the controller does
/// not ask about again.
pub const EXTRA_FIXED: &str = "acos.permission.FIXED";
/// ACOS system-app launch contract: the requesting package (the controller's
/// `getCallingPackage()` equivalent; the system app cannot read the caller).
pub const EXTRA_CALLING_PACKAGE: &str = "acos.permission.CALLING_PACKAGE";

/// A `REQUEST_PERMISSIONS` intent, reduced to what the system needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsRequest {
    /// The permissions the app asked for, in the app's order.
    pub permissions: Vec<String>,
    /// The intent's target package (`com.android.permissioncontroller`).
    pub package: String,
}

/// Recognise the framework's permission-request intent.
///
/// `Activity.requestPermissions` builds it with `buildRequestPermissionsIntent`
/// (`ACTION_REQUEST_PERMISSIONS` + `EXTRA_REQUEST_PERMISSIONS_NAMES`, addressed
/// to the permission controller package) and starts it for a result. Anything
/// that is not that intent is `None` and takes the normal launch path.
#[must_use]
pub fn permission_request(intent: &crate::ams::IntentFacts) -> Option<PermissionsRequest> {
    let is_request = intent.action.as_deref() == Some(ACTION_REQUEST_PERMISSIONS)
        || (intent.package.as_deref() == Some(PERMISSION_CONTROLLER_PACKAGE)
            && intent.extras.contains_key(EXTRA_REQUEST_PERMISSIONS_NAMES));
    if !is_request {
        return None;
    }
    let permissions = match intent.extras.get(EXTRA_REQUEST_PERMISSIONS_NAMES) {
        Some(crate::ams::Extra::Strings(items)) => items.clone(),
        _ => Vec::new(),
    };
    Some(PermissionsRequest {
        permissions,
        package: intent
            .package
            .clone()
            .unwrap_or_else(|| PERMISSION_CONTROLLER_PACKAGE.to_string()),
    })
}

/// `PackageManager.FLAG_PERMISSION_USER_SET` (`base/core/java/android/content/pm/PackageManager.java:5090`).
pub const FLAG_PERMISSION_USER_SET: i32 = 1 << 0;
/// `PackageManager.FLAG_PERMISSION_USER_FIXED` (`:5100`) — "don't ask again".
pub const FLAG_PERMISSION_USER_FIXED: i32 = 1 << 1;
/// `PackageManager.FLAG_PERMISSION_POLICY_FIXED` (`:5110`) — never set by ACOS.
pub const FLAG_PERMISSION_POLICY_FIXED: i32 = 1 << 2;
/// `PackageManager.FLAG_PERMISSION_SYSTEM_FIXED` (`:5135`) — never set by ACOS.
pub const FLAG_PERMISSION_SYSTEM_FIXED: i32 = 1 << 4;
/// `PackageManager.FLAG_PERMISSION_GRANTED_BY_DEFAULT` (`:5146`) — an install-time grant.
pub const FLAG_PERMISSION_GRANTED_BY_DEFAULT: i32 = 1 << 5;

/// One permission's runtime state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionEntry {
    /// The permission is granted right now.
    pub granted: bool,
    /// The user answered the dialog at least once (`FLAG_PERMISSION_USER_SET`).
    pub user_set: bool,
    /// The user chose "don't ask again" (`FLAG_PERMISSION_USER_FIXED`).
    pub user_fixed: bool,
    /// The app actually called `requestPermissions` for it.
    pub requested: bool,
}

impl PermissionEntry {
    /// The `getPermissionFlags` answer.
    #[must_use]
    pub fn flags(&self, install_time_granted: bool) -> i32 {
        let mut flags = 0;
        if self.user_set {
            flags |= FLAG_PERMISSION_USER_SET;
        }
        if self.user_fixed {
            flags |= FLAG_PERMISSION_USER_FIXED;
        }
        if self.granted && !self.user_set && install_time_granted {
            flags |= FLAG_PERMISSION_GRANTED_BY_DEFAULT;
        }
        flags
    }
}

/// One `requestPermissions` the framework delivered to the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    /// Permissions asked for, in the app's order.
    pub permissions: Vec<String>,
    /// `Activity.requestPermissions` request code.
    pub request_code: i32,
    /// `PackageManager.EXTRA_REQUEST_PERMISSIONS_NAMES` target package
    /// (`com.android.permissioncontroller` on a device).
    pub package: String,
}

/// The runtime-permission table.
///
/// ACOS installs one package per session, so the table is keyed by permission
/// name; `package` records which app it belongs to for the JSON control op.
#[derive(Debug, Default)]
pub struct PermissionStore {
    /// The package these answers are for (the installed APK).
    pub package: String,
    /// Per-permission state, in permission-name order.
    entries: BTreeMap<String, PermissionEntry>,
    /// Manifest permissions, published by the PMS when its state is built.
    install_time: Vec<String>,
    /// Every `requestPermissions` the system received, in order.
    pub requests: Vec<PermissionRequest>,
    /// Requests the system could not hand to a permission dialog (no controller
    /// system app), kept as evidence: `(permissions, package)`.
    pub refused: Vec<(Vec<String>, String)>,
    /// Control-op/driver mutations, in order (`grant:camera`, ...).
    pub mutations: Vec<String>,
    /// SYSUI1: the `permissions` module's `system_ui` setting. `true` (the
    /// `real` profile's default) routes `requestPermissions` to the guest
    /// permission-controller system app; `false` keeps MODNOTIF2's structured
    /// `PERMISSION_UI_UNAVAILABLE`.
    dialogs: bool,
}

impl PermissionStore {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the table for `package` with the manifest's install-time grants.
    #[must_use]
    pub fn for_package(package: &str, install_time: &[String]) -> Self {
        Self {
            package: package.to_string(),
            entries: BTreeMap::new(),
            install_time: install_time.to_vec(),
            requests: Vec::new(),
            refused: Vec::new(),
            mutations: Vec::new(),
            dialogs: false,
        }
    }

    /// Turn the permission dialog on/off (`system_ui: dialogs|off`).
    pub fn set_dialogs(&mut self, enabled: bool) {
        self.dialogs = enabled;
        self.mutations
            .push(format!("system_ui:{}", if enabled { "dialogs" } else { "off" }));
    }

    /// Whether the framework's request is routed to the controller system app.
    #[must_use]
    pub fn dialogs_enabled(&self) -> bool {
        self.dialogs
    }

    /// True when the permission was denied with "don't ask again"
    /// (`FLAG_PERMISSION_USER_FIXED`), which suppresses a second dialog.
    #[must_use]
    pub fn never_ask_again(&self, permission: &str) -> bool {
        self.entries
            .get(permission)
            .is_some_and(|entry| entry.user_fixed && !entry.granted)
    }

    /// Attach the installed package's manifest grants (called by the PMS when
    /// its state is built; the table is shared, so it is filled in place).
    pub fn set_install_time(&mut self, package: &str, granted: &[String]) {
        if !package.is_empty() {
            self.package = package.to_string();
        }
        self.install_time = granted.to_vec();
    }

    /// True when the manifest declared this permission.
    #[must_use]
    pub fn is_declared(&self, permission: &str) -> bool {
        self.install_time.iter().any(|p| p == permission)
    }

    /// The entry for `permission`, when the runtime table has one.
    #[must_use]
    pub fn entry(&self, permission: &str) -> Option<&PermissionEntry> {
        self.entries.get(permission)
    }

    /// `grantRuntimePermission`: grant and mark the user as having set it.
    ///
    /// `user_set`/`requested` are what a dialog answer implies: the app asked
    /// and the user answered. A *pre-grant* configured before launch is the same
    /// thing done ahead of time, which is why the fixture app never needs a
    /// dialog in that arm.
    pub fn grant(&mut self, permission: &str) {
        let entry = self.entries.entry(permission.to_string()).or_default();
        entry.granted = true;
        entry.user_set = true;
        entry.user_fixed = false;
        entry.requested = true;
        self.mutations.push(format!("grant:{permission}"));
    }

    /// The user answered "Don't allow" (`never_ask_again` = "Don't allow" plus
    /// the system's `USER_FIXED` flag).
    pub fn deny(&mut self, permission: &str, never_ask_again: bool) {
        let entry = self.entries.entry(permission.to_string()).or_default();
        entry.granted = false;
        entry.user_set = true;
        entry.user_fixed = never_ask_again;
        entry.requested = true;
        self.mutations.push(format!(
            "deny:{permission}{}",
            if never_ask_again { ":fixed" } else { "" }
        ));
    }

    /// `revokeRuntimePermission`: forget the runtime state, back to install.
    pub fn revoke(&mut self, permission: &str) {
        self.entries.remove(permission);
        self.mutations.push(format!("revoke:{permission}"));
    }

    /// `updatePermissionFlags` with `FLAG_PERMISSION_USER_SET` in the mask.
    pub fn set_user_set(&mut self, permission: &str, value: bool) {
        let entry = self.entries.entry(permission.to_string()).or_default();
        entry.user_set = value;
        self.mutations
            .push(format!("update_flags:{permission}:user_set={value}"));
    }

    /// `updatePermissionFlags` with `FLAG_PERMISSION_USER_FIXED` in the mask.
    pub fn set_user_fixed(&mut self, permission: &str, value: bool) {
        let entry = self.entries.entry(permission.to_string()).or_default();
        entry.user_fixed = value;
        self.mutations
            .push(format!("update_flags:{permission}:user_fixed={value}"));
    }

    /// Record that the framework asked for these permissions. `requested` is
    /// what makes `shouldShowRequestPermissionRationale` start answering `true`
    /// after the first denial.
    pub fn note_request(&mut self, permissions: &[String], request_code: i32, package: &str) {
        for permission in permissions {
            self.entries
                .entry(permission.clone())
                .or_default()
                .requested = true;
        }
        self.requests.push(PermissionRequest {
            permissions: permissions.to_vec(),
            request_code,
            package: package.to_string(),
        });
    }

    /// Record a request that never reached a dialog: the permission controller
    /// system app does not exist yet, so the app's request was refused.
    pub fn refuse_request(&mut self, permissions: &[String], package: &str) {
        self.refused
            .push((permissions.to_vec(), package.to_string()));
    }

    /// Record a request the dialog system app is answering (SYSUI1): the same
    /// `requested` flag the framework path sets, so
    /// `shouldShowRequestPermissionRationale` keeps answering the documented
    /// contract ("true only after the user denied it").
    pub fn record_request(&mut self, permissions: &[String], package: &str) {
        for permission in permissions {
            self.entries
                .entry(permission.clone())
                .or_default()
                .requested = true;
        }
        self.requests.push(PermissionRequest {
            permissions: permissions.to_vec(),
            request_code: -1,
            package: package.to_string(),
        });
    }

    /// Apply the controller's dialog answer to the table (SYSUI1).
    ///
    /// `names` and `results` are the AOSP result extras
    /// (`EXTRA_REQUEST_PERMISSIONS_NAMES` / `EXTRA_REQUEST_PERMISSIONS_RESULTS`);
    /// `never_ask_again` is the ACOS system-app extra for "Don't ask again".
    /// Each permission takes the same path `IPermissionManager` takes:
    /// granted → `grant` (USER_SET), denied → `deny` (USER_SET, + USER_FIXED
    /// when the user ticked "Don't ask again").
    pub fn apply_dialog_result(
        &mut self,
        names: &[String],
        results: &[i32],
        never_ask_again: bool,
    ) {
        for (index, permission) in names.iter().enumerate() {
            match results.get(index).copied() {
                Some(PERMISSION_GRANTED) => self.grant(permission),
                Some(_) => self.deny(permission, never_ask_again),
                None => continue,
            }
        }
    }

    /// `checkPermission`: the runtime table wins over the manifest.
    #[must_use]
    pub fn check(&self, permission: &str) -> i32 {
        if let Some(entry) = self.entries.get(permission) {
            return if entry.granted {
                PERMISSION_GRANTED
            } else {
                PERMISSION_DENIED
            };
        }
        if self.is_declared(permission) {
            PERMISSION_GRANTED
        } else {
            PERMISSION_DENIED
        }
    }

    /// `shouldShowRequestPermissionRationale`.
    ///
    /// `true` only for: asked before, denied by the user, not "don't ask again",
    /// still not granted. Everything else is `false` — including a permission
    /// that is already granted (the documented contract).
    #[must_use]
    pub fn should_show_rationale(&self, permission: &str) -> bool {
        self.entries.get(permission).is_some_and(|entry| {
            entry.requested && entry.user_set && !entry.user_fixed && !entry.granted
        })
    }

    /// `getPermissionFlags`.
    #[must_use]
    pub fn flags(&self, permission: &str) -> i32 {
        let install_time_granted = self.is_declared(permission);
        match self.entries.get(permission) {
            Some(entry) => entry.flags(install_time_granted),
            None => {
                if install_time_granted {
                    FLAG_PERMISSION_GRANTED_BY_DEFAULT
                } else {
                    0
                }
            }
        }
    }

    /// True when every permission in `permissions` is granted. Used by the
    /// request path: `GrantPermissionsActivity` grants immediately and returns
    /// when nothing is left to ask (`allRequestedPermissionsGranted`).
    #[must_use]
    pub fn all_granted(&self, permissions: &[String]) -> bool {
        permissions
            .iter()
            .all(|permission| self.check(permission) == PERMISSION_GRANTED)
    }

    /// The honest `state` control op.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut permissions: Vec<Value> = self
            .entries
            .iter()
            .map(|(name, entry)| {
                json!({
                    "permission": name,
                    "state": if entry.granted { "granted" } else { "denied" },
                    "granted": entry.granted,
                    "user_set": entry.user_set,
                    "never_ask_again": entry.user_fixed,
                    "requested": entry.requested,
                    "rationale": self.should_show_rationale(name),
                    "flags": self.flags(name),
                    "declared": self.is_declared(name),
                })
            })
            .collect();
        // Manifest permissions that were never touched are part of the state
        // too: an agent asking "what does this app have?" must see them.
        for declared in &self.install_time {
            if self.entries.contains_key(declared) {
                continue;
            }
            permissions.push(json!({
                "permission": declared,
                "state": "install-time",
                "granted": true,
                "user_set": false,
                "never_ask_again": false,
                "requested": false,
                "rationale": false,
                "flags": FLAG_PERMISSION_GRANTED_BY_DEFAULT,
                "declared": true,
            }));
        }
        json!({
            "package": self.package,
            "permissions": permissions,
            "requests": self.requests.iter().map(|request| json!({
                "permissions": request.permissions,
                "request_code": request.request_code,
                "package": request.package,
            })).collect::<Vec<_>>(),
            "refused_requests": self.refused.iter().map(|(permissions, package)| json!({
                "permissions": permissions,
                "package": package,
                "reason": "PERMISSION_UI_UNAVAILABLE",
            })).collect::<Vec<_>>(),
            "mutations": self.mutations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> PermissionStore {
        PermissionStore::for_package(
            "com.acos.probe",
            &["android.permission.INTERNET".to_string()],
        )
    }

    #[test]
    fn a_manifest_permission_is_install_time_granted_and_a_runtime_one_is_not() {
        let store = store();
        assert_eq!(
            store.check("android.permission.INTERNET"),
            PERMISSION_GRANTED
        );
        assert_eq!(
            store.check("android.permission.POST_NOTIFICATIONS"),
            PERMISSION_DENIED
        );
    }

    #[test]
    fn grant_deny_never_ask_again_and_revoke_follow_the_documented_contract() {
        let mut store = store();
        let camera = "android.permission.CAMERA";
        assert_eq!(store.check(camera), PERMISSION_DENIED);
        assert!(
            !store.should_show_rationale(camera),
            "never asked: no rationale"
        );
        store.note_request(&[camera.to_string()], 7, "com.android.permissioncontroller");
        assert!(
            !store.should_show_rationale(camera),
            "asked, not answered: no rationale"
        );
        store.deny(camera, false);
        assert_eq!(store.check(camera), PERMISSION_DENIED);
        assert!(
            store.should_show_rationale(camera),
            "denied once: show rationale"
        );
        assert_eq!(store.flags(camera), FLAG_PERMISSION_USER_SET);
        store.deny(camera, true);
        assert!(
            !store.should_show_rationale(camera),
            "don't ask again: no rationale"
        );
        assert_eq!(
            store.flags(camera),
            FLAG_PERMISSION_USER_SET | FLAG_PERMISSION_USER_FIXED
        );
        store.grant(camera);
        assert_eq!(store.check(camera), PERMISSION_GRANTED);
        assert!(
            !store.should_show_rationale(camera),
            "granted: no rationale"
        );
        store.revoke(camera);
        assert_eq!(
            store.check(camera),
            PERMISSION_DENIED,
            "revoked runtime permission is not a manifest grant"
        );
    }

    #[test]
    fn all_granted_is_false_for_an_empty_request_list() {
        let store = store();
        assert!(store.all_granted(&["android.permission.INTERNET".to_string()]));
        assert!(!store.all_granted(&["android.permission.CAMERA".to_string()]));
    }
}
