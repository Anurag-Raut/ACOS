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

use crate::permission_catalog;
use crate::value::{Parcelable, Value as WireValue};

/// `Process.SYSTEM_UID`: `system_server`, the uid every host-originated binder
/// transaction runs as (BINDERPID).
pub const SYSTEM_UID: u32 = 1000;

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

/// The `android.permission-group.*` prefix a `PermissionInfo.group` carries.
pub const GROUP_PREFIX: &str = "android.permission-group.";

/// `PermissionInfo.PROTECTION_*` bits (`base/core/java/android/content/pm/PermissionInfo.java:50-310`).
///
/// Used to turn the `<permission android:protectionLevel="...">` attribute an
/// app declares into the int `PermissionInfo.protectionLevel` carries.
#[must_use]
pub fn protection_bits(declared: &str) -> i32 {
    let mut parts = declared.split('|');
    let mut bits = match parts.next().unwrap_or("normal").trim() {
        "normal" => 0x0,
        "dangerous" => 0x1,
        "signature" => 0x2,
        "signatureOrSystem" => 0x3,
        "internal" => 0x4,
        // `PackageParser.parsePermission`: an unknown base level is an install
        // error; the store refuses rather than inventing one, so it keeps
        // `normal` and records nothing else.
        _ => 0x0,
    };
    for flag in parts {
        bits |= match flag.trim() {
            "privileged" | "system" => 0x10,
            "development" => 0x20,
            "appop" => 0x40,
            "pre23" => 0x80,
            "installer" => 0x100,
            "verifier" => 0x200,
            "preinstalled" => 0x400,
            "setup" => 0x800,
            "instant" => 0x1000,
            "runtime" => 0x2000,
            "oem" => 0x4000,
            "vendorPrivileged" => 0x8000,
            "textClassifier" => 0x1_0000,
            "wellbeing" => 0x2_0000,
            "documenter" => 0x4_0000,
            "configurator" => 0x8_0000,
            "incidentReportApprover" => 0x10_0000,
            "appPredictor" => 0x20_0000,
            "module" => 0x40_0000,
            "companion" => 0x80_0000,
            "retailDemo" => 0x100_0000,
            "recents" => 0x200_0000,
            "role" => 0x400_0000,
            "knownSigner" => 0x800_0000,
            _ => 0,
        };
    }
    bits
}

/// One permission's `PermissionInfo` facts, as `IPermissionManager` answers
/// them.
///
/// This is the *description* half of the permission model (SYSUI3B): the
/// runtime state stays in [`PermissionStore`]'s entry table, and the label /
/// group / description come from [`crate::permission_catalog`] for a platform
/// permission or from the installed package's own `<permission>` declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDescription {
    /// The permission name (`android.permission.CAMERA`).
    pub name: String,
    /// The defining package (`android` for a platform permission).
    pub package_name: String,
    /// `PackageItemInfo.nonLocalizedLabel`: what `PermissionInfo.loadLabel`
    /// returns without resolving resources.
    pub label: Option<String>,
    /// `PermissionInfo.nonLocalizedDescription`.
    pub description: Option<String>,
    /// The full group name (`android.permission-group.CAMERA`).
    pub group: String,
    /// `PermissionInfo.protectionLevel`.
    pub protection_level: i32,
    /// `android:backgroundPermission`, when declared.
    pub background_permission: Option<String>,
}

impl PermissionDescription {
    /// The `android.content.pm.PermissionInfo` parcelable payload, in the
    /// image's own `writeToParcel` order (see [`crate::parcelable`]).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.PermissionInfo",
            vec![
                // PackageItemInfo prefix
                WireValue::Str8(Some(self.name.clone())),
                WireValue::Str8(Some(self.package_name.clone())),
                WireValue::Int(0), // banner
                WireValue::Str(self.label.clone()),
                WireValue::Int(0), // icon
                WireValue::Int(0), // labelRes (the label is non-localized)
                WireValue::Bundle(None),
                WireValue::Int(0), // showUser
                WireValue::Int(0), // showBadge
                // PermissionInfo
                WireValue::Int(self.protection_level),
                WireValue::Int(0), // flags
                WireValue::Str8(Some(self.group.clone())),
                WireValue::Str8(self.background_permission.clone()),
                WireValue::Int(0), // descriptionRes (non-localized)
                WireValue::Int(0), // requestRes
                WireValue::Str(self.description.clone()),
                // `knownCerts` is a `Parcelling.ForStringSet`; a null set is the
                // same leading int the verified `PermissionInfo` codec writes.
                WireValue::Int(0),
            ],
        )
    }
}

/// One permission group's `PermissionGroupInfo` facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDescription {
    /// The full group name (`android.permission-group.CAMERA`).
    pub name: String,
    /// The defining package (`android`).
    pub package_name: String,
    /// `PackageItemInfo.nonLocalizedLabel` (the dialog's group title).
    pub label: Option<String>,
    /// `PermissionGroupInfo.nonLocalizedDescription`.
    pub description: Option<String>,
    /// `PermissionGroupInfo.priority`.
    pub priority: i32,
}

impl GroupDescription {
    /// The `android.content.pm.PermissionGroupInfo` parcelable payload, in the
    /// image's own `writeToParcel` order (see [`crate::parcelable`]).
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.content.pm.PermissionGroupInfo",
            vec![
                // PackageItemInfo prefix
                WireValue::Str8(Some(self.name.clone())),
                WireValue::Str8(Some(self.package_name.clone())),
                WireValue::Int(0), // banner
                WireValue::Str(self.label.clone()),
                WireValue::Int(0), // icon
                WireValue::Int(0), // labelRes (the label is non-localized)
                WireValue::Bundle(None),
                WireValue::Int(0), // showUser
                WireValue::Int(0), // showBadge
                // PermissionGroupInfo
                WireValue::Int(0), // descriptionRes (non-localized)
                WireValue::Int(0), // requestRes
                WireValue::Int(0), // requestDetailResourceId
                WireValue::Int(0), // backgroundRequestResourceId
                WireValue::Int(0), // backgroundRequestDetailResourceId
                WireValue::Str(self.description.clone()),
                WireValue::Int(0), // flags
                WireValue::Int(self.priority),
            ],
        )
    }
}

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
    /// `<permission>` elements the installed package declares itself (SYSUI3B):
    /// a custom permission's `PermissionInfo` comes from here, never from the
    /// platform catalog and never from a guess.
    declared: Vec<acos_apk::manifest::DeclaredPermission>,
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
    /// Every installed package's `(uid, package, manifest grants)`, published
    /// by the PMS (`PmsState::uid_grants`). This is the table
    /// `IPermissionManager.checkUidPermission` answers from: the framework asks
    /// that question with a **uid** (`ActivityManager.checkComponentPermission`
    /// -> `Context.checkCallingPermission`), and only the package manager knows
    /// which package (and which install-time grants) a uid names.
    pub uid_grants: Vec<(i32, String, Vec<String>)>,
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
            declared: Vec::new(),
            requests: Vec::new(),
            refused: Vec::new(),
            mutations: Vec::new(),
            dialogs: false,
            uid_grants: Vec::new(),
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

    /// Publish the installed package's own `<permission>` declarations
    /// (SYSUI3B). The PMS calls this with the same state change that publishes
    /// the install-time grants, so `getPermissionInfo` for a custom permission
    /// and `checkPermission` for it can never disagree about the package.
    pub fn set_declared_permissions(
        &mut self,
        package: &str,
        declared: &[acos_apk::manifest::DeclaredPermission],
    ) {
        if self.package.is_empty() && !package.is_empty() {
            self.package = package.to_string();
        }
        self.declared = declared.to_vec();
    }

    /// `IPermissionManager.getPermissionInfo`: the platform catalog first (the
    /// AOSP label/description/group), then the installed package's own
    /// `<permission>` declarations. `None` is the honest answer for an unknown
    /// permission: the framework turns it into `NameNotFoundException`, and the
    /// dialog's last-segment fallback is what a device would show for a
    /// permission ACOS cannot describe.
    #[must_use]
    pub fn permission_description(&self, permission: &str) -> Option<PermissionDescription> {
        if let Some(row) = permission_catalog::permission(permission) {
            return Some(PermissionDescription {
                name: row.name.to_string(),
                package_name: "android".to_string(),
                label: row.label.map(str::to_string),
                description: row.description.map(str::to_string),
                group: format!("{GROUP_PREFIX}{}", row.group),
                protection_level: row.protection_level,
                background_permission: row.background_permission.map(str::to_string),
            });
        }
        let declared = self.declared.iter().find(|row| row.name == permission)?;
        Some(PermissionDescription {
            name: declared.name.clone(),
            package_name: self.package.clone(),
            label: declared.label.clone(),
            description: declared.description.clone(),
            group: declared
                .group
                .clone()
                .unwrap_or_else(|| format!("{GROUP_PREFIX}UNDEFINED")),
            protection_level: protection_bits(
                declared.protection_level.as_deref().unwrap_or("normal"),
            ),
            background_permission: declared.background_permission.clone(),
        })
    }

    /// `IPermissionManager.getPermissionGroupInfo`: a platform group in AOSP's
    /// own words. A group the app declared itself has no catalog row (its label
    /// would live in the app's resources, which ACOS does not resolve), so the
    /// honest answer is `None`.
    #[must_use]
    pub fn group_description(&self, group: &str) -> Option<GroupDescription> {
        let row = permission_catalog::group(group)?;
        Some(GroupDescription {
            name: row.name.to_string(),
            package_name: "android".to_string(),
            label: row.label.map(str::to_string),
            description: row.description.map(str::to_string),
            priority: row.priority,
        })
    }

    /// `IPermissionManager.queryPermissionsByGroup`: every platform permission
    /// in `group`, plus any the installed package declares in it.
    #[must_use]
    pub fn permissions_in_group(&self, group: &str) -> Vec<PermissionDescription> {
        let short = group.strip_prefix(GROUP_PREFIX).unwrap_or(group);
        let mut rows: Vec<PermissionDescription> = permission_catalog::permissions_in_group(short)
            .into_iter()
            .filter_map(|row| self.permission_description(row.name))
            .collect();
        for declared in &self.declared {
            let declared_short = declared
                .group
                .as_deref()
                .map(|name| name.strip_prefix(GROUP_PREFIX).unwrap_or(name));
            if declared_short == Some(short) {
                if let Some(row) = self.permission_description(&declared.name) {
                    rows.push(row);
                }
            }
        }
        rows
    }

    /// `IPermissionManager.getAllPermissionGroups`: every platform group.
    #[must_use]
    pub fn group_descriptions(&self) -> Vec<GroupDescription> {
        permission_catalog::GROUPS
            .iter()
            .filter_map(|row| self.group_description(row.name))
            .collect()
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

    /// Publish the installed `(uid, package, grants)` table (called by the PMS
    /// whenever the installed set changes).
    pub fn set_uid_grants(&mut self, grants: Vec<(i32, String, Vec<String>)>) {
        self.uid_grants = grants;
    }

    /// `IPermissionManager.checkUidPermission(int uid, String permission, int
    /// deviceId)`: the grant **for a uid**, which is the question
    /// `Context.checkCallingPermission`/`Context.checkPermission` ask through
    /// `ActivityManager.checkComponentPermission`.
    ///
    /// AOSP answers from the per-uid permission state
    /// (`PermissionManagerService.checkUidPermission`): system/root are granted
    /// everything, an isolated app gets nothing, and every other uid is granted
    /// when one of its packages holds the permission. The runtime table
    /// describes exactly one package, so it applies to that package; a second
    /// package answers from its own install-time grants. A uid no package owns
    /// gets the historical single-package answer when the table describes no
    /// package at all (a bare host), otherwise `DENIED` — never a fabricated
    /// grant.
    #[must_use]
    pub fn check_uid_permission(&self, uid: u32, permission: &str) -> i32 {
        const AID_SYSTEM: u32 = SYSTEM_UID;
        const AID_ROOT: u32 = 0;
        // `UserHandle.getAppId`: the app id is the uid modulo the per-user
        // range (`PER_USER_RANGE` = 100000).
        let app_id = uid % 100_000;
        if app_id == AID_ROOT || app_id == AID_SYSTEM {
            return PERMISSION_GRANTED;
        }
        // Isolated processes (`AID_ISOLATED_START`..`AID_ISOLATED_END`) get no
        // permissions.
        if (99000..100000).contains(&app_id) {
            return PERMISSION_DENIED;
        }
        let owned: Vec<&(i32, String, Vec<String>)> = self
            .uid_grants
            .iter()
            .filter(|(owner, _, _)| *owner == uid as i32)
            .collect();
        if owned.is_empty() {
            if self.uid_grants.is_empty() && !self.package.is_empty() {
                // The PMS never published a uid table (a host built without a
                // package manager): the one package's table is all there is.
                return self.check(permission);
            }
            return PERMISSION_DENIED;
        }
        for (_, package, grants) in owned {
            if package == &self.package {
                if self.check(permission) == PERMISSION_GRANTED {
                    return PERMISSION_GRANTED;
                }
            } else if grants.iter().any(|p| p == permission) {
                return PERMISSION_GRANTED;
            }
        }
        PERMISSION_DENIED
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
