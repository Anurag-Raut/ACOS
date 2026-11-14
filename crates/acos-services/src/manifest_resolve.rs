//! Launcher-intent resolution from the **parsed** manifest (W27).
//!
//! `aapt2 dump badging` reports `launchable-activity` only for a plain
//! `<activity>`; an app that declares its launcher entry as an
//! `<activity-alias>` (the Gaten debug APK does: three themed aliases targeting
//! one `LauncherActivity`) therefore looked like it had no launcher at all.
//! This module walks the AXML DOM with the same rules `PackageParser` uses:
//!
//! * a candidate is an `<activity>` **or** `<activity-alias>` whose
//!   `<intent-filter>` has both `android.intent.action.MAIN` and
//!   `android.intent.category.LAUNCHER`;
//! * `<activity-alias android:targetActivity="…">` must reference a declared
//!   `<activity>` in the same manifest (stock `parseActivityAlias` fails the
//!   parse when the target is missing);
//! * `android:enabled="false"` excludes the component (from the component or
//!   from `<application>`); a disabled alias is not a launcher;
//! * the alias copies the target's label/icon first and then applies its own
//!   attributes (`parseActivityAlias`); a literal label clears the resource id.
//!
//! Deterministic choice with several enabled launchers: **enabled aliases
//! first, then enabled activities, each in manifest declaration order**. An
//! alias exists specifically to be a launcher entry (themed icons), so it wins
//! over the target activity, which usually carries no launcher filter at all.
//! The choice and the full candidate list are recorded in
//! [`LauncherActivity::candidates`]; nothing is random and nothing is inferred.

use std::path::Path;

use acos_apk::axml::{TypedValue, XmlDocument, XmlNode};

use crate::pms::LabelSpec;

/// A resolved `MAIN`/`LAUNCHER` component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherActivity {
    /// The component the launcher Intent names (the alias when the entry is an
    /// `<activity-alias>`, otherwise the activity).
    pub component: String,
    /// `ActivityInfo.targetActivity` — the real class `ActivityThread` loads.
    /// `None` for a plain activity.
    pub target: Option<String>,
    /// The label `ActivityInfo.loadLabel` would use (alias label, else the
    /// target activity's label; both unset means the application label).
    pub label: LabelSpec,
    /// `android:icon` reference, alias overrides target (informational: the
    /// guest window path does not consume icons yet).
    pub icon_ref: Option<u32>,
    /// `true` when the chosen entry is an `<activity-alias>`.
    pub alias: bool,
    /// `ActivityInfo.configChanges` as `PackageParser.parseActivity` builds it:
    /// the manifest's `android:configChanges` bitmask (an `int` in the binary
    /// XML, the values are `ActivityInfo.CONFIG_*`) **or**-ed with the bits
    /// `getActivityConfigChanges()` always forces (`CONFIG_MCC|CONFIG_MNC`, from
    /// `RECREATE_ON_CONFIG_CHANGES_MASK`); an alias inherits the target's value
    /// (`parseActivityAlias`: `info.configChanges = target.info.configChanges`).
    /// This is what decides whether a display rotation re-creates the Activity
    /// (`acos_wm::RotationRelaunch::for_rotation`).
    pub config_changes: i32,
    /// Every enabled launcher candidate in resolution order (declaration
    /// order within the alias and activity groups), as `component` names.
    pub candidates: Vec<String>,
}

/// Why launcher resolution failed. Every variant names the manifest fact that
/// was missing, so `acos-art launch` can print a precise message instead of
/// "declares no launchable activity".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherError {
    /// The APK has no readable/parsable binary manifest.
    Manifest(String),
    /// No `<activity>`/`<activity-alias>` declares MAIN + LAUNCHER.
    NoLauncher,
    /// Launchers exist, but every one is `android:enabled="false"` (or the
    /// application is disabled).
    AllDisabled { components: Vec<String> },
    /// An alias with a launcher filter has no `android:targetActivity`.
    AliasWithoutTarget { alias: String },
    /// An alias targets an activity the manifest does not declare.
    AliasTargetMissing { alias: String, target: String },
}

impl std::fmt::Display for LauncherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "cannot read AndroidManifest.xml: {e}"),
            Self::NoLauncher => {
                write!(f, "no <activity>/<activity-alias> declares MAIN + LAUNCHER")
            }
            Self::AllDisabled { components } => write!(
                f,
                "MAIN/LAUNCHER entry is disabled (android:enabled=false): {}",
                components.join(", ")
            ),
            Self::AliasWithoutTarget { alias } => write!(
                f,
                "<activity-alias> {alias} declares MAIN/LAUNCHER but no android:targetActivity"
            ),
            Self::AliasTargetMissing { alias, target } => write!(
                f,
                "<activity-alias> {alias} targets {target}, which the manifest does not declare"
            ),
        }
    }
}

impl std::error::Error for LauncherError {}

impl LauncherError {
    /// Stable machine-readable discriminator (report/evidence friendly).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Manifest(_) => "LAUNCHER_MANIFEST_UNREADABLE",
            Self::NoLauncher => "LAUNCHER_NONE",
            Self::AllDisabled { .. } => "LAUNCHER_DISABLED",
            Self::AliasWithoutTarget { .. } => "LAUNCHER_ALIAS_NO_TARGET",
            Self::AliasTargetMissing { .. } => "LAUNCHER_ALIAS_TARGET_MISSING",
        }
    }
}

/// Resolve the launcher component of an APK from its binary manifest.
///
/// Served from [`crate::apk_facts`]: one launch asks for this several times, and
/// the APK is opened and parsed once per (path, length, mtime).
pub fn launcher_activity(apk: impl AsRef<Path>) -> Result<LauncherActivity, LauncherError> {
    let path = apk.as_ref();
    let facts = crate::apk_facts::launch_facts(path)
        .map_err(|e| LauncherError::Manifest(format!("{}: {e}", path.display())))?;
    facts.launcher.clone()
}

// ---------------------------------------------------------------- the model

#[derive(Debug, Clone)]
struct Decl {
    name: String,
    target: Option<String>,
    alias: bool,
    enabled: Option<bool>,
    label: LabelSpec,
    icon: Option<u32>,
    launcher: bool,
    config_changes: Option<i32>,
}

/// Walk the AXML DOM exactly as `PackageParser.parseBaseApplication` does.
pub fn resolve_from_dom(doc: &XmlDocument) -> Result<LauncherActivity, LauncherError> {
    let root = &doc.root;
    let package = attr_string(root, "package").unwrap_or_default();
    let Some(application) = root.children_named("application").next() else {
        return Err(LauncherError::NoLauncher);
    };
    let app_enabled = attr_bool(application, "enabled").unwrap_or(true);

    let mut decls: Vec<Decl> = Vec::new();
    for node in &application.children {
        match node.name.as_str() {
            "activity" => {
                if let Some(decl) = parse_component(node, &package, false) {
                    decls.push(decl);
                }
            }
            "activity-alias" => {
                if let Some(decl) = parse_component(node, &package, true) {
                    decls.push(decl);
                }
            }
            _ => {}
        }
    }

    // Stock `parseActivityAlias` requires the target to be a declared activity
    // and fails the whole manifest otherwise. Check aliases even when they are
    // disabled: a malformed manifest is a structured error, not a silent skip.
    let activity_names: Vec<&str> = decls
        .iter()
        .filter(|d| !d.alias)
        .map(|d| d.name.as_str())
        .collect();
    for decl in decls.iter().filter(|d| d.alias && d.launcher) {
        let Some(target) = decl.target.as_deref() else {
            return Err(LauncherError::AliasWithoutTarget {
                alias: decl.name.clone(),
            });
        };
        if !activity_names.contains(&target) {
            return Err(LauncherError::AliasTargetMissing {
                alias: decl.name.clone(),
                target: target.to_string(),
            });
        }
    }

    let launchers: Vec<&Decl> = decls.iter().filter(|d| d.launcher).collect();
    if launchers.is_empty() {
        return Err(LauncherError::NoLauncher);
    }

    let enabled = |d: &&Decl| app_enabled && d.enabled.unwrap_or(true);
    let mut disabled: Vec<String> = launchers
        .iter()
        .filter(|d| !enabled(d))
        .map(|d| d.name.clone())
        .collect();
    disabled.sort();
    disabled.dedup();

    // Enabled aliases first (declaration order), then enabled activities.
    let ordered: Vec<&Decl> = launchers
        .iter()
        .copied()
        .filter(enabled)
        .filter(|d| d.alias)
        .chain(
            launchers
                .iter()
                .copied()
                .filter(enabled)
                .filter(|d| !d.alias),
        )
        .collect();
    if ordered.is_empty() {
        return Err(LauncherError::AllDisabled {
            components: disabled,
        });
    }
    let chosen = ordered[0];
    let target_decl = chosen
        .target
        .as_deref()
        .and_then(|t| decls.iter().find(|d| !d.alias && d.name == t));
    let label = if chosen.label.is_unset() {
        target_decl.map(|d| d.label.clone()).unwrap_or_default()
    } else {
        chosen.label.clone()
    };
    let icon_ref = chosen.icon.or_else(|| target_decl.and_then(|d| d.icon));
    // `parseActivityAlias` copies the target's `configChanges`; an alias has no
    // `android:configChanges` attribute of its own in the manifest schema.
    let declared = chosen
        .config_changes
        .or_else(|| target_decl.and_then(|d| d.config_changes))
        .unwrap_or(0);
    Ok(LauncherActivity {
        component: chosen.name.clone(),
        target: chosen.target.clone(),
        label,
        icon_ref,
        alias: chosen.alias,
        config_changes: get_activity_config_changes(declared),
        candidates: ordered.into_iter().map(|d| d.name.clone()).collect(),
    })
}

fn parse_component(node: &XmlNode, package: &str, alias: bool) -> Option<Decl> {
    let raw = attr_string(node, "name")?;
    if raw.is_empty() {
        return None;
    }
    let name = acos_apk::manifest::fq_name(package, &raw);
    let target = if alias {
        attr_string(node, "targetActivity")
            .filter(|t| !t.is_empty())
            .map(|t| acos_apk::manifest::fq_name(package, &t))
    } else {
        None
    };
    let (literal, res) = label_of(node);
    let launcher = node.children_named("intent-filter").any(|f| {
        let action = f
            .children_named("action")
            .filter_map(|a| attr_string(a, "name"))
            .any(|a| a == "android.intent.action.MAIN");
        let category = f
            .children_named("category")
            .filter_map(|c| attr_string(c, "name"))
            .any(|c| c == "android.intent.category.LAUNCHER");
        action && category
    });
    // `parseActivityAlias` does not read `configChanges` for an alias.
    let config_changes = if alias { None } else { attr_int(node, "configChanges") };
    Some(Decl {
        name,
        target,
        alias,
        enabled: attr_bool(node, "enabled"),
        label: LabelSpec::from_parts(literal.as_ref(), res),
        icon: attr_ref(node, "icon"),
        launcher,
        config_changes,
    })
}

// ---------------------------------------------------------- launch budgets

/// Effective resume budget for a launch (W27 item 2).
///
/// The one-shot/interactive guest stops waiting for `Activity.mResumed` after
/// [`RESUME_MS_BASE`]. A multidex app or one with a large native closure
/// legitimately spends longer than 12 s in bootstrap (Gaten debug: 31 dex,
/// 108 MiB of `.so`, ~12 s of framework work before RN even mounts), so those
/// APKs get [`RESUME_MS_LARGE`]. `ACOSLAUNCH_RESUME_MS` always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchBudget {
    pub dex_count: usize,
    pub native_bytes: u64,
    pub resume_ms: u64,
}

/// Default `Activity.mResumed` wait for an ordinary app (unchanged since W5).
pub const RESUME_MS_BASE: u64 = 12_000;
/// Default wait for a large app (multidex or a large native closure).
pub const RESUME_MS_LARGE: u64 = 45_000;
/// At or above this dex count the app is treated as large (Gaten debug: 31).
pub const LARGE_DEX_COUNT: usize = 8;
/// At or above this native-library total the app is treated as large
/// (Gaten debug: ~108 MiB).
pub const LARGE_NATIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Read the APK shape and pick the documented default resume budget.
///
/// Served from [`crate::apk_facts`] (see [`launcher_activity`]).
pub fn launch_budget(apk: impl AsRef<Path>) -> Result<LaunchBudget, LauncherError> {
    let path = apk.as_ref();
    let facts = crate::apk_facts::launch_facts(path)
        .map_err(|e| LauncherError::Manifest(format!("{}: {e}", path.display())))?;
    Ok(facts.budget)
}

/// The budget of an already-opened APK (dex count and native closure size).
pub fn budget_of(apk: &acos_apk::Apk) -> LaunchBudget {
    let dex_count = apk.dex_files.len();
    let native_bytes: u64 = apk
        .native_libs
        .values()
        .flat_map(|libs| libs.iter())
        .map(|lib| lib.size)
        .sum();
    let large = dex_count >= LARGE_DEX_COUNT || native_bytes >= LARGE_NATIVE_BYTES;
    LaunchBudget {
        dex_count,
        native_bytes,
        resume_ms: if large {
            RESUME_MS_LARGE
        } else {
            RESUME_MS_BASE
        },
    }
}

// ------------------------------------------------------------------ helpers

fn attr<'a>(node: &'a XmlNode, name: &str) -> Option<&'a TypedValue> {
    node.attribute(name).map(|a| &a.value)
}

fn attr_string(node: &XmlNode, name: &str) -> Option<String> {
    match attr(node, name)? {
        TypedValue::String(s) => Some(s.clone()),
        TypedValue::IntDec(i) => Some(i.to_string()),
        TypedValue::IntHex(h) => Some(format!("{h:#x}")),
        TypedValue::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn attr_bool(node: &XmlNode, name: &str) -> Option<bool> {
    match attr(node, name)? {
        TypedValue::Boolean(b) => Some(*b),
        TypedValue::IntDec(i) => Some(*i != 0),
        _ => None,
    }
}

fn attr_int(node: &XmlNode, name: &str) -> Option<i32> {
    match attr(node, name)? {
        TypedValue::IntDec(i) => Some(*i),
        TypedValue::IntHex(h) => Some(*h as i32),
        TypedValue::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn attr_ref(node: &XmlNode, name: &str) -> Option<u32> {
    match attr(node, name)? {
        TypedValue::Reference(r) => Some(*r),
        _ => None,
    }
}

/// `ActivityInfo.CONFIG_MCC` — one of the two bits
/// `RECREATE_ON_CONFIG_CHANGES_MASK` names.
pub const CONFIG_MCC: i32 = 0x0000_0001;
/// `ActivityInfo.CONFIG_MNC`.
pub const CONFIG_MNC: i32 = 0x0000_0002;

/// `PackageParser.getActivityConfigChanges(configChanges, 0)`: the declared
/// bits **or** `(~recreateOnConfigChanges) & RECREATE_ON_CONFIG_CHANGES_MASK`.
/// The fixture APKs declare no `android:recreateOnConfigChanges`, so the forced
/// bits are always `CONFIG_MCC|CONFIG_MNC`; they do not intersect the rotation
/// bits, so they never change the relaunch decision — they are kept because the
/// result is published as `ActivityInfo.configChanges`.
#[must_use]
pub fn get_activity_config_changes(declared: i32) -> i32 {
    declared | (CONFIG_MCC | CONFIG_MNC)
}

fn label_of(node: &XmlNode) -> (Option<String>, Option<u32>) {
    match attr(node, "label") {
        Some(TypedValue::String(s)) => (Some(s.clone()), None),
        Some(TypedValue::Reference(r)) => (None, Some(*r)),
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr_node(name: &str, attrs: &[(&str, TypedValue)], children: Vec<XmlNode>) -> XmlNode {
        XmlNode {
            namespace: None,
            name: name.to_string(),
            line: 0,
            attributes: attrs
                .iter()
                .map(|(n, v)| acos_apk::axml::XmlAttribute {
                    namespace: None,
                    name: (*n).to_string(),
                    raw_value: None,
                    value: v.clone(),
                    resource_id: None,
                    name_index: 0,
                })
                .collect(),
            children,
        }
    }

    fn s(v: &str) -> TypedValue {
        TypedValue::String(v.to_string())
    }
    fn b(v: bool) -> TypedValue {
        TypedValue::Boolean(v)
    }
    fn i(v: i32) -> TypedValue {
        TypedValue::IntDec(v)
    }
    fn r(v: u32) -> TypedValue {
        TypedValue::Reference(v)
    }

    fn filter() -> XmlNode {
        attr_node(
            "intent-filter",
            &[],
            vec![
                attr_node(
                    "action",
                    &[("name", s("android.intent.action.MAIN"))],
                    vec![],
                ),
                attr_node(
                    "category",
                    &[("name", s("android.intent.category.LAUNCHER"))],
                    vec![],
                ),
            ],
        )
    }

    fn dom(children: Vec<XmlNode>) -> XmlDocument {
        XmlDocument {
            strings: Vec::new(),
            resource_ids: Vec::new(),
            root: attr_node(
                "manifest",
                &[("package", s("dev.acos.test"))],
                vec![attr_node("application", &[], children)],
            ),
        }
    }

    #[test]
    fn alias_resolves_to_its_target_and_keeps_the_alias_component() {
        let doc = dom(vec![
            attr_node("activity", &[("name", s(".Main"))], vec![]),
            attr_node(
                "activity-alias",
                &[
                    ("name", s(".Light")),
                    ("targetActivity", s(".Main")),
                    ("enabled", b(true)),
                ],
                vec![filter()],
            ),
        ]);
        let got = resolve_from_dom(&doc).expect("alias is the launcher");
        assert_eq!(got.component, "dev.acos.test.Light");
        assert_eq!(got.target.as_deref(), Some("dev.acos.test.Main"));
        assert!(got.alias);
        assert_eq!(got.candidates, vec!["dev.acos.test.Light"]);
    }

    #[test]
    fn alias_label_overrides_target_label_and_falls_back_to_it() {
        let target = attr_node(
            "activity",
            &[("name", s(".Main")), ("label", s("Target title"))],
            vec![],
        );
        let labelled = attr_node(
            "activity-alias",
            &[
                ("name", s(".Alias")),
                ("targetActivity", s(".Main")),
                ("label", r(0x7f01_0001)),
            ],
            vec![filter()],
        );
        let got = resolve_from_dom(&dom(vec![target.clone(), labelled])).unwrap();
        assert_eq!(
            got.label,
            LabelSpec {
                res: 0x7f01_0001,
                literal: None
            }
        );
        let unlabelled = attr_node(
            "activity-alias",
            &[("name", s(".Alias")), ("targetActivity", s(".Main"))],
            vec![filter()],
        );
        let got = resolve_from_dom(&dom(vec![target, unlabelled])).unwrap();
        assert_eq!(
            got.label,
            LabelSpec {
                res: 0,
                literal: Some("Target title".into())
            }
        );
    }

    #[test]
    fn disabled_alias_is_skipped_and_the_enabled_one_wins() {
        // Gaten's shape: one enabled alias among two disabled ones.
        let doc = dom(vec![
            attr_node("activity", &[("name", s(".Launcher"))], vec![]),
            attr_node(
                "activity-alias",
                &[
                    ("name", s(".System")),
                    ("targetActivity", s(".Launcher")),
                    ("enabled", b(false)),
                ],
                vec![filter()],
            ),
            attr_node(
                "activity-alias",
                &[
                    ("name", s(".Light")),
                    ("targetActivity", s(".Launcher")),
                    ("enabled", b(true)),
                ],
                vec![filter()],
            ),
            attr_node(
                "activity-alias",
                &[
                    ("name", s(".Dark")),
                    ("targetActivity", s(".Launcher")),
                    ("enabled", b(false)),
                ],
                vec![filter()],
            ),
        ]);
        let got = resolve_from_dom(&doc).unwrap();
        assert_eq!(got.component, "dev.acos.test.Light");
        assert_eq!(got.candidates, vec!["dev.acos.test.Light"]);
    }

    #[test]
    fn only_disabled_launchers_is_a_structured_disabled_error() {
        let doc = dom(vec![attr_node(
            "activity",
            &[("name", s(".Main")), ("enabled", b(false))],
            vec![filter()],
        )]);
        let err = resolve_from_dom(&doc).unwrap_err();
        assert_eq!(err.code(), "LAUNCHER_DISABLED");
        assert!(err.to_string().contains("dev.acos.test.Main"));
    }

    #[test]
    fn alias_to_missing_target_and_alias_without_target_are_structured_errors() {
        let missing = dom(vec![attr_node(
            "activity-alias",
            &[("name", s(".Alias")), ("targetActivity", s(".Gone"))],
            vec![filter()],
        )]);
        let err = resolve_from_dom(&missing).unwrap_err();
        assert_eq!(err.code(), "LAUNCHER_ALIAS_TARGET_MISSING");
        assert!(err.to_string().contains("dev.acos.test.Gone"));

        let no_target = dom(vec![attr_node(
            "activity-alias",
            &[("name", s(".Alias"))],
            vec![filter()],
        )]);
        let err = resolve_from_dom(&no_target).unwrap_err();
        assert_eq!(err.code(), "LAUNCHER_ALIAS_NO_TARGET");
    }

    #[test]
    fn no_launcher_filter_is_a_structured_no_launcher_error() {
        let doc = dom(vec![
            attr_node("activity", &[("name", s(".Main"))], vec![]),
            attr_node(
                "activity",
                &[("name", s(".Detail"))],
                vec![attr_node(
                    "intent-filter",
                    &[],
                    vec![attr_node(
                        "action",
                        &[("name", s("android.intent.action.VIEW"))],
                        vec![],
                    )],
                )],
            ),
        ]);
        let err = resolve_from_dom(&doc).unwrap_err();
        assert_eq!(err.code(), "LAUNCHER_NONE");
    }

    #[test]
    fn multiple_launchers_choose_aliases_first_then_declaration_order() {
        let doc = dom(vec![
            attr_node("activity", &[("name", s(".Default"))], vec![filter()]),
            attr_node(
                "activity-alias",
                &[("name", s(".A")), ("targetActivity", s(".Default"))],
                vec![filter()],
            ),
            attr_node(
                "activity-alias",
                &[("name", s(".B")), ("targetActivity", s(".Default"))],
                vec![filter()],
            ),
        ]);
        let got = resolve_from_dom(&doc).unwrap();
        assert_eq!(got.component, "dev.acos.test.A");
        assert_eq!(
            got.candidates,
            vec![
                "dev.acos.test.A",
                "dev.acos.test.B",
                "dev.acos.test.Default"
            ]
        );
    }

    #[test]
    fn application_enabled_false_disables_every_component() {
        let doc = XmlDocument {
            strings: Vec::new(),
            resource_ids: Vec::new(),
            root: attr_node(
                "manifest",
                &[("package", s("dev.acos.test"))],
                vec![attr_node(
                    "application",
                    &[("enabled", b(false))],
                    vec![attr_node(
                        "activity",
                        &[("name", s(".Main"))],
                        vec![filter()],
                    )],
                )],
            ),
        };
        let err = resolve_from_dom(&doc).unwrap_err();
        assert_eq!(err.code(), "LAUNCHER_DISABLED");
    }

    #[test]
    fn real_apks_resolve_like_aapt2_could_not() {
        // Paths are relative to the workspace root (`CARGO_MANIFEST_DIR/../..`),
        // not the crate directory: `cargo test` runs with the crate as cwd.
        fn repo_path(rel: &str) -> std::path::PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
        }

        // Built fixture: plain `<activity>`. REQUIRED: `fixtures/build` is
        // gitignored, so a checkout without it must fail loudly, never skip.
        let counter = repo_path("fixtures/build/apps/counter.apk");
        assert!(
            counter.is_file(),
            "required fixture missing: {} (fixtures/build is not in git; build the fixtures)",
            counter.display()
        );
        let got = launcher_activity(&counter).unwrap();
        assert_eq!(got.component, "dev.acos.fixtures.counter.MainActivity");
        assert_eq!(got.target, None);
        assert!(!got.alias);
        let budget = launch_budget(&counter).unwrap();
        assert_eq!(budget.resume_ms, RESUME_MS_BASE);

        // The real Gaten release APK (`.cache/gaten`, read-only copy): the
        // launcher is `LauncherIconLight`, an alias to `LauncherActivity`, and
        // aapt2 badging says "no launchable activity". OPTIONAL: the APK is
        // not in git, so its absence is a documented environment gap. The skip
        // is printed with its reason (run with --nocapture to see it).
        let gaten = repo_path(".cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk");
        if !gaten.is_file() {
            eprintln!(
                "SKIP real_apks_resolve_like_aapt2_could_not (Gaten half): {} is absent; \
                 documented environment gap (the APK is not in git)",
                gaten.display()
            );
            return;
        }
        let got = launcher_activity(&gaten).unwrap();
        assert_eq!(got.component, "com.gaten.app.LauncherIconLight");
        assert_eq!(
            got.target.as_deref(),
            Some("com.gaten.app.LauncherActivity")
        );
        assert!(got.alias);
        // The release build has one classes.dex (the 31-dex shape belongs to the
        // debug build, gaten-dev-9feffd2), so it is large by its native closure
        // alone: 23 .so files, ~70 MB >= LARGE_NATIVE_BYTES (64 MiB).
        let budget = launch_budget(&gaten).unwrap();
        assert_eq!(budget.dex_count, 1, "release APK: one classes.dex");
        assert!(budget.dex_count < LARGE_DEX_COUNT);
        assert!(budget.native_bytes >= LARGE_NATIVE_BYTES, "~70 MB of .so");
        assert_eq!(budget.resume_ms, RESUME_MS_LARGE);
    }

    #[test]
    fn config_changes_are_parsed_from_the_binary_manifest_int_bits() {
        // `android:configChanges="orientation|screenSize"` is compiled to one
        // `int` bitmask (0x80|0x400); PackageParser reads it with
        // `sa.getInt(R.styleable.AndroidManifestActivity_configChanges, 0)`.
        let doc = dom(vec![attr_node(
            "activity",
            &[
                ("name", s(".Main")),
                ("configChanges", i(0x80 | 0x400)),
            ],
            vec![filter()],
        )]);
        let got = resolve_from_dom(&doc).unwrap();
        // `getActivityConfigChanges` forces CONFIG_MCC|CONFIG_MNC on top.
        assert_eq!(got.config_changes, 0x480 | 0x3);
        // The rotation decision AOSP derives from it
        // (`ActivityRecord.getRealConfigChanged`): two of the four rotation bits
        // are missing, so this Activity is re-created on a rotation.
        let decision = acos_wm::RotationRelaunch::for_rotation(got.config_changes);
        assert_eq!(
            decision.missing(),
            acos_wm::display::CONFIG_SCREEN_LAYOUT | acos_wm::display::CONFIG_SMALLEST_SCREEN_SIZE
        );
        assert!(decision.relaunches());
    }

    #[test]
    fn all_four_rotation_bits_keep_the_activity_and_no_declaration_relaunches() {
        let handled = dom(vec![attr_node(
            "activity",
            &[
                ("name", s(".Main")),
                ("configChanges", i(0x80 | 0x400 | 0x100 | 0x800)),
            ],
            vec![filter()],
        )]);
        let got = resolve_from_dom(&handled).unwrap();
        assert_eq!(got.config_changes & 0xD80, 0xD80);
        assert!(!acos_wm::RotationRelaunch::for_rotation(got.config_changes).relaunches());

        // Every fixture APK declares nothing: 0x0 (plus the forced MCC/MNC).
        let none = dom(vec![attr_node(
            "activity",
            &[("name", s(".Main"))],
            vec![filter()],
        )]);
        let got = resolve_from_dom(&none).unwrap();
        assert_eq!(got.config_changes, 0x3);
        assert!(acos_wm::RotationRelaunch::for_rotation(got.config_changes).relaunches());
    }

    #[test]
    fn activity_alias_inherits_the_targets_config_changes() {
        // `PackageParser.parseActivityAlias`: `info.configChanges =
        // target.info.configChanges`; an alias has no attribute of its own.
        let doc = dom(vec![
            attr_node(
                "activity",
                &[("name", s(".Main")), ("configChanges", i(0xD80))],
                vec![],
            ),
            attr_node(
                "activity-alias",
                &[("name", s(".Alias")), ("targetActivity", s(".Main"))],
                vec![filter()],
            ),
        ]);
        let got = resolve_from_dom(&doc).unwrap();
        assert!(got.alias);
        assert_eq!(got.config_changes & 0xD80, 0xD80);
        assert!(!acos_wm::RotationRelaunch::for_rotation(got.config_changes).relaunches());
    }

    #[test]
    fn budget_thresholds_are_documented_and_env_boundary_is_explicit() {
        // Pure shape arithmetic (the APK reader is exercised by the real-APK
        // test below when fixtures are built).
        assert_eq!(RESUME_MS_BASE, 12_000);
        assert_eq!(RESUME_MS_LARGE, 45_000);
        assert!(LARGE_DEX_COUNT == 8);
        assert!(LARGE_NATIVE_BYTES == 64 * 1024 * 1024);
    }
}
