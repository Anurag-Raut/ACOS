# ACOS physical-device reference device

Captured with `tools/oracle/capture-device-reference.sh`.

- **Fidelity label: `physical-device`** — a real phone, not an emulator and not
  ART-reference. It is never silently mixed with
  `fixtures/reference/<app>/art-api34-x86_64`.
- **Device serial:** stored only as a SHA-256 hash (`74c946119e36b4e4529639cb8b53c7fa789c7ac5628d4cf9d50a846286fa2e62`); the raw
  serial is deliberately not written to this repository.
- **Model:** motorola moto g35 5G
- **Device/product:** manila / manila_g
- **Android:** 15 (API level 35)
- **Build:** VUOA35.116-151 — `motorola/manila_g/manila:15/VUOA35.116-151/a43dc2:user/release-keys`
- **Display:** 1080x2400 physical, density 400 dpi,
  current display 1080x2400
- **Locale:** en-US, font scale 1.0
- **ABI:** arm64-v8a (arm64-v8a,armeabi-v7a,armeabi)
- **Rotation settings:** the device was already in sensor-driven portrait when the
  capture started. The pre-existing default was `user_rotation=null`
  (`accelerometer_rotation=1`); the capture run recorded `user_rotation=1` at
  start because an earlier manual smoke test had set it. The capture script
  restores the rotation settings it observed and finishes with
  `agent-device orientation auto`; the device was left in sensor-driven portrait
  (`user_rotation=0`, `accelerometer_rotation=1`, display 1080x2400).

## Why the numbers differ from `fixtures/reference/<app>/art-api34-x86_64`

The ART oracle is an **API 34** `google_apis` **x86_64** emulator at **420 dpi**.
This phone is an **API 35** arm64 device at **400 dpi**. Texts and resource ids
are stable, but pixel bounds are not: density changes the dp→px ratio and the
Android 15 edge-to-edge/inset rules move the content origin. Every bounds
difference in `COMPARISON.md` is labelled `bounds_differ_density` and is an
expected fidelity difference, not a regression.

## Safety notes

* Only the four ACOS fixture packages (`dev.acos.fixtures.counter`,
  `dev.acos.fixtures.todolist`, `dev.acos.fixtures.multiactivity`,
  `dev.acos.fixtures.formvalidation`) were installed, driven and uninstalled.
* No account, security, connectivity or lock-screen setting was changed.
* Orientation was driven through `agent-device orientation` and restored with
  `agent-device orientation auto`.
