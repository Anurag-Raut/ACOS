#!/usr/bin/env python3
"""Generate `crates/acos-services/src/permission_catalog.rs`.

SYSUI3B: the permission dialog asks the guest framework
`PackageManager.getPermissionInfo(...)` / `getPermissionGroupInfo(...)`, which
land on `IPermissionManager`. The answer has to be the *real* AOSP label and
description for each platform permission, not an invented string, so this
script transcribes them out of the AOSP source cache:

  * `base/core/res/AndroidManifest.xml` — every `<permission>` whose
    `protectionLevel` contains `dangerous` (name, label/description resource,
    protection level, background permission) and every `<permission-group>`
    (name, label/description resource, priority);
  * `base/core/res/res/values/strings.xml` — the `permlab_*` / `permdesc_*` /
    `permgrouplab_*` / `permgroupdesc_*` strings (the `product="default"` /
    untagged variant, which is what an English phone shows).

The permission → group membership is the **permission controller's** platform
table (`packages/modules/Permissions`, `PermissionGroups`/`Utils`), because the
API-34 core manifest declares every runtime permission with
`android:permissionGroup="android.permission-group.UNDEFINED"` and the module
maps the names to groups itself. The module's source is not in the sparse cache;
the group names, labels and descriptions below are, and the membership is the
module's published table (`TRANSCRIBED_GROUPS`).

Usage:
    tools/build/gen-permission-catalog.py [--aosp DIR] [--out FILE]

Defaults: `$ACOS_AOSP` or `/home/Qui/Projects/acos/.cache/aosp`, output
`crates/acos-services/src/permission_catalog.rs`.
"""

import argparse
import os
import re
import sys

# `packages/modules/Permissions` — the permission controller's platform
# permission -> permission group table (group name → platform permissions).
TRANSCRIBED_GROUPS = {
    "CONTACTS": ["READ_CONTACTS", "WRITE_CONTACTS", "GET_ACCOUNTS"],
    "CALENDAR": ["READ_CALENDAR", "WRITE_CALENDAR"],
    "SMS": [
        "SEND_SMS",
        "RECEIVE_SMS",
        "READ_SMS",
        "RECEIVE_WAP_PUSH",
        "RECEIVE_MMS",
        "READ_CELL_BROADCASTS",
    ],
    "STORAGE": [
        "READ_EXTERNAL_STORAGE",
        "WRITE_EXTERNAL_STORAGE",
        "ACCESS_MEDIA_LOCATION",
    ],
    "READ_MEDIA_AURAL": ["READ_MEDIA_AUDIO"],
    "READ_MEDIA_VISUAL": [
        "READ_MEDIA_IMAGES",
        "READ_MEDIA_VIDEO",
        "READ_MEDIA_VISUAL_USER_SELECTED",
    ],
    "LOCATION": [
        "ACCESS_FINE_LOCATION",
        "ACCESS_COARSE_LOCATION",
        "ACCESS_BACKGROUND_LOCATION",
    ],
    "CALL_LOG": ["READ_CALL_LOG", "WRITE_CALL_LOG", "PROCESS_OUTGOING_CALLS"],
    "PHONE": [
        "READ_PHONE_STATE",
        "READ_PHONE_NUMBERS",
        "CALL_PHONE",
        "ADD_VOICEMAIL",
        "USE_SIP",
        "ANSWER_PHONE_CALLS",
        "ACCEPT_HANDOVER",
    ],
    "MICROPHONE": ["RECORD_AUDIO"],
    "ACTIVITY_RECOGNITION": ["ACTIVITY_RECOGNITION"],
    "CAMERA": ["CAMERA"],
    "SENSORS": ["BODY_SENSORS", "BODY_SENSORS_BACKGROUND"],
    "NOTIFICATIONS": ["POST_NOTIFICATIONS"],
    "NEARBY_DEVICES": [
        "BLUETOOTH_SCAN",
        "BLUETOOTH_CONNECT",
        "BLUETOOTH_ADVERTISE",
        "UWB_RANGING",
        "NEARBY_WIFI_DEVICES",
    ],
}

# `android:protectionLevel` string -> `PermissionInfo.PROTECTION_*` bits
# (`base/core/java/android/content/pm/PermissionInfo.java:50-310`).
PROTECTION_BASE = {
    "normal": 0x0,
    "dangerous": 0x1,
    "signature": 0x2,
    "signatureOrSystem": 0x3,
    "internal": 0x4,
}
PROTECTION_FLAGS = {
    "privileged": 0x10,
    "system": 0x10,
    "development": 0x20,
    "appop": 0x40,
    "pre23": 0x80,
    "installer": 0x100,
    "verifier": 0x200,
    "preinstalled": 0x400,
    "setup": 0x800,
    "instant": 0x1000,
    "runtime": 0x2000,
    "oem": 0x4000,
    "vendorPrivileged": 0x8000,
    "textClassifier": 0x10000,
    "wellbeing": 0x20000,
    "documenter": 0x40000,
    "configurator": 0x80000,
    "incidentReportApprover": 0x100000,
    "appPredictor": 0x200000,
    "module": 0x400000,
    "companion": 0x800000,
    "retailDemo": 0x1000000,
    "recents": 0x2000000,
    "role": 0x4000000,
    "knownSigner": 0x8000000,
}


def protection_level(text):
    """Parse `android:protectionLevel` into the `PermissionInfo` int."""
    parts = text.split("|")
    value = PROTECTION_BASE.get(parts[0])
    if value is None:
        raise SystemExit(f"unknown protectionLevel {text!r}")
    for part in parts[1:]:
        try:
            value |= PROTECTION_FLAGS[part]
        except KeyError:
            raise SystemExit(f"unknown protectionLevel flag {part!r}") from None
    return value


def element_blocks(lines, tag):
    """Yield `(start_line, text)` for every `<tag .../>` element."""
    pattern = re.compile(rf"<{re.escape(tag)}[ >]")
    for index, line in enumerate(lines):
        if not pattern.search(line) or "uses-permission" in line:
            continue
        block = [line]
        end = index
        while "/>" not in lines[end] and f"</{tag}>" not in lines[end]:
            end += 1
            block.append(lines[end])
        yield index + 1, "\n".join(block)


def attr(text, name):
    match = re.search(rf'android:{name}="([^"]*)"', text)
    return match.group(1) if match else None


def string_table(lines, prefix):
    """`name -> (value, line)` for `<string name="prefix...">` entries.

    A value can span several lines, and a name can have `product=` variants;
    the English phone (`product="default"` or no product) wins.
    """
    table = {}
    for index, line in enumerate(lines):
        match = re.search(rf'<string name="({re.escape(prefix)}[A-Za-z0-9_]*)"([^>]*)>', line)
        if not match:
            continue
        name, attributes = match.group(1), match.group(2)
        if 'product="' in attributes and 'product="default"' not in attributes:
            continue
        value = line[match.end() :]
        end = index
        while "</string>" not in value:
            end += 1
            value += "\n" + lines[end]
        value = value.split("</string>", 1)[0]
        table.setdefault(name, (value, index + 1))
    return table
    return table


def resolve(strings, reference, where):
    if reference is None:
        return None, 0
    match = re.match(r"@string/(.+)", reference)
    if not match:
        raise SystemExit(f"{where}: {reference!r} is not a @string reference")
    name = match.group(1)
    if name not in strings:
        raise SystemExit(f"{where}: @string/{name} not in strings.xml")
    return strings[name][0], strings[name][1]


def xml_unescape(value):
    """Undo the XML escapes AOSP uses in `strings.xml` values, and aapt's
    whitespace collapsing (runs of whitespace in a value are one space)."""
    value = re.sub(r"\s+", " ", value).strip()
    return (
        value.replace("\\'", "'")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", '"')
    )


def rust_str(value):
    """A plain `&'static str` literal (the value is required)."""
    escaped = xml_unescape(value).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def rust_opt_str(value):
    """An `Option<&'static str>` literal."""
    return "None" if value is None else f"Some({rust_str(value)})"


def main():
    parser = argparse.ArgumentParser()
    default_aosp = os.environ.get("ACOS_AOSP", "/home/Qui/Projects/acos/.cache/aosp")
    parser.add_argument("--aosp", default=default_aosp)
    here = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    parser.add_argument(
        "--out",
        default=os.path.join(here, "crates/acos-services/src/permission_catalog.rs"),
    )
    args = parser.parse_args()

    manifest_path = os.path.join(args.aosp, "base/core/res/AndroidManifest.xml")
    strings_path = os.path.join(args.aosp, "base/core/res/res/values/strings.xml")
    manifest = open(manifest_path, encoding="utf-8").read().split("\n")
    strings = string_table(open(strings_path, encoding="utf-8").read().split("\n"), "")

    groups = []
    for line, text in element_blocks(manifest, "permission-group"):
        name = attr(text, "name")
        if name is None:
            continue
        label, label_line = resolve(strings, attr(text, "label"), f"AndroidManifest.xml:{line}")
        description, description_line = resolve(
            strings, attr(text, "description"), f"AndroidManifest.xml:{line}"
        )
        groups.append(
            {
                "name": name,
                "label": label,
                "description": description,
                "priority": int(attr(text, "priority") or "0"),
                "source": f"base/core/res/AndroidManifest.xml:{line}",
                "string_source": f"base/core/res/res/values/strings.xml:{label_line}"
                + (f",{description_line}" if description_line else ""),
            }
        )
    groups.sort(key=lambda group: group["name"])

    permissions = []
    for line, text in element_blocks(manifest, "permission"):
        name = attr(text, "name")
        protection = attr(text, "protectionLevel")
        if name is None or protection is None or "dangerous" not in protection:
            continue
        label, label_line = resolve(strings, attr(text, "label"), f"AndroidManifest.xml:{line}")
        description, description_line = resolve(
            strings, attr(text, "description"), f"AndroidManifest.xml:{line}"
        )
        declared_group = (attr(text, "permissionGroup") or "").replace(
            "android.permission-group.", ""
        )
        group = "UNDEFINED"
        for candidate, names in TRANSCRIBED_GROUPS.items():
            if name.split(".")[-1] in names:
                group = candidate
                break
        if group == "UNDEFINED" and declared_group != "UNDEFINED":
            group = declared_group
        permissions.append(
            {
                "name": name,
                "label": label,
                "description": description,
                "protection_level": protection_level(protection),
                "group": group,
                "background_permission": attr(text, "backgroundPermission"),
                "source": f"base/core/res/AndroidManifest.xml:{line}",
                "string_source": f"base/core/res/res/values/strings.xml:{label_line}"
                + (f",{description_line}" if description_line else ""),
            }
        )
    permissions.sort(key=lambda permission: permission["name"])

    known_groups = {group["name"].split(".")[-1] for group in groups}
    for permission in permissions:
        if permission["group"] not in known_groups:
            raise SystemExit(
                f"{permission['name']}: group {permission['group']} has no <permission-group>"
            )

    lines = []
    lines.append("//! The platform permission catalog: AOSP's English labels, groups and")
    lines.append("//! descriptions for the dangerous permissions (SYSUI3B).")
    lines.append("//!")
    lines.append("//! Generated by `tools/build/gen-permission-catalog.py` from the AOSP source cache")
    lines.append("//! (tag `android-14.0.0_r75`). Do not edit by hand: edit the generator and")
    lines.append("//! re-run it, so every row keeps its `file:line` source.")
    lines.append("//!")
    lines.append("//! Field meanings mirror `android.content.pm.PermissionInfo` /")
    lines.append("//! `PermissionGroupInfo`: `label` becomes the reply's")
    lines.append("//! `nonLocalizedLabel` (so the framework's own `PermissionInfo.loadLabel`")
    lines.append("//! answers it with no resource resolution), `description` becomes")
    lines.append("//! `nonLocalizedDescription`, and `group` is the short")
    lines.append("//! `android.permission-group.*` name the permission belongs to.")
    lines.append("")
    lines.append("/// One platform permission, in AOSP's own words.")
    lines.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    lines.append("pub struct CatalogPermission {")
    lines.append("    /// The full permission name (`android.permission.CAMERA`).")
    lines.append("    pub name: &'static str,")
    lines.append("    /// `permlab_*`: what `PermissionInfo.loadLabel` returns (`ACCEPT_HANDOVER`")
    lines.append("    /// declares none in the API-34 core manifest).")
    lines.append("    pub label: Option<&'static str>,")
    lines.append("    /// `permdesc_*`: what `PermissionInfo.loadDescription` returns.")
    lines.append("    pub description: Option<&'static str>,")
    lines.append("    /// The `PermissionInfo.PROTECTION_*` bits from `android:protectionLevel`.")
    lines.append("    pub protection_level: i32,")
    lines.append("    /// The short group name (`CAMERA`), or `UNDEFINED`.")
    lines.append("    pub group: &'static str,")
    lines.append("    /// `android:backgroundPermission`, when the permission has one.")
    lines.append("    pub background_permission: Option<&'static str>,")
    lines.append("    /// Where the declaration came from (`file:line` in the AOSP cache).")
    lines.append("    pub source: &'static str,")
    lines.append("    /// Where the label/description strings came from (`file:line`).")
    lines.append("    pub string_source: &'static str,")
    lines.append("}")
    lines.append("")
    lines.append("/// One platform permission group, in AOSP's own words.")
    lines.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    lines.append("pub struct CatalogGroup {")
    lines.append("    /// The full group name (`android.permission-group.CAMERA`).")
    lines.append("    pub name: &'static str,")
    lines.append("    /// `permgrouplab_*`: the group title a dialog shows (`UNDEFINED` has none).")
    lines.append("    pub label: Option<&'static str>,")
    lines.append("    /// `permgroupdesc_*`.")
    lines.append("    pub description: Option<&'static str>,")
    lines.append("    /// `android:priority` (the dialog sorts groups by it).")
    lines.append("    pub priority: i32,")
    lines.append("    /// Where the declaration came from (`file:line` in the AOSP cache).")
    lines.append("    pub source: &'static str,")
    lines.append("    /// Where the label/description strings came from (`file:line`).")
    lines.append("    pub string_source: &'static str,")
    lines.append("}")
    lines.append("")
    lines.append("/// The permission controller's platform permission → group table")
    lines.append("/// (`packages/modules/Permissions`, `PermissionGroups`/`Utils`). The API-34")
    lines.append("/// core manifest declares every runtime permission with")
    lines.append("/// `android:permissionGroup=\"android.permission-group.UNDEFINED\"`; the")
    lines.append("/// controller is what maps the names to groups, so the mapping is")
    lines.append("/// transcribed here (the module source is not in the sparse AOSP cache).")
    lines.append("pub const GROUP_SOURCE: &str =")
    lines.append('    "packages/modules/Permissions (PermissionController PermissionGroups table)";')
    lines.append("")
    lines.append("/// Every dangerous platform permission, in name order.")
    lines.append("pub const PERMISSIONS: &[CatalogPermission] = &[")
    for permission in permissions:
        lines.append("    CatalogPermission {")
        lines.append(f'        name: "{permission["name"]}",')
        lines.append(f"        label: {rust_opt_str(permission['label'])},")
        lines.append(f"        description: {rust_opt_str(permission['description'])},")
        lines.append(f"        protection_level: 0x{permission['protection_level']:x},")
        lines.append(f'        group: "{permission["group"]}",')
        background = permission["background_permission"]
        lines.append(
            f"        background_permission: {rust_opt_str(background)},"
        )
        lines.append(f'        source: "{permission["source"]}",')
        lines.append(f'        string_source: "{permission["string_source"]}",')
        lines.append("    },")
    lines.append("];")
    lines.append("")
    lines.append("/// Every platform permission group, in name order.")
    lines.append("pub const GROUPS: &[CatalogGroup] = &[")
    for group in groups:
        lines.append("    CatalogGroup {")
        lines.append(f'        name: "{group["name"]}",')
        lines.append(f"        label: {rust_opt_str(group['label'])},")
        lines.append(f"        description: {rust_opt_str(group['description'])},")
        lines.append(f"        priority: {group['priority']},")
        lines.append(f'        source: "{group["source"]}",')
        lines.append(f'        string_source: "{group["string_source"]}",')
        lines.append("    },")
    lines.append("];")
    lines.append("")
    lines.append("/// The catalog row for `name`, when it is a standard dangerous permission.")
    lines.append("#[must_use]")
    lines.append("pub fn permission(name: &str) -> Option<&'static CatalogPermission> {")
    lines.append("    PERMISSIONS.iter().find(|row| row.name == name)")
    lines.append("}")
    lines.append("")
    lines.append("/// The catalog row for the group `name` (full or short form).")
    lines.append("#[must_use]")
    lines.append("pub fn group(name: &str) -> Option<&'static CatalogGroup> {")
    lines.append('    let short = name.rsplit(\'.\').next().unwrap_or(name);')
    lines.append("    GROUPS")
    lines.append("        .iter()")
    lines.append("        .find(|row| row.name == name || row.name.ends_with(&format!(\".{short}\")))")
    lines.append("}")
    lines.append("")
    lines.append("/// Every catalog permission in `group` (short or full group name).")
    lines.append("#[must_use]")
    lines.append("pub fn permissions_in_group(group_name: &str) -> Vec<&'static CatalogPermission> {")
    lines.append('    let short = group_name.rsplit(\'.\').next().unwrap_or(group_name);')
    lines.append("    PERMISSIONS")
    lines.append("        .iter()")
    lines.append("        .filter(|row| row.group == short)")
    lines.append("        .collect()")
    lines.append("}")
    lines.append("")
    lines.append("#[cfg(test)]")
    lines.append("mod tests {")
    lines.append("    use super::*;")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn every_row_carries_a_label_and_a_source() {")
    lines.append("        for row in PERMISSIONS {")
    lines.append('            assert!(row.label.is_some() || row.name.ends_with("ACCEPT_HANDOVER"));')
    lines.append('            assert!(row.description.is_some() || row.name.ends_with("ACCEPT_HANDOVER"));')
    lines.append('            assert!(row.source.contains("AndroidManifest.xml:"), "{} source", row.name);')
    lines.append('            assert!(row.string_source.contains("strings.xml:"), "{} strings", row.name);')
    lines.append("        }")
    lines.append("        for row in GROUPS {")
    lines.append('            assert!(row.source.contains("AndroidManifest.xml:"), "{} source", row.name);')
    lines.append('            assert!(row.label.is_some() || row.name.ends_with("UNDEFINED"));')
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn the_dialog_permissions_have_aosp_labels_and_groups() {")
    lines.append('        let camera = permission("android.permission.CAMERA").expect("camera");')
    lines.append('        assert_eq!(camera.label, "take pictures and videos");')
    lines.append('        assert_eq!(camera.group, "CAMERA");')
    lines.append("        assert_eq!(camera.protection_level, 0x1 | 0x1000);")
    lines.append("        let location =")
    lines.append('            permission("android.permission.ACCESS_FINE_LOCATION").expect("fine location");')
    lines.append('        assert_eq!(location.group, "LOCATION");')
    lines.append("        assert_eq!(")
    lines.append("            location.background_permission,")
    lines.append('            Some("android.permission.ACCESS_BACKGROUND_LOCATION")')
    lines.append("        );")
    lines.append('        assert_eq!(group("CAMERA").expect("camera group").label, "Camera");')
    lines.append('        assert_eq!(group("LOCATION").expect("location group").label, "Location");')
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn every_catalog_permission_belongs_to_a_catalog_group() {")
    lines.append("        for row in PERMISSIONS {")
    lines.append("            assert!(")
    lines.append("                group(row.group).is_some(),")
    lines.append('                "{} names group {} which is not in GROUPS",')
    lines.append("                row.name,")
    lines.append("                row.group")
    lines.append("            );")
    lines.append("            assert_eq!(permissions_in_group(row.group).len() > 0, true);")
    lines.append("        }")
    lines.append("    }")
    lines.append("}")
    lines.append("")

    output = "\n".join(lines)
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write(output)
    print(
        f"wrote {args.out}: {len(permissions)} permissions, {len(groups)} groups",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
