#![allow(clippy::field_reassign_with_default)]
//! Differential test: the engine's ARSC parsing and configuration matching
//! against the platform `aapt` tool (`aapt dump --values resources`).
//!
//! For a deterministic sample of resource ids across types we compare
//! (a) the multiset of raw `(type, data)` values across all configurations,
//! (b) bag parent + key sets, (c) the configuration label set, and
//! (d) the value resolved for the device configuration against the aapt block
//! with the same label. Zero mismatches is the acceptance bar.
//!
//! The test skips (loudly) when the `aapt` binary or the fixture APK is absent.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;

use acos_framework_natives::res::{ApkAssets, AssetSource, ResConfig};

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn find_aapt() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AAPT") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let base = dirs_home()?.join("Android/Sdk/build-tools");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    versions.sort();
    for v in versions.into_iter().rev() {
        let candidate = v.join("aapt");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AaptValue {
    simple: Option<(u8, u32)>,
    bag: Option<(u32, Vec<u32>)>,
}

#[derive(Debug, Default, Clone)]
struct AaptResource {
    /// label -> value
    by_label: BTreeMap<String, AaptValue>,
}

fn flush_bag(
    out: &mut HashMap<u32, AaptResource>,
    pending: &mut Option<(u32, AaptValue)>,
    label: &str,
) {
    if let Some((id, value)) = pending.take() {
        out.entry(id)
            .or_default()
            .by_label
            .insert(label.to_string(), value);
    }
}

fn parse_aapt(output: &str) -> HashMap<u32, AaptResource> {
    let mut out: HashMap<u32, AaptResource> = HashMap::new();
    let mut current_label = String::new();
    let mut pending: Option<(u32, AaptValue)> = None;
    for raw in output.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("      config ") {
            flush_bag(&mut out, &mut pending, &current_label);
            let label = rest
                .split(" flags=")
                .next()
                .unwrap_or(rest)
                .trim_end_matches(':')
                .to_string();
            current_label = label;
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("resource 0x") {
            flush_bag(&mut out, &mut pending, &current_label);
            let Some(id_hex) = rest.get(..8) else {
                continue;
            };
            let Ok(id) = u32::from_str_radix(id_hex, 16) else {
                continue;
            };
            if rest.contains("<bag>") {
                pending = Some((
                    id,
                    AaptValue {
                        simple: None,
                        bag: Some((0, Vec::new())),
                    },
                ));
            } else if let (Some(t), Some(d)) =
                (find_hex_after(rest, "t=0x"), find_hex_after(rest, "d=0x"))
            {
                out.entry(id).or_default().by_label.insert(
                    current_label.clone(),
                    AaptValue {
                        simple: Some((t as u8, d)),
                        bag: None,
                    },
                );
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Parent=0x") {
            if let Some((_, value)) = &mut pending {
                if let Some(bag) = &mut value.bag {
                    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                    if let Ok(p) = u32::from_str_radix(&hex, 16) {
                        bag.0 = p;
                    }
                }
            }
            continue;
        }
        if trimmed.starts_with('#') {
            if let Some(key) = find_hex_after(trimmed, "Key=0x") {
                if let Some((_, value)) = &mut pending {
                    if let Some(bag) = &mut value.bag {
                        bag.1.push(key);
                    }
                }
            }
        }
    }
    flush_bag(&mut out, &mut pending, &current_label);
    out
}

fn find_hex_after(s: &str, needle: &str) -> Option<u32> {
    let idx = s.find(needle)? + needle.len();
    let rest = &s[idx..];
    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u32::from_str_radix(&hex, 16).ok()
}

#[test]
fn differential_framework_res_values() {
    let Some(aapt) = find_aapt() else {
        eprintln!("aapt not found; skipping differential test");
        return;
    };
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let (checked, v, l, b) = differential_apk(&aapt, &apk_path, 120, 500);
    eprintln!(
        "DIFFERENTIAL framework-res: checked={} value_mismatches={} label_mismatches={} bag_mismatches={}",
        checked, v, l, b
    );
    assert!(
        checked >= 500,
        "expected at least 500 sampled ids, got {checked}"
    );
    assert_eq!(v, 0, "value mismatches");
    assert_eq!(l, 0, "config label mismatches");
    assert_eq!(b, 0, "bag structural mismatches");
}

#[test]
fn differential_app_values() {
    let Some(aapt) = find_aapt() else {
        eprintln!("aapt not found; skipping differential test");
        return;
    };
    if !acos_test_support::have_fixture("fixtures/build/apps/hello-views.apk") {
        eprintln!("hello-views.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join("fixtures/build/apps/hello-views.apk");
    let (checked, v, l, b) = differential_apk(&aapt, &apk_path, usize::MAX, 1);
    eprintln!(
        "DIFFERENTIAL hello-views: checked={} value_mismatches={} label_mismatches={} bag_mismatches={}",
        checked, v, l, b
    );
    assert!(
        checked >= 1,
        "expected at least one resource, got {checked}"
    );
    assert_eq!(v, 0, "value mismatches");
    assert_eq!(l, 0, "config label mismatches");
    assert_eq!(b, 0, "bag structural mismatches");
}

/// Returns `(checked, value_mismatches, label_mismatches, bag_mismatches)`.
fn differential_apk(
    aapt_bin: &PathBuf,
    apk_path: &PathBuf,
    per_type: usize,
    min_checked: usize,
) -> (usize, usize, usize, usize) {
    let output = Command::new(aapt_bin)
        .args(["dump", "--values", "resources"])
        .arg(apk_path)
        .output()
        .expect("run aapt");
    assert!(output.status.success(), "aapt failed");
    let text = String::from_utf8_lossy(&output.stdout);
    let aapt = parse_aapt(&text);
    eprintln!("aapt listed {} resource ids", aapt.len());

    let apk = ApkAssets::open(apk_path).unwrap();
    let table = apk.resource_table().unwrap();
    let bytes = apk.resource_bytes().unwrap();

    let mut by_type: BTreeMap<u8, Vec<u32>> = BTreeMap::new();
    for &id in aapt.keys() {
        by_type
            .entry(((id >> 16) & 0xff) as u8)
            .or_default()
            .push(id);
    }
    let mut sample: Vec<u32> = Vec::new();
    for ids in by_type.values_mut() {
        ids.sort_unstable();
        if per_type == usize::MAX {
            sample.extend(ids.iter().copied());
        } else {
            sample.extend(
                ids.iter()
                    .step_by((ids.len() / per_type).max(1))
                    .take(per_type),
            );
        }
    }
    sample.sort_unstable();
    sample.dedup();

    let mut checked = 0usize;
    let mut value_mismatches = 0usize;
    let mut label_mismatches = 0usize;
    let mut bag_mismatches = 0usize;
    for &id in &sample {
        let Some(aapt_res) = aapt.get(&id) else {
            continue;
        };
        let mine = table.all_configs(bytes, id);
        if mine.is_empty() {
            continue;
        }
        checked += 1;

        // (a) value multiset (ignoring labels)
        let mut aapt_values: Vec<Option<(u8, u32)>> =
            aapt_res.by_label.values().map(|v| v.simple).collect();
        aapt_values.sort_unstable();
        let mut my_values: Vec<Option<(u8, u32)>> = mine
            .iter()
            .map(|c| {
                if c.bag.is_some() {
                    None
                } else {
                    Some((c.value.data_type, c.value.data))
                }
            })
            .collect();
        my_values.sort_unstable();
        if aapt_values != my_values {
            value_mismatches += 1;
            if value_mismatches <= 5 {
                eprintln!("VALUE id {id:#010x}: aapt={aapt_values:?} mine={my_values:?}");
            }
        }

        // (b) label set
        let aapt_labels_owned: BTreeSet<String> = aapt_res.by_label.keys().cloned().collect();
        let my_labels: BTreeSet<String> = mine.iter().map(|c| c.config_label.clone()).collect();
        if aapt_labels_owned != my_labels {
            label_mismatches += 1;
            if label_mismatches <= 5 {
                eprintln!(
                    "LABEL id {id:#010x}:\n   only aapt: {:?}\n   only mine: {:?}",
                    aapt_labels_owned.difference(&my_labels).collect::<Vec<_>>(),
                    my_labels.difference(&aapt_labels_owned).collect::<Vec<_>>()
                );
            }
        }

        // (c) bag key sets
        let mut aapt_bags: Vec<(u32, Vec<u32>)> = aapt_res
            .by_label
            .values()
            .filter_map(|v| {
                v.bag.as_ref().map(|(p, k)| {
                    let mut keys = k.clone();
                    keys.sort_unstable();
                    (*p, keys)
                })
            })
            .collect();
        aapt_bags.sort();
        let mut my_bags: Vec<(u32, Vec<u32>)> = mine
            .iter()
            .filter_map(|c| {
                c.bag.as_ref().map(|b| {
                    let mut keys: Vec<u32> = b.entries.iter().map(|(n, _)| *n).collect();
                    keys.sort_unstable();
                    (b.parent, keys)
                })
            })
            .collect();
        my_bags.sort();
        if aapt_bags != my_bags {
            bag_mismatches += 1;
            if bag_mismatches <= 5 {
                eprintln!("BAG id {id:#010x}: aapt={aapt_bags:?} mine={my_bags:?}");
            }
        }
    }
    let _ = min_checked;
    (checked, value_mismatches, label_mismatches, bag_mismatches)
}

/// Resolve the device configuration and compare the chosen value against the
/// aapt block with the same configuration label.
#[test]
fn differential_device_config_selection() {
    let Some(aapt) = find_aapt() else {
        eprintln!("aapt not found; skipping");
        return;
    };
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    if !apk_path.exists() {
        return;
    }
    let output = Command::new(&aapt)
        .args(["dump", "--values", "resources"])
        .arg(&apk_path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let aapt = parse_aapt(&text);
    let apk = ApkAssets::open(&apk_path).unwrap();
    let table = apk.resource_table().unwrap();
    let bytes = apk.resource_bytes().unwrap();

    let mut config = ResConfig::default();
    config.language = *b"en";
    config.country = *b"US";
    config.density = 420;
    config.smallest_screen_width_dp = 411;
    config.screen_width_dp = 411;
    config.screen_height_dp = 842;
    config.orientation = 1;
    config.ui_mode = 0x11;
    config.sdk_version = 34;
    config.screen_layout = 0x12;

    let mut ids: Vec<u32> = aapt.keys().copied().collect();
    ids.sort_unstable();
    let sample: Vec<u32> = ids
        .iter()
        .step_by((ids.len() / 800).max(1))
        .take(800)
        .copied()
        .collect();
    let mut checked = 0usize;
    let mut mismatches = 0usize;
    for id in sample {
        let Some(entry) = table.resolve(bytes, id, &config) else {
            continue;
        };
        checked += 1;
        let Some(aapt_res) = aapt.get(&id) else {
            continue;
        };
        if let Some(aapt_value) = aapt_res.by_label.get(&entry.config_label) {
            if let Some((t, d)) = aapt_value.simple {
                if entry.bag.is_none() && (entry.value.data_type, entry.value.data) != (t, d) {
                    mismatches += 1;
                    if mismatches <= 5 {
                        eprintln!(
                            "SELECT id {id:#010x} label {}: aapt=({t:#x},{d:#x}) mine=({:#x},{:#x})",
                            entry.config_label, entry.value.data_type, entry.value.data
                        );
                    }
                }
            }
        }
    }
    eprintln!("DIFFERENTIAL device selection: checked={checked} mismatches={mismatches}");
    assert!(checked >= 100, "expected at least 100 resolutions");
    assert_eq!(mismatches, 0, "device-config selection mismatches");
}
