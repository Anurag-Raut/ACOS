# Live parity captures (`device-screenshot.png`)

These are fresh live captures taken from the reference phone
(`ZA222WGJPP`, motorola moto g35 5G, Android 15 / API 35, 1080x2400 @ 400 dpi)
through `acos parity --device`:

| app | file | size | notes |
|---|---|---|---|
| counter | `counter/device-screenshot.png` | 540x1200 | app foreground, "Counter / 0 / Increment / Reset" |
| todo-dogfood | `todo-dogfood/device-screenshot.png` | 540x1200 | app foreground, "Todo Dogfood / Count: 0 / +1 -1 Reset / Save snapshot" |
| widget-gallery | `widget-gallery/device-screenshot.png` | 540x1200 | app foreground, full widget gallery |

The originals are 1080x2400 APNG-less RGBA PNGs; they were downscaled to
540x1200 (`magick <in> -resize 540x1200 -strip -define png:compression-level=9`)
so only small files are checked in. They are **screenshots only**: the
`uiautomator dump` XML could not be captured on this run.

## Why no `device-ui.xml`

`uiautomator dump` is consistently SIGKILLed (`exit 137`) by the device's
low-memory killer once a fixture app is foreground: the phone reports
~3.0 GiB of 3.4 GiB used (~320-490 MiB free, `adb shell free -m`). When no app
is running the dump does sometimes complete and write a hierarchy, so the
failure is memory pressure, not a broken tool. `acos parity` therefore reports

```json
{ "ok": false, "code": "DEVICE_BUSY",
  "message": "uiautomator dump produced no <hierarchy>; the device may be out of memory ..." }
```

and still saves the screenshot. Freeing the device (uninstalling the stale
`dev.acos.fixtures.*` builds from earlier waves, or rebooting it) would let the
same command produce `device-ui.xml` as well.

`native-hello` is deliberately absent: it failed to launch on this device
(the screenshot showed the launcher), so no capture of that app was saved.

## Regenerate

```bash
cargo build -p acos-cli

# one fixture app (writes <app>/device-screenshot.png + device-ui.xml when the
# phone can supply a hierarchy):
./target/debug/acos parity --device ZA222WGJPP fixtures/build/apps/counter.apk \
  --output results/parity/live-counter --json
```

When the phone can supply a hierarchy but ACOS cannot yet produce its own tree
(the engine currently stops early in the launch ladder, e.g. at
`VectorDrawable`), the report is `code: ACOS_BLOCKED` with a full
`acos_blocked_at` object and every device node listed as `missing_in_acos` —
never an empty pass.
