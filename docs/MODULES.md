<!-- ACOS_MODULES_BEGIN -->
# Service modules (generated)

Generated from `ModuleRegistry::standard()` (ADR-0029). Do not edit this block by hand: `cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md`, and `crates/acos-services/tests/modules_doc.rs` fails when it is stale.

A module is a set of Android system pieces attached to a session. `off` is always available and is not listed under `profiles`; a profile that is listed is an implementation, and a verdict that relies on `stub` or `scripted` says so. `attach` is `at-launch` (only in `acos_launch {services: [...]}`) or `hot` (also on a live session with `acos_services`).

## Modules

| module | profiles | attach | security | RAM (MiB) | startup (ms) | summary | provides | controls |
|---|---|---|---|---|---|---|---|---|
| `core` | `off`, `real` | `at-launch` | `in-process` | 16 | 250 | the Android system services every session boots with (AMS, ATMS, PMS, WMS, IME, clipboard, settings, display, extras) | `activity`, `window`, `activity_task`, `package`, `display`, `autofill`, `textservices`, `audio`, `graphicsstats`, `network_management`, `game`, `virtualdevice`, `content_capture`, `voiceinteraction`, `content`, `settings`, `input`, `input_method`, `ime_session`, `ime_tracker`, `clipboard`, `user`, `uimode`, `notification`, `statusbar`, `permission`, `appops`, `connectivity`, `androidfuture`, `SurfaceFlinger`, `mount`, `shortcut`, `jobscheduler`, `power`, `account`, `thermalservice`, `alarm`, `trust`, `restrictions`, provider `settings` | — |
| `accessibility` | `off`, `real` | `hot` | `in-process` | 2 | 10 | AccessibilityManagerService state: enable services via secure settings, client state, event/interaction records (service binding not yet) | `accessibility`, window `TYPE_ACCESSIBILITY_OVERLAY` | `enable`, `disable`, `list_events`, `global_action`, `status` |
| `system.permissions` | `off`, `real` | `hot` | `in-process` | 1 | 2 | runtime permissions: one shared store behind both checkPermission paths, flags, rationale, grant/deny/revoke, and the permission dialog served by the com.android.permissioncontroller system app | `permission`, app `com.android.permissioncontroller` | `grant`, `deny`, `revoke`, `state` |
| `system.notifications` | `off`, `real` | `hot` | `in-process` | 1 | 2 | notification channels, posting with the POST_NOTIFICATIONS gate, taps/actions fired through the real PendingIntent sender, dismiss (no status-bar pixels: the shade is the system-UI track) | `notification`, app `com.android.systemui` | `list`, `tap`, `action`, `dismiss`, `state`, `open_shade` |
| `sensors` | `off`, `stub`, `scripted` | `hot` | `in-process` | 2 | 5 | scripted sensors (accelerometer, gyroscope, magnetometer, light, proximity, rotation vector, step counter) through the native sensorservice; no `real` profile (ACOS has no sensor hardware) | `sensorservice` | `status`, `set_value`, `play`, `shake`, `tilt`, `enable`, `disable`, `events`, `reset` |
| `feedback.vibrator` | `off`, `stub`, `scripted` | `hot` | `in-process` | 1 | 5 | vibrator manager: stub/scripted (no vibration hardware), records what the app asked for | `vibrator_manager` | `calls`, `patterns`, `reset` |
| `location` | `off`, `stub`, `scripted` | `hot` | `in-process` | 2 | 10 | location: scripted providers, fixes, tracks and geocoder (no GPS in a sandbox) | `location` | `set_location`, `play_track`, `stop_track`, `set_provider_enabled`, `set_location_enabled`, `set_permission`, `set_geocoder`, `delivered`, `reset` |
| `net.telephony` | `off`, `stub`, `scripted` | `at-launch` | `in-process` | 1 | 5 | telephony: scripted SIM/phone/network state, line-1 number, SMS send recording and SMS_RECEIVED/call-state events (no radio) | `phone`, `iphonesubinfo`, `telephony.registry`, `isms`, feature `android.hardware.telephony`, feature `android.hardware.telephony.messaging`, feature `android.hardware.telephony.subscription`, broadcast `android.provider.Telephony.SMS_RECEIVED` | `state`, `set`, `call_state`, `sms`, `reset` |
| `hw.bluetooth` | `off`, `stub`, `scripted` | `hot` | `in-process` | 1 | 5 | bluetooth: scripted adapter state, name/address, bonded-device list and classic-discovery results (no radio, no BLE/GATT) | `bluetooth_manager`, `bluetooth`, feature `android.hardware.bluetooth`, broadcast `android.bluetooth.adapter.action.STATE_CHANGED`, broadcast `android.bluetooth.adapter.action.DISCOVERY_STARTED`, broadcast `android.bluetooth.adapter.action.DISCOVERY_FINISHED`, broadcast `android.bluetooth.device.action.FOUND` | `state`, `set`, `bonded`, `scan`, `reset` |
| `hw.biometric` | `off`, `stub`, `scripted` | `hot` | `in-process` | 1 | 5 | biometrics: scripted canAuthenticate and BiometricPrompt authenticate results (no hardware, no prompt UI) | `auth` | `state`, `set`, `auth`, `reset` |
| `network` | `off`, `real`, needs `pasta`, `acos-netd` | `at-launch` | `host-tool` | 2 | 100 | real internet through the acos-netd relay (one pasta per host, one netns+veth per session, dnsproxyd in the guest); hot online/offline/latency/loss controls and the explicit host-port opt-in; no `stub`/`scripted` profile (a fake network would be a fake success) | `connectivity` | `status`, `offline`, `online`, `delay`, `hostport` |
| `webview` | `off`, `real`, needs `acos-webview`, `chrome-headless-shell` | `at-launch` | `host-tool` | 4 | 11 | a real WebView: the `com.android.webview` provider package installed in the session plus the shared host Chromium behind the session's gate socket (one broker per host, ~130-140 MiB once, started on the first connect); `off` is Android's own no-provider path, and there is no stub/scripted profile | app `com.android.webview` | `status` |
| `net.wifi` | `off`, `stub`, `scripted` | `hot` | `in-process` | 1 | 5 | Wi-Fi client radio: stub (one SSID from config) / scripted (connect, disconnect, rssi, scan -> WIFI_STATE_CHANGED, NETWORK_STATE_CHANGED, RSSI_CHANGED, SCAN_RESULTS, NetworkCallback loss/availability); no real radio, no hotspot/P2P/configured networks | `wifi`, feature `android.hardware.wifi`, broadcast `android.net.wifi.WIFI_STATE_CHANGED`, broadcast `android.net.wifi.STATE_CHANGE`, broadcast `android.net.wifi.RSSI_CHANGED`, broadcast `android.net.wifi.SCAN_RESULTS` | `state`, `enable`, `connect`, `disconnect`, `rssi`, `scan`, `reset` |
| `media.session` | `off`, `stub`, `scripted` | `hot` | `in-process` | 1 | 5 | media sessions: stub (record what apps publish) / scripted (agent play, pause, next, seek reach MediaSession.Callback); no media-key routing, queue, volume, remote sessions or router | `media_session`, `media_router` | `sessions`, `play`, `pause`, `stop`, `next`, `previous`, `fast_forward`, `rewind`, `seek`, `skip_to_item`, `custom_action`, `reset` |
| `hw.camera` | `off`, `stub` | `hot` | `in-process` | 1 | 5 | camera service (`media.camera`): one camera (id, facing, orientation, sensor size from the config) with real CameraCharacteristics for CameraManager and CameraX; opening it and frames are not implemented yet; no `real` profile (ACOS has no camera HAL) | `media.camera`, feature `android.hardware.camera`, feature `android.hardware.camera.any` | `status`, `camera`, `characteristics`, `calls`, `reset` |
| `hw.usb` | `off`, `stub` | `hot` | `in-process` | 1 | 2 | USB host: stub (no USB hardware), empty device list and no configured functions | `usb` | — |
| `system.dropbox` | `off`, `stub` | `hot` | `in-process` | 1 | 2 | DropBox: stub (no system DropBox), no entries and no enabled tags | `dropbox` | — |
| `hw.nfc` | `off` | `hot` | `in-process` | 1 | 1 | NFC: off (no NFC hardware) | `nfc` | — |
| `system.wallpaper` | `off` | `hot` | `in-process` | 1 | 1 | wallpaper: off (no wallpaper service; AOSP's DisabledWallpaperManager is the honest manager) | `wallpaper` | — |
| `system.print` | `off` | `hot` | `in-process` | 1 | 1 | print: off (no print service; the client is feature-gated) | `print` | — |
| `system.search` | `off` | `hot` | `in-process` | 1 | 1 | search: off (no search service and no searchable index) | `search` | — |
| `system.usage` | `off` | `hot` | `in-process` | 1 | 1 | usage stats: off (nothing records app usage) | `usagestats` | — |
| `system.dpm` | `off` | `hot` | `in-process` | 1 | 1 | device policy: off (no managed device or profile) | `device_policy` | — |
| `system.role` | `off` | `hot` | `in-process` | 1 | 1 | role manager: off (no role manager; the AIDL table is outside this image's framework dex) | `role` | — |
| `geo.country` | `off` | `hot` | `in-process` | 1 | 1 | country detector: off (no location/country service) | `country_detector` | — |

`RAM` is the manifest's declared attach budget.

## Control operations

`acos_service_ctl {module, op, args}` (also `service_ctl:` in a scenario).

| module | op | args |
|---|---|---|
| `accessibility` | `enable` | {"services": ["pkg/.Class"]} — the Settings UI's switch: writes Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES/ACCESSIBILITY_ENABLED and lets the system's settings observer bind the service (AccessibilityManagerService.setEnabledServices -> bindServices) |
| `accessibility` | `disable` | none — the Settings UI switched off: clears the enable set, so the bound service is unbound (onUnbind/onDestroy) |
| `accessibility` | `list_events` | {"limit": N} — the recent a11y events (newest last, default 50) |
| `accessibility` | `global_action` | {"action": "BACK"|"HOME"|"RECENTS"|"NOTIFICATIONS"} — performed through the session's input path |
| `accessibility` | `status` | none — the module's full honest state (enabled, connections, event counts) |
| `system.permissions` | `grant` | {permission} — grant a runtime permission (the app's checkSelfPermission answers granted) |
| `system.permissions` | `deny` | {permission, never_ask_again?} — deny it; `never_ask_again: true` also sets FLAG_PERMISSION_USER_FIXED |
| `system.permissions` | `revoke` | {permission} — clear the runtime state, back to the install-time answer |
| `system.permissions` | `state` | none — every permission with granted/never_ask_again/rationale/flags, the requests and the mutations |
| `system.notifications` | `list` | none — every posted notification with id/tag/title/text/channel/actions/content sender |
| `system.notifications` | `tap` | {id} or {tag} or {title}/{text} — fire the content PendingIntent (starts the target activity) |
| `system.notifications` | `action` | {id, index} — fire action `index`'s PendingIntent |
| `system.notifications` | `dismiss` | {id} or {tag} — remove the notification, like a user swipe |
| `system.notifications` | `state` | none — channels, posted, dropped (POST_NOTIFICATIONS), taps and the gate |
| `system.notifications` | `open_shade` | none — open the notification shade: start the com.android.systemui system app's ShadeActivity with one row per posted notification (starts a process lazily, like requestPermissions -> the dialog) |
| `sensors` | `status` | none — the served sensor table, current values, live connections and counters |
| `sensors` | `set_value` | {sensor: <name|type>, values: [..]} — set one sensor's value and deliver it now |
| `sensors` | `play` | {sensor, samples: [{t_ms, values}], loop} — schedule a timeline of events |
| `sensors` | `shake` | {sensor?: accelerometer, amplitude?: 12.0, count?: 12, period_ms?: 40} |
| `sensors` | `tilt` | {angle_deg?: 30, axis?: x|y, duration_ms?: 250} — ramp the gravity vector |
| `sensors` | `enable` | {sensor} — enable a sensor on every open event connection (as a listener would) |
| `sensors` | `disable` | {sensor} — disable it again |
| `sensors` | `events` | {limit?: 32} — the delivered events a verdict can cite |
| `sensors` | `reset` | none — clear values, timelines and the event log |
| `feedback.vibrator` | `calls` | none — returns the recorded vibrate/cancel/haptic/query counts and every recorded pattern (token, length, hash) |
| `feedback.vibrator` | `patterns` | none — the recorded `vibrate(...)` patterns only |
| `feedback.vibrator` | `reset` | none — clears the recorded counts |
| `location` | `set_location` | {lat, lng, provider?, accuracy?, altitude?, speed?, bearing?, mock?} — one fix |
| `location` | `play_track` | {points: [{lat, lng, ...}], interval_ms, loop?, provider?} — updates at real timing |
| `location` | `stop_track` | none — stops a running play_track |
| `location` | `set_provider_enabled` | {provider, enabled} — toggles a provider and notifies its listeners |
| `location` | `set_location_enabled` | {enabled} — the Settings.Secure location setting |
| `location` | `set_permission` | {level: fine|coarse|none} — what callers may receive |
| `location` | `set_geocoder` | {present?, places?, by_name?} — scripted geocoder answers (`present:false` = none) |
| `location` | `delivered` | none — every fix delivered in this session (a verdict cites this) |
| `location` | `reset` | none — clears delivered fixes and the geocoder script |
| `net.telephony` | `state` | none — the scripted state, recorded sends and injected SMS |
| `net.telephony` | `set` | {sim_state?, icc_present?, phone_type?, network_type?, line1?, operator_numeric?, operator_name?} — change the scripted answers; a call-state change notifies every registered listener |
| `net.telephony` | `call_state` | {state: "idle"|"ringing"|"offhook", number?} — set the call state and deliver onLegacyCallStateChanged/onCallStateChanged to every listener that asked for it |
| `net.telephony` | `sms` | {from, body, pdu?} — inject an incoming SMS as the SMS_RECEIVED broadcast (a real GSM 03.40 SMS-DELIVER PDU; `pdu` hex overrides the encoder) |
| `net.telephony` | `reset` | none — clear recorded sends/injections (not the scripted state) |
| `hw.bluetooth` | `state` | none — the scripted adapter state, bonded/scan devices and recorded calls |
| `hw.bluetooth` | `set` | {on?, name?, address?} — set the adapter state; a change delivers ACTION_STATE_CHANGED and calls onBluetoothServiceUp/Down |
| `hw.bluetooth` | `bonded` | {devices: [{address, name, rssi?}]} — replace the bonded-device list `getBondedDevices` answers |
| `hw.bluetooth` | `scan` | {devices?: [{address, name, rssi?}]} — run a scripted discovery: ACTION_DISCOVERY_STARTED, one ACTION_FOUND per device, ACTION_DISCOVERY_FINISHED |
| `hw.bluetooth` | `reset` | none — clear the recorded calls (not the scripted state) |
| `hw.biometric` | `state` | none — canAuthenticate answer, enrolled flag, pending and answered authentications |
| `hw.biometric` | `set` | {can_authenticate?, enrolled?} — can_authenticate is success|no_hardware|none_enrolled|hw_unavailable|security_update |
| `hw.biometric` | `auth` | {result: success|failed|error|cancel|lockout, modality?, error?, vendor_code?} — answer the pending authenticate call; `error` needs modality/error (constants or names) |
| `hw.biometric` | `reset` | none — clear pending/answered records (not the scripted answers) |
| `network` | `status` | none — the relay state and this session's own row (`networked`, `offline`, `delay_ms`, `loss_pct`, `host_ports`) |
| `network` | `offline` | none — drop this session's traffic (its own veth only) |
| `network` | `online` | none — restore it |
| `network` | `delay` | {ms, jitter_ms, loss_pct} — shape this session's veth; `0/0/0` clears it |
| `network` | `hostport` | {port, on?} — the explicit host-loopback opt-in (Metro's `localhost`) |
| `webview` | `status` | none — the provider package and APK this session installs, the gate socket, whether the broker is listening, the frame resolution rule and the measured budget |
| `net.wifi` | `state` | none — the radio, the associated access point, the scan list and the call counters |
| `net.wifi` | `enable` | {on, autoconnect?} (scripted) — WIFI_STATE_CHANGED; off drops the connection (refused while the `network` module is `real`: use `acos_net offline`) |
| `net.wifi` | `connect` | {ssid?, bssid?, rssi?, frequency?, link_speed?} (scripted) — associate: NETWORK_STATE_CHANGED (CONNECTED) and onAvailable on the default network |
| `net.wifi` | `disconnect` | none (scripted) — NETWORK_STATE_CHANGED (DISCONNECTED) and onLost on the default network (refused while the `network` module is `real`) |
| `net.wifi` | `rssi` | {rssi} (scripted) — change the associated RSSI; RSSI_CHANGED |
| `net.wifi` | `scan` | {results?: [{ssid, bssid?, rssi?, frequency?, capabilities?}]} (scripted) — replace the scan list and complete a scan: SCAN_RESULTS_AVAILABLE and every IScanResultsCallback |
| `net.wifi` | `reset` | none — clear the call counters and unsupported-call list |
| `media.session` | `sessions` | none — every session: package, tag, active, playback state, metadata, delivered commands |
| `media.session` | `play` | {session?} (scripted) — ISessionCallback.onPlay reaches MediaSession.Callback.onPlay |
| `media.session` | `pause` | {session?} (scripted) — onPause |
| `media.session` | `stop` | {session?} (scripted) — onStop |
| `media.session` | `next` | {session?} (scripted) — onSkipToNext |
| `media.session` | `previous` | {session?} (scripted) — onSkipToPrevious |
| `media.session` | `fast_forward` | {session?} (scripted) — onFastForward |
| `media.session` | `rewind` | {session?} (scripted) — onRewind |
| `media.session` | `seek` | {session?, position} (scripted) — onSeekTo(position ms) |
| `media.session` | `skip_to_item` | {session?, id} (scripted) — onSkipToQueueItem(id) |
| `media.session` | `custom_action` | {session?, action} (scripted) — onCustomAction(action, null) |
| `media.session` | `reset` | none — clear the recorded `unsupported` calls |
| `hw.camera` | `status` | none — the served camera table and the transaction log |
| `hw.camera` | `camera` | none — the single camera's fixed description (what `getCameraInfo` answers) |
| `hw.camera` | `characteristics` | none — the `CameraCharacteristics` the camera serves (hardware level, sensor, every metadata tag id/type/count), exactly what `getCameraCharacteristics` writes |
| `hw.camera` | `calls` | none — every transaction the app made (`{code, method, camera_id}`) |
| `hw.camera` | `reset` | none — clear the transaction log |

## Names with no module attached

A name a phone publishes that this build does not. Each row says what the app observes: a structured `UNSUPPORTED_ANDROID_API` naming the module and the attach call, or Android's own missing path (`null`) where AOSP builds a working manager from it.

| name | module | answer | why |
|---|---|---|---|
| `vibrator_manager` | `feedback.vibrator` | structured `UNSUPPORTED_ANDROID_API` | no vibrator hardware model; `Vibrator.vibrate()` is a silent no-op today |
| `vibrator` | `feedback.vibrator` | Android's missing path (`null`) | no vibrator hardware model |
| `usb` | `hw.usb` | structured `UNSUPPORTED_ANDROID_API` | no USB host/device model |
| `midi` | `hw.midi` | structured `UNSUPPORTED_ANDROID_API` | no MIDI device model |
| `launcherapps` | `system.launcher` | structured `UNSUPPORTED_ANDROID_API` | no launcher and no launcher-apps service |
| `usagestats` | `system.usage` | structured `UNSUPPORTED_ANDROID_API` | no usage-stats service (nothing records app usage) |
| `dropbox` | `system.dropbox` | structured `UNSUPPORTED_ANDROID_API` | no DropBox service |
| `device_policy` | `system.dpm` | structured `UNSUPPORTED_ANDROID_API` | no device-policy manager (no managed device/profile) |
| `batterystats` | `system.battery` | structured `UNSUPPORTED_ANDROID_API` | no battery-stats service |
| `fingerprint` | `hw.biometric` | structured `UNSUPPORTED_ANDROID_API` | no fingerprint/biometric HAL |
| `auth` | `hw.biometric` | structured `UNSUPPORTED_ANDROID_API` | no biometric authentication service |
| `print` | `system.print` | structured `UNSUPPORTED_ANDROID_API` | no print service |
| `country_detector` | `geo.country` | structured `UNSUPPORTED_ANDROID_API` | no country detector |
| `wallpaper` | `system.wallpaper` | Android's missing path (`null`) | no wallpaper service |
| `wifi` | `net.wifi` | Android's missing path (`null`) | no Wi-Fi stack (the network module owns real internet, ADR-0030) |
| `phone` | `net.telephony` | Android's missing path (`null`) | no telephony stack |
| `bluetooth` | `hw.bluetooth` | Android's missing path (`null`) | no Bluetooth stack |
| `bluetooth_manager` | `hw.bluetooth` | Android's missing path (`null`) | no Bluetooth stack |
| `nfc` | `hw.nfc` | Android's missing path (`null`) | no NFC stack |
| `media.camera` | `hw.camera` | structured `UNSUPPORTED_ANDROID_API` | no camera attached (module `hw.camera` is off) |
| `media_session` | `media.session` | structured `UNSUPPORTED_ANDROID_API` | no media-session service |
| `media_router` | `media.session` | structured `UNSUPPORTED_ANDROID_API` | no media-router service |
| `search` | `system.search` | structured `UNSUPPORTED_ANDROID_API` | no search service |
| `role` | `system.role` | structured `UNSUPPORTED_ANDROID_API` | no role manager |
| `carrier_config` | `net.telephony` | Android's missing path (`null`) | no telephony stack |
| `webviewupdate` | `webview` | Android's missing path (`null`) | no WebView provider package installed (module `webview` is off; ADR-0031) |
<!-- ACOS_MODULES_END -->
