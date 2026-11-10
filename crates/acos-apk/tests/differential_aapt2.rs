//! Differential comparison against the Android SDK `aapt2`.
//!
//! For every fixture APK (and the read-only legacy APKs, when present) this
//! compares the ACOS manifest model against `aapt2 dump badging`, and the AXML
//! DOM against `aapt2 dump xmltree --file AndroidManifest.xml`.
//!
//! The test is env-gated: if `aapt2` cannot be found the test prints a skip
//! notice and passes, so `cargo test` works without an SDK.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_apk::{Apk, XmlNode};

fn find_aapt2() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(var) {
            roots.push(PathBuf::from(v));
        }
    }
    roots.push(PathBuf::from("/home/Qui/Android/Sdk"));
    for root in roots {
        let bt = root.join("build-tools");
        let Ok(entries) = std::fs::read_dir(&bt) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs.into_iter().rev() {
            let candidate = dir.join("aapt2");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn fixture_apks() -> Vec<PathBuf> {
    let mut apks = Vec::new();
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    if let Ok(entries) = std::fs::read_dir(&data) {
        let mut v: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "apk"))
            .collect();
        v.sort();
        apks.extend(v);
    }
    // Read-only legacy APKs. Never written to.
    let legacy = Path::new("/home/Qui/Work/android-container/helper/build");
    if let Ok(entries) = std::fs::read_dir(legacy) {
        let mut v: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "apk"))
            .collect();
        v.sort();
        apks.extend(v);
    }
    apks
}

fn run_aapt2(aapt2: &Path, args: &[&str], apk: &Path) -> Option<String> {
    let out = Command::new(aapt2).args(args).arg(apk).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn quoted(line: &str, key: &str) -> Option<String> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    let q1 = rest.find('\'')?;
    let rest = &rest[q1 + 1..];
    let q2 = rest.find('\'')?;
    Some(rest[..q2].to_string())
}

fn all_quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(q1) = rest.find('\'') {
        rest = &rest[q1 + 1..];
        if let Some(q2) = rest.find('\'') {
            out.push(rest[..q2].to_string());
            rest = &rest[q2 + 1..];
        } else {
            break;
        }
    }
    out
}

fn badging_expectations(badging: &str) -> Badging {
    let mut b = Badging::default();
    for line in badging.lines() {
        if let Some(rest) = line.strip_prefix("package: ") {
            b.package = quoted(rest, "name");
            b.version_code = quoted(rest, "versionCode").filter(|s| !s.is_empty());
            b.version_name = quoted(rest, "versionName").filter(|s| !s.is_empty());
        } else if line.starts_with("uses-permission:") {
            if let Some(v) = quoted(line, "name") {
                b.permissions.push(v);
            }
        } else if line.starts_with("minSdkVersion:") {
            b.min_sdk = quoted(line, "minSdkVersion");
        } else if line.starts_with("targetSdkVersion:") {
            b.target_sdk = quoted(line, "targetSdkVersion");
        } else if line.starts_with("maxSdkVersion:") {
            b.max_sdk = quoted(line, "maxSdkVersion");
        } else if line.starts_with("application-label:") {
            b.label = quoted(line, "application-label");
        } else if line.starts_with("application:") {
            // Literal (non-resource) labels are reported on the `application:`
            // line instead of `application-label:`.
            if b.label.is_none() {
                b.label = quoted(line, "label").filter(|s| !s.is_empty());
            }
        } else if line.starts_with("launchable-activity:") {
            b.launcher = quoted(line, "name");
        } else if line.starts_with("native-code:") {
            b.native_code = all_quoted(line);
        }
    }
    b.permissions.sort();
    b
}

#[derive(Default, Debug)]
struct Badging {
    package: Option<String>,
    version_code: Option<String>,
    version_name: Option<String>,
    permissions: Vec<String>,
    min_sdk: Option<String>,
    target_sdk: Option<String>,
    max_sdk: Option<String>,
    label: Option<String>,
    launcher: Option<String>,
    native_code: Vec<String>,
}

fn check_eq<T: PartialEq + std::fmt::Debug>(
    apk: &Path,
    name: &str,
    expected: Option<T>,
    actual: Option<T>,
    mismatches: &mut Vec<String>,
) {
    if expected != actual {
        mismatches.push(format!(
            "{}: {name}: expected {:?}, got {:?}",
            apk.display(),
            expected,
            actual
        ));
    }
}

fn compare_badging(apk: &Path, badging: &str, mismatches: &mut Vec<String>) {
    let b = badging_expectations(badging);
    let parsed = match Apk::open(apk) {
        Ok(a) => a,
        Err(e) => {
            mismatches.push(format!("{}: APK open failed: {}", apk.display(), e));
            return;
        }
    };
    let manifest = match parsed.manifest() {
        Ok(m) => m,
        Err(e) => {
            mismatches.push(format!("{}: manifest parse failed: {}", apk.display(), e));
            return;
        }
    };
    let res = parsed.resources().ok().flatten();

    check_eq(
        apk,
        "package",
        b.package.clone(),
        Some(manifest.package.clone()),
        mismatches,
    );

    let actual_version_code = manifest.version_code.map(|v| v.to_string());
    check_eq(
        apk,
        "versionCode",
        b.version_code.clone(),
        actual_version_code,
        mismatches,
    );

    check_eq(
        apk,
        "versionName",
        b.version_name.clone(),
        manifest.version_name.clone(),
        mismatches,
    );

    check_eq(
        apk,
        "minSdkVersion",
        b.min_sdk.as_ref().and_then(|s| s.parse::<u32>().ok()),
        manifest.min_sdk,
        mismatches,
    );
    check_eq(
        apk,
        "targetSdkVersion",
        b.target_sdk.as_ref().and_then(|s| s.parse::<u32>().ok()),
        manifest.target_sdk,
        mismatches,
    );
    if let Some(max) = b.max_sdk.as_ref().and_then(|s| s.parse::<u32>().ok()) {
        check_eq(
            apk,
            "maxSdkVersion",
            Some(max),
            manifest.max_sdk,
            mismatches,
        );
    }

    let mut actual_perms: Vec<String> = manifest
        .uses_permissions
        .iter()
        .map(|p| p.name.clone())
        .collect();
    actual_perms.sort();
    if actual_perms != b.permissions {
        mismatches.push(format!(
            "{}: permissions: expected {:?}, got {:?}",
            apk.display(),
            b.permissions,
            actual_perms
        ));
    }

    check_eq(
        apk,
        "application-label",
        b.label.clone(),
        manifest.app_label(res.as_ref()),
        mismatches,
    );

    check_eq(
        apk,
        "launchable-activity",
        b.launcher.clone(),
        manifest.launcher_activity().map(|c| c.name.clone()),
        mismatches,
    );

    let mut actual_abis = parsed.native_abis();
    actual_abis.sort();
    if !b.native_code.is_empty() {
        let mut expected = b.native_code.clone();
        expected.sort();
        if actual_abis != expected {
            mismatches.push(format!(
                "{}: native-code: expected {:?}, got {:?}",
                apk.display(),
                expected,
                actual_abis
            ));
        }
    }
}

// --- xmltree comparison -----------------------------------------------------

#[derive(Default)]
struct TNode {
    name: String,
    attrs: Vec<String>,
    children: Vec<TNode>,
}

fn parse_xmltree(text: &str) -> Option<TNode> {
    let mut root: Option<TNode> = None;
    let mut stack: Vec<(usize, TNode)> = Vec::new();
    let mut root_depth = None;

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();
        let depth = indent / 2;
        if trimmed.is_empty() || trimmed.starts_with("N:") {
            continue;
        }
        if let Some(name_part) = trimmed.strip_prefix("E: ") {
            let name = name_part
                .split(" (line=")
                .next()
                .unwrap_or(name_part)
                .trim()
                .to_string();
            if root_depth.is_none() {
                root_depth = Some(depth);
            }
            let node = TNode {
                name,
                ..Default::default()
            };
            // Attach to the current parent if any, else this is the root.
            while let Some((d, _)) = stack.last() {
                if *d >= depth {
                    let (_, finished) = stack.pop().unwrap();
                    attach(&mut stack, &mut root, finished);
                } else {
                    break;
                }
            }
            stack.push((depth, node));
        } else if let Some(attr_part) = trimmed.strip_prefix("A: ") {
            if let Some(attr) = normalize_aapt2_attr(attr_part, root_depth.map_or(0, |r| depth - r))
            {
                if let Some((_, node)) = stack.last_mut() {
                    node.attrs.push(attr);
                }
            }
        }
    }
    while let Some((_, finished)) = stack.pop() {
        attach(&mut stack, &mut root, finished);
    }
    root
}

fn attach(stack: &mut [(usize, TNode)], root: &mut Option<TNode>, node: TNode) {
    if let Some((_, parent)) = stack.last_mut() {
        parent.children.push(node);
    } else if root.is_none() {
        *root = Some(node);
    } else if let Some(existing) = root.as_mut() {
        // Multiple roots: attach as sibling (should not happen).
        existing.children.push(node);
    }
}

fn normalize_aapt2_attr(part: &str, _depth: usize) -> Option<String> {
    // Strip a trailing " (Raw: ...)".
    let part = match part.rfind(" (Raw:") {
        Some(idx) => &part[..idx],
        None => part,
    };
    let (left, value) = match part.split_once('=') {
        Some((l, v)) => (l, v),
        None => (part, ""),
    };
    let mut ns = String::new();
    let mut name = left.to_string();
    let mut id = String::new();
    if let Some(open) = left.find("(0x") {
        name = left[..open].to_string();
        if let Some(close) = left[open..].find(')') {
            id = left[open + 3..open + close].to_string();
        }
        if let Some((n, local)) = name.rsplit_once(':') {
            ns = n.to_string();
            name = local.to_string();
        }
    }
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
        .to_string();
    Some(format!("{ns}|{name}|{id}|{value}"))
}

fn canonical_dom(node: &XmlNode, depth: usize, out: &mut String) {
    out.push_str(&format!("{depth} {}\n", node.name));
    let mut attrs: Vec<String> = node
        .attributes
        .iter()
        .map(|a| {
            format!(
                "{} {}|{}|{}|{}",
                depth + 1,
                a.namespace.clone().unwrap_or_default(),
                a.name,
                a.resource_id
                    .map(|i| format!("{i:08x}"))
                    .unwrap_or_default(),
                a.value.display()
            )
        })
        .collect();
    attrs.sort();
    for a in attrs {
        out.push_str(&a);
        out.push('\n');
    }
    for child in &node.children {
        canonical_dom(child, depth + 1, out);
    }
}

fn canonical_tree(node: &TNode, depth: usize, out: &mut String) {
    out.push_str(&format!("{depth} {}\n", node.name));
    let mut attrs = node.attrs.clone();
    attrs.sort();
    for a in attrs {
        out.push_str(&format!("{} {}\n", depth + 1, a));
    }
    for child in &node.children {
        canonical_tree(child, depth + 1, out);
    }
}

fn compare_xmltree(apk: &Path, text: &str, mismatches: &mut Vec<String>) {
    let parsed = match Apk::open(apk) {
        Ok(a) => a,
        Err(e) => {
            mismatches.push(format!("{}: APK open failed: {}", apk.display(), e));
            return;
        }
    };
    let dom = match parsed.manifest_dom() {
        Ok(d) => d,
        Err(e) => {
            mismatches.push(format!(
                "{}: manifest DOM parse failed: {}",
                apk.display(),
                e
            ));
            return;
        }
    };
    let Some(expected) = parse_xmltree(text) else {
        mismatches.push(format!("{}: could not parse aapt2 xmltree", apk.display()));
        return;
    };
    let mut a = String::new();
    canonical_dom(&dom.root, 0, &mut a);
    let mut b = String::new();
    canonical_tree(&expected, 0, &mut b);
    if a != b {
        mismatches.push(format!(
            "{}: xmltree mismatch\n--- aapt2 ---\n{b}\n--- acos ---\n{a}",
            apk.display()
        ));
    }
}

#[test]
fn differential_against_aapt2() {
    let Some(aapt2) = find_aapt2() else {
        eprintln!("differential_aapt2: SKIP (aapt2 not found)");
        return;
    };
    let apks = fixture_apks();
    assert!(!apks.is_empty(), "no APKs discovered for differential test");

    let mut mismatches = Vec::new();
    let mut compared = 0;
    for apk in &apks {
        let Some(badging) = run_aapt2(&aapt2, &["dump", "badging"], apk) else {
            mismatches.push(format!("{}: aapt2 dump badging failed", apk.display()));
            continue;
        };
        compare_badging(apk, &badging, &mut mismatches);

        if let Some(xmltree) = run_aapt2(
            &aapt2,
            &["dump", "xmltree", "--file", "AndroidManifest.xml"],
            apk,
        ) {
            compare_xmltree(apk, &xmltree, &mut mismatches);
        } else {
            mismatches.push(format!("{}: aapt2 dump xmltree failed", apk.display()));
        }
        compared += 1;
    }

    eprintln!("differential_aapt2: compared {compared} APK(s)");
    assert!(
        mismatches.is_empty(),
        "aapt2 differential mismatches ({}):\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}
