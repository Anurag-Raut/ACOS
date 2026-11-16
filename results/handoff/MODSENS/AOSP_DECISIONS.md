# `sensorservice` — a name `core` publishes and the `sensors` module fills (MODSENS)

ADR-0029 item 3 asks for a per-name AOSP decision. `sensorservice` is **not** a
tombstone and not a `null`: it is published by `core` in the honest
"device without sensors" state, and the `sensors` module installs its feed into
that same node while attached.

## Why not a tombstone

A tombstone (`UNSUPPORTED_ANDROID_API` on every transaction) is the right answer
only when AOSP's client can survive a present-but-throwing binder. The sensor
client cannot:

* `SystemSensorManager.nativeCreate` → `SensorManager::getInstanceForPackage` →
  `waitForSensorService` (`frameworks/native/libs/sensor/SensorManager.cpp:163`)
  loops 60 times on `getService("sensorservice")`; each miss costs libbinder's
  `ServiceManagerShim::getService` 5000 ms
  (`native/libs/binder/IServiceManager.cpp:309`). W26 measured the cost inside the
  launcher's 12 s resume budget: **~11.9 s**. A `null` name is therefore not
  free, and a throwing binder does not help either (`getSensorList` would throw
  out of `nativeCreate`).
* The "no sensors on this device" answer is a **real** Android answer: an empty
  `getSensorList`. That is what this build serves when the module is off
  (`verify-off.json`: the app prints `sensors: 0` and registers no listener).

## Why the node belongs to `core` and the content to the module

`SensorManager` resolves the `ISensorServer` proxy **once**, in its constructor,
and keeps it for the life of the process (`libs/sensor/SensorManager.h`:
`sp<ISensorServer> mSensorServer`). A hot attach that published a *second* binder
would be invisible to an app that already booted. So:

* `core` registers the node (`sensors::install`, no feed = the stock empty reply,
  which also keeps W26's 12 s away);
* the `sensors` module takes that node through `ServiceHost::dynamic_object_mut`
  + the documented `as_any_mut` downcast and installs a `SensorFeed` into it
  (`sensors::SensorServer::install_feed`), i.e. the **binder identity never
  changes**;
* detach clears the feed (`SensorServer::clear_feed`), so the same binder answers
  the stock empty list again and every open event connection stops.

The module's manifest therefore owns the name (`ServiceProvide { name:
"sensorservice" }`) and `core`'s manifest excludes it
(`core_module::FILLED_BY_MODULE`), because the registry refuses two owners.

## What changed for W26's placeholder (defects found while serving a real list)

1. The reply must have **no** exception header: `ISensorServer` is hand-written
   C++, so `BpSensorServer::getSensorList` reads `readUint32()` first. The
   placeholder wrote an exception word, which the client read as the count.
2. Each flattened `Sensor` must be **padded to 4 bytes**: `Parcel::writeInplace`
   reserves `pad_size(len)` and the client's `LightFlattenable` read advances by
   the padded size. A default `Sensor` is 145 bytes; without the pad every sensor
   after the first deserialises one byte late and the framework logs
   `Failed to read reply from getSensorList`, returning an empty list.
3. `isDataInjectionEnabled()` (code 3) is a known `() -> int` and now answers
   `0` instead of a named gap; `createSensorDirectConnection` (a `NativeHandle`
   channel) and `setOperationParameter` stay named gaps, because they need a HAL
   ACOS does not have.

Evidence: `crates/acos-services/src/sensors.rs` module docs (file:line for every
claim), `crates/acos-services/tests/sensors_module.rs` (the wire is decoded the
way the native proxy decodes it), `results/handoff/MODSENS/logcat-scripted.txt`
(the framework prints the decoded `Sensor` object).
