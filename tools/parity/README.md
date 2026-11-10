# UI-tree parity (`acos parity`)

Two modes, one comparator:

| mode | command | input | phone? |
|---|---|---|---|
| recorded | `acos parity --recorded <app>` | `fixtures/reference-device/<app>/snapshot-*.json` vs `fixtures/reference/<app>/art-api34-x86_64/ui_initial.xml` | no |
| live | `acos parity --device <serial> <apk>` | `uiautomator dump` + screencap + the live ACOS `View` tree | yes |

Both emit one JSON object. `identical: true` / `ok: true` only when there is no
normalized difference. A missing phone is `ok: false`, `code:
NEEDS_REAL_DEVICE` and exit 1 — never an empty pass. A phone that is attached
but cannot supply a capture (out of memory, `uiautomator` killed) is
`ok: false`, `code: DEVICE_BUSY` with the screenshot still saved as a
reference — also never an empty pass.

When the phone capture succeeds but ACOS cannot yet produce its own tree (the
current engine stops early in the launch ladder, e.g. at `VectorDrawable`), the
report is `ok: false`, `code: ACOS_BLOCKED`, carries the full
`acos_blocked_at` object (`reason`, `message`, `app_created`,
`transaction_delivered`, `looper_quit`, `launch_error`), lists every device
node as `missing_in_acos`, and saves the phone capture under
`fixtures/reference-device/<app>/`. It is a blocker on the engine, not a pass.

## What is compared

Nodes are matched by a stable key, in this order:

1. `id:<resource-id>` when the Android id is non-empty;
2. `text:<class>|<text>` when there is text;
3. `class:<class>` otherwise.

For each matched pair the comparator checks:

* **text** — exact after trimming whitespace;
* **class** — exact when both sides report one;
* **bounds** — the device bounds are multiplied by `--bounds-scale` and then
  each edge must be within `--bounds-tolerance` pixels of ACOS's.

`--ignore-bounds` restricts the comparison to text/class/id, and
`--strict-state` also compares `enabled`/`focused`/`clickable`.

Difference kinds: `missing_in_acos` (device has it, ACOS does not),
`extra_in_acos`, `text_differs`, `class_differs`, `bounds_differ`,
`state_differs`. A scaled bounds difference is labelled
`bounds_differ_density` in `note` — that is the expected API-35/400-dpi vs
API-34/420-dpi difference documented in `fixtures/reference-device/COMPARISON.md`,
not a text failure.

## Recorded (no phone)

```bash
tools/parity/recorded.sh
# or one app:
./target/release/acos parity --recorded counter --bounds-scale 1.05 --bounds-tolerance 24 --json
```

`--acos-tree <file>` replaces the ART reference with a stored ACOS
`observe --tree` JSON so a device capture can be compared with a real ACOS
tree offline.

## Live (optional)

```bash
tools/parity/live.sh fixtures/build/apps/counter.apk
# equivalent:
./target/release/acos parity --device ZA222WGJPP fixtures/build/apps/counter.apk \
  --output results/parity/live --json
```

Safety:

* the APK package must start with `dev.acos.fixtures.` (checked before install);
* only `adb install`, `am start`, `uiautomator dump`, `screencap` and
  `adb uninstall` of that package are issued;
* no device setting is read or written;
* `--keep` leaves the fixture installed for manual inspection.

If the ACOS side cannot reach a `ViewRootImpl` (the launch has not built a view
hierarchy yet), the live report is `ok: false`, `code: ACOS_BLOCKED` with
`acos_blocked_at` (the launch booleans and the blocked stage) and the phone
capture saved under `fixtures/reference-device/<app>/device-ui.xml` +
`device-screenshot.png`. That is a blocker on the engine, not a pass. If
`uiautomator` is killed before it writes a hierarchy (a memory-pressured
device), the report is `code: DEVICE_BUSY` and only the screenshot is saved.
