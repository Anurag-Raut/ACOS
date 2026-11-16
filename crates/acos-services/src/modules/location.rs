//! `location` — the location service module (stub, scripted).
//!
//! The module owns the `ServiceManager` name `location`
//! (`android.location.ILocationManager`, API 34) and serves the subset of the
//! framework interface a `LocationManager` / `Geocoder` client actually uses.
//! It has **no `real` profile**: a sandbox has no GPS, no network trilateration
//! and no geocoder backend, so `scripted` (the agent drives every fix) is the
//! honest top profile.
//!
//! ## Wire evidence
//!
//! * Interface: `ILocationManager.aidl` (`android-14.0.0_r75`,
//!   `location/java/android/location/ILocationManager.aidl`). AIDL numbers
//!   transactions 1..N in declaration order; [`codes`] names the ones this
//!   module serves. The mapping is confirmed by the existing core node for this
//!   descriptor, which answers `geocoderIsPresent` as code **12**
//!   (`crate::extras::LOCATION_TRANSACTION_GEOCODER_IS_PRESENT`), matching
//!   `isGeocodeAvailable`'s position in the AIDL.
//! * `Location.writeToParcel` (`core/java/android/location/Location.java:1067`):
//!   provider String8, fields mask, time i64, elapsedRealtimeNs i64,
//!   [elapsedRealtimeUncertaintyNs f64], lat f64, lng f64, [altitude f64],
//!   [speed f32], [bearing f32], [accuracy f32], [vertical accuracy f32],
//!   [speed accuracy f32], [bearing accuracy f32], [MSL altitude f64],
//!   [MSL accuracy f32], extras Bundle. Field presence is the mask written by
//!   the setters (`HAS_*_MASK` at `Location.java:98-108`).
//! * `LastLocationRequest.writeToParcel` (`LastLocationRequest.java:125`): three
//!   booleans (`hiddenFromAppOps`, `adasGnssBypass`, `locationSettingsIgnored`).
//! * `LocationRequest.writeToParcel` (`LocationRequest.java:794`): provider
//!   String16, interval i64, quality i32, expireAt i64, duration i64, maxUpdates
//!   i32, minUpdateInterval i64, minUpdateDistance f32, maxUpdateDelay i64, four
//!   booleans, WorkSource parcelable.
//! * `PendingIntent.writeToParcel` (`app/PendingIntent.java:1500`) is a single
//!   strong binder (`mTarget`, the `IIntentSender`).
//! * `ILocationListener` (`oneway`): `onLocationChanged(List<Location>,
//!   IRemoteCallback)` = 1, `onProviderEnabledChanged(String, boolean)` = 2,
//!   `onFlushComplete(int)` = 3. `ILocationCallback` (`oneway`):
//!   `onLocation(Location)` = 1. `IGeocodeCallback` (`oneway`): `onError(String)`
//!   = 1, `onResults(List<Address>)` = 2.
//! * `Address.writeToParcel` (`Address.java:533`): locale language/country
//!   String16, address-line count + (index, line) pairs, feature name,
//!   admin area, sub-admin area, locality, sub-locality, thoroughfare,
//!   sub-thoroughfare, premises, postal code, country code, country name,
//!   has-latitude + latitude, has-longitude + longitude, phone, URL, extras.
//!
//! ## Framework behaviour reused (not invented)
//!
//! * **Permission**: `LocationManagerService.getLastLocation` /
//!   `registerLocationListener` / `getCurrentLocation` call
//!   `LocationPermissions.enforceLocationPermission(uid, level, PERMISSION_COARSE)`
//!   (`LocationManagerService.java:772-800, 980-998`), which throws
//!   `SecurityException` when the caller has neither `ACCESS_FINE_LOCATION` nor
//!   `ACCESS_COARSE_LOCATION`. A coarse caller gets a coarsened fix
//!   (`LocationProviderManager.getPermittedLocation` ->
//!   `LocationFudger.createCoarse`): bearing/speed/altitude/extras removed,
//!   accuracy raised to at least 200 m, coordinates snapped to a 200 m grid plus
//!   a slowly moving random offset. Until the permissions module exists the
//!   launch config stands in for the grant (`config.permissions`).
//! * `getLastLocation` returns `null` for a provider that does not exist
//!   (never an exception) and `null` when the location setting is off
//!   (`LocationProviderManager.getLastLocation` -> `isActive`).
//! * `isProviderEnabledForUser` answers `false` for an unknown provider
//!   (`LocationManagerService.LocalService.isProviderEnabledForUser`), and the
//!   gps/network/fused providers are only enabled while the location setting is
//!   on. The passive provider mirrors other providers and stays enabled
//!   (a real device answers `true` for `isProviderEnabled("passive")` with
//!   location off).
//! * `getAllProviders` lists the visible providers (`gps`, `network`, `fused`,
//!   `passive`); `getProviders(criteria, enabledOnly)` answers an empty list
//!   without a location permission (`LocationManagerService.java:697-700`).
//! * `reverseGeocode`/`forwardGeocode` with no geocoder provider answer
//!   `callback.onError(null)` (`LocationManagerService.java:1380-1410`) and
//!   `isGeocodeAvailable` answers `false`. That is the framework's own
//!   "no geocoder" path and is what `profile: off` / an unscripted geocoder
//!   produce here.
//! * `getCurrentLocation` is a one-shot request: the callback receives exactly
//!   one `Location` (or `null`), and the returned `ICancellationSignal` cancels
//!   it (`LocationManagerService.java:772`).
//!
//! ## What is scripted, and what is not implemented
//!
//! * Every fix the module ever delivers is recorded (provider, lat/lng,
//!   accuracy, timestamps) and summarised in [`ModuleReport::reason`], so a
//!   verdict can cite what the session actually received.
//! * `play_track` delivers one point per `interval_ms` on a thread, so updates
//!   arrive at real timing; `loop` repeats the track until `play_track` is
//!   called again, `stop_track` runs, or the module detaches.
//! * The PendingIntent form of `requestLocationUpdates` is **not delivered**:
//!   the guest cannot create a `PendingIntent` in this build (ATMS has no
//!   `getIntentSender`), and delivering one needs `IIntentSender.send` with a
//!   marshalled `Intent`, which no host writer exists for. The registration is
//!   refused with a structured error naming that gap, never accepted silently.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};
use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::{write_no_exception, EX_ILLEGAL_ARGUMENT, EX_SECURITY};
use crate::host::{CallResult, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// The `ServiceManager` name this module owns.
pub const SERVICE_NAME: &str = "location";
/// The AIDL descriptor of the interface it serves.
pub const DESCRIPTOR: &str = "android.location.ILocationManager";
/// Descriptor of the listener interface the module calls back into.
pub const LISTENER_DESCRIPTOR: &str = "android.location.ILocationListener";
/// Descriptor of the one-shot callback interface.
pub const CALLBACK_DESCRIPTOR: &str = "android.location.ILocationCallback";
/// Descriptor of the geocode callback.
pub const GEOCODE_CALLBACK_DESCRIPTOR: &str = "android.location.provider.IGeocodeCallback";
/// Descriptor of the cancellation signal returned by `getCurrentLocation`.
pub const CANCELLATION_DESCRIPTOR: &str = "android.os.ICancellationSignal";
/// Descriptor of the `PendingIntent` target the pending-intent form would drive.
pub const INTENT_SENDER_DESCRIPTOR: &str = "android.content.IIntentSender";
/// The module id.
pub const MODULE_ID: &str = "location";

/// `ILocationManager` transaction codes this module serves (AIDL order).
pub mod codes {
    /// `Location getLastLocation(String, LastLocationRequest, String, String)`.
    pub const GET_LAST_LOCATION: u32 = 1;
    /// `ICancellationSignal getCurrentLocation(...)`.
    pub const GET_CURRENT_LOCATION: u32 = 2;
    /// `void registerLocationListener(...)`.
    pub const REGISTER_LOCATION_LISTENER: u32 = 3;
    /// `void unregisterLocationListener(ILocationListener)`.
    pub const UNREGISTER_LOCATION_LISTENER: u32 = 4;
    /// `void registerLocationPendingIntent(...)`.
    pub const REGISTER_LOCATION_PENDING_INTENT: u32 = 5;
    /// `void unregisterLocationPendingIntent(PendingIntent)`.
    pub const UNREGISTER_LOCATION_PENDING_INTENT: u32 = 6;
    /// `boolean isGeocodeAvailable()`.
    pub const IS_GEOCODE_AVAILABLE: u32 = 12;
    /// `void reverseGeocode(ReverseGeocodeRequest, IGeocodeCallback)`.
    pub const REVERSE_GEOCODE: u32 = 13;
    /// `void forwardGeocode(ForwardGeocodeRequest, IGeocodeCallback)`.
    pub const FORWARD_GEOCODE: u32 = 14;
    /// `boolean hasProvider(String)`.
    pub const HAS_PROVIDER: u32 = 36;
    /// `List<String> getAllProviders()`.
    pub const GET_ALL_PROVIDERS: u32 = 37;
    /// `List<String> getProviders(Criteria, boolean)`.
    pub const GET_PROVIDERS: u32 = 38;
    /// `boolean isProviderEnabledForUser(String, int)`.
    pub const IS_PROVIDER_ENABLED_FOR_USER: u32 = 47;
    /// `boolean isLocationEnabledForUser(int)`.
    pub const IS_LOCATION_ENABLED_FOR_USER: u32 = 48;
    /// `void setLocationEnabledForUser(boolean, int)`.
    pub const SET_LOCATION_ENABLED_FOR_USER: u32 = 49;
    /// `List<GnssAntennaInfo> getGnssAntennaInfos()` — no GNSS hardware.
    pub const GET_GNSS_ANTENNA_INFOS: u32 = 18;
    /// `int getGnssYearOfHardware()`.
    pub const GET_GNSS_YEAR_OF_HARDWARE: u32 = 16;
    /// `String getGnssHardwareModelName()`.
    pub const GET_GNSS_HARDWARE_MODEL_NAME: u32 = 17;
    /// `void requestListenerFlush(String, ILocationListener, int)`.
    pub const REQUEST_LISTENER_FLUSH: u32 = 8;
}

/// `ILocationListener` transaction codes.
pub mod listener_codes {
    /// `onLocationChanged(List<Location>, IRemoteCallback)`.
    pub const ON_LOCATION_CHANGED: u32 = 1;
    /// `onProviderEnabledChanged(String, boolean)`.
    pub const ON_PROVIDER_ENABLED_CHANGED: u32 = 2;
    /// `onFlushComplete(int)`.
    pub const ON_FLUSH_COMPLETE: u32 = 3;
}

/// `ILocationCallback` transaction codes.
pub mod callback_codes {
    /// `onLocation(Location)`.
    pub const ON_LOCATION: u32 = 1;
}

/// `IGeocodeCallback` transaction codes.
pub mod geocode_codes {
    /// `onError(String)`.
    pub const ON_ERROR: u32 = 1;
    /// `onResults(List<Address>)`.
    pub const ON_RESULTS: u32 = 2;
}

/// `ICancellationSignal.cancel()`.
pub const CANCELLATION_CANCEL: u32 = 1;

/// `Location` field-presence mask bits (`Location.java:98-108`).
mod mask {
    pub const ALTITUDE: i32 = 1 << 0;
    pub const SPEED: i32 = 1 << 1;
    pub const BEARING: i32 = 1 << 2;
    pub const ACCURACY: i32 = 1 << 3;
    pub const MOCK: i32 = 1 << 4;
}

/// The four providers an API-34 phone publishes.
pub const PROVIDERS: [&str; 4] = ["gps", "network", "fused", "passive"];

/// `LocationFudger.MIN_ACCURACY_M` (`fudger/LocationFudger.java:35`).
const COARSE_MIN_ACCURACY_M: f32 = 200.0;
/// `LocationFudger.APPROXIMATE_METERS_PER_DEGREE_AT_EQUATOR`.
const METERS_PER_DEGREE: f64 = 111_000.0;
/// How many delivered fixes the session keeps for a verdict to cite.
const RECORD_CAPACITY: usize = 256;

/// What a caller is allowed to receive (the framework's `PERMISSION_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionLevel {
    /// Neither `ACCESS_FINE_LOCATION` nor `ACCESS_COARSE_LOCATION`.
    None,
    /// `ACCESS_COARSE_LOCATION` only: coarsened fixes, no bearing/speed/altitude.
    Coarse,
    /// `ACCESS_FINE_LOCATION`.
    Fine,
}

impl PermissionLevel {
    /// Parse the launch-config spelling (`fine`, `coarse`, `none`).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fine" => Some(Self::Fine),
            "coarse" => Some(Self::Coarse),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The wire/config spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Coarse => "coarse",
            Self::Fine => "fine",
        }
    }
}

/// One location fix the scripted feed produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    /// Provider that reported it (`gps`, `network`, ...).
    pub provider: String,
    /// Degrees, `-90..=90`.
    pub latitude: f64,
    /// Degrees, `-180..=180`.
    pub longitude: f64,
    /// Horizontal accuracy in metres (`Location.hasAccuracy()` always set).
    pub accuracy: f32,
    /// Metres above the WGS-84 ellipsoid, when scripted.
    pub altitude: Option<f64>,
    /// Metres per second, when scripted.
    pub speed: Option<f32>,
    /// Degrees, when scripted.
    pub bearing: Option<f32>,
    /// `Location.time` (wall clock, ms).
    pub time_ms: i64,
    /// `Location.elapsedRealtimeNanos`.
    pub elapsed_realtime_ns: i64,
    /// `Location.isMock()`. Off by default: a device reports GNSS fixes as real,
    /// and the verdict's `profile: scripted` row is what tells the agent.
    pub mock: bool,
}

impl Fix {
    /// A fix at `lat`,`lng` with `accuracy`, stamped now.
    #[must_use]
    pub fn now(provider: impl Into<String>, latitude: f64, longitude: f64, accuracy: f32) -> Self {
        let (time_ms, elapsed_realtime_ns) = now_stamp();
        Self {
            provider: provider.into(),
            latitude,
            longitude,
            accuracy,
            altitude: None,
            speed: None,
            bearing: None,
            time_ms,
            elapsed_realtime_ns,
            mock: false,
        }
    }

    /// The verdict/control JSON row.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut row = json!({
            "provider": self.provider,
            "lat": self.latitude,
            "lng": self.longitude,
            "accuracy": self.accuracy,
            "time": self.time_ms,
            "elapsed_realtime_ns": self.elapsed_realtime_ns,
        });
        if let Value::Object(map) = &mut row {
            if let Some(altitude) = self.altitude {
                map.insert("altitude".into(), json!(altitude));
            }
            if let Some(speed) = self.speed {
                map.insert("speed".into(), json!(speed));
            }
            if let Some(bearing) = self.bearing {
                map.insert("bearing".into(), json!(bearing));
            }
        }
        row
    }
}

/// Wall clock ms and monotonic ns, the two stamps `Location` always carries.
#[must_use]
pub fn now_stamp() -> (i64, i64) {
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mono = monotonic_ns() as i64;
    (wall, mono)
}

fn monotonic_ns() -> u64 {
    use rustix::time::{clock_gettime, ClockId};
    let ts = clock_gettime(ClockId::Monotonic);
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

/// A monotonic `elapsedRealtimeNanos` source that never repeats a value.
///
/// `LocationResult.validate` requires strictly increasing monotonic stamps
/// (`core/java/android/location/LocationResult.java:171-190`), and a scripted
/// track can deliver two points inside one clock tick, so equal stamps are
/// bumped by one nanosecond.
#[derive(Clone)]
pub struct Stamp(Arc<AtomicU64>);

impl Stamp {
    /// A fresh stamp source starting at 0.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    /// The next value (never smaller than the previous one).
    #[must_use]
    pub fn next(&self) -> i64 {
        let now = monotonic_ns();
        loop {
            let previous = self.0.load(Ordering::Acquire);
            let next = now.max(previous + 1);
            if self
                .0
                .compare_exchange(previous, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return next as i64;
            }
        }
    }
}

impl Default for Stamp {
    fn default() -> Self {
        Self::new()
    }
}

/// The destination of one registered update stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sink {
    /// `ILocationListener` (the `LocationListener` form).
    Listener(BinderId),
    /// `ILocationCallback` (the `getCurrentLocation` one-shot).
    Current(BinderId),
}

/// One registered update stream.
#[derive(Clone, Debug)]
pub struct Registration {
    /// Where deliveries go.
    pub sink: Sink,
    /// Provider the caller asked for.
    pub provider: String,
    /// Package the caller claimed.
    pub package: String,
    /// `LocationRequest.getMinUpdateIntervalMillis()` (0 when not parsed).
    pub min_interval_ms: i64,
    /// `LocationRequest.getIntervalMillis()`.
    pub interval_ms: i64,
    /// Last fix delivered to this registration (for interval filtering).
    pub last_delivered: Option<Instant>,
}

/// A scripted geocoder answer set.
#[derive(Clone, Debug, Default)]
pub struct GeocoderScript {
    /// Whether `isGeocodeAvailable()` answers true.
    pub present: bool,
    /// Reverse-geocode answers, keyed by the scripted point (rounded to 5
    /// decimals) — nearest point within `max_distance_m` wins.
    pub places: Vec<ScriptedPlace>,
    /// Forward-geocode answers by name (case-insensitive, first match wins).
    pub by_name: BTreeMap<String, Vec<AddressRecord>>,
    /// Calls the module answered, for the session record.
    pub calls: Vec<Value>,
}

/// One place a reverse geocode can answer.
#[derive(Clone, Debug)]
pub struct ScriptedPlace {
    /// Latitude of the place.
    pub latitude: f64,
    /// Longitude of the place.
    pub longitude: f64,
    /// Answers are used within this radius (default 1000 m).
    pub max_distance_m: f64,
    /// Addresses returned for a query inside the radius.
    pub addresses: Vec<AddressRecord>,
}

/// One `Address` the scripted geocoder returns (`Address.writeToParcel` order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddressRecord {
    /// `Address` locale language.
    pub language: String,
    /// `Address` locale country.
    pub country: String,
    /// `Address.getAddressLine(i)` values, index order.
    pub lines: Vec<String>,
    /// `Address.getFeatureName()`.
    pub feature_name: Option<String>,
    /// `Address.getAdminArea()`.
    pub admin_area: Option<String>,
    /// `Address.getSubAdminArea()`.
    pub sub_admin_area: Option<String>,
    /// `Address.getLocality()`.
    pub locality: Option<String>,
    /// `Address.getSubLocality()`.
    pub sub_locality: Option<String>,
    /// `Address.getThoroughfare()`.
    pub thoroughfare: Option<String>,
    /// `Address.getSubThoroughfare()`.
    pub sub_thoroughfare: Option<String>,
    /// `Address.getPremises()`.
    pub premises: Option<String>,
    /// `Address.getPostalCode()`.
    pub postal_code: Option<String>,
    /// `Address.getCountryCode()`.
    pub country_code: Option<String>,
    /// `Address.getCountryName()`.
    pub country_name: Option<String>,
    /// `Address.hasLatitude()`/`getLatitude()`.
    pub latitude: Option<f64>,
    /// `Address.hasLongitude()`/`getLongitude()`.
    pub longitude: Option<f64>,
    /// `Address.getPhone()`.
    pub phone: Option<String>,
    /// `Address.getUrl()`.
    pub url: Option<String>,
}

impl AddressRecord {
    /// The control/verdict JSON row.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "language": self.language,
            "country": self.country,
            "lines": self.lines,
            "feature_name": self.feature_name,
            "locality": self.locality,
            "admin_area": self.admin_area,
            "postal_code": self.postal_code,
            "country_code": self.country_code,
            "country_name": self.country_name,
            "latitude": self.latitude,
            "longitude": self.longitude,
        })
    }
}

/// One provider's scripted state.
#[derive(Clone, Debug)]
pub struct ProviderState {
    /// Configured on/off switch (the control op and the launch config write it).
    pub enabled: bool,
    /// The last fix this provider reported.
    pub last: Option<Fix>,
    /// Monotonic counter the module uses to keep stamps increasing.
    pub last_elapsed_ns: i64,
}

/// The scripted track a `play_track` control op started.
#[derive(Clone, Debug)]
pub struct Track {
    /// What is playing, for the report.
    pub label: String,
    /// Points in play order.
    pub points: Vec<Fix>,
    /// Delay between points.
    pub interval: Duration,
    /// Whether to start over after the last point.
    pub repeat: bool,
    /// How many points were delivered so far.
    pub delivered: u64,
}

/// The shared, thread-safe module state.
#[derive(Default)]
pub struct LocationState {
    /// The profile the module is attached with.
    pub profile: Option<Profile>,
    /// Location setting (`Settings.Secure.LOCATION_MODE != OFF`).
    pub location_enabled: bool,
    /// What the caller may receive.
    pub permission: Option<PermissionLevel>,
    /// Provider state, in [`PROVIDERS`] order.
    pub providers: BTreeMap<&'static str, ProviderState>,
    /// Live registrations (`ILocationListener` and one-shot callbacks).
    pub registrations: Vec<Registration>,
    /// The scripted geocoder (`None` = the framework's "no geocoder" path).
    pub geocoder: Option<GeocoderScript>,
    /// Every fix delivered in this session, oldest first (capped).
    pub delivered: VecDeque<Value>,
    /// Fixes delivered per provider.
    pub delivered_count: u64,
    /// `getCurrentLocation` requests waiting for the next fix.
    pub pending_current: Vec<(BinderId, String, Option<BinderId>)>,
    /// Cancelled `getCurrentLocation` callback binders.
    pub cancelled: BTreeSet<u64>,
    /// Provider-enabled notifications the module emitted.
    pub provider_events: Vec<Value>,
    /// The track currently playing, for the report.
    pub track: Option<Track>,
    /// `system -> app` callbacks queued for the peer to deliver.
    pub pending: Vec<PendingDelivery>,
    /// Why a delivery was refused, for the report (`None` when clean).
    pub last_error: Option<String>,
}

impl std::fmt::Debug for LocationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocationState")
            .field("profile", &self.profile)
            .field("location_enabled", &self.location_enabled)
            .field("permission", &self.permission)
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("registrations", &self.registrations.len())
            .field("delivered_count", &self.delivered_count)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl LocationState {
    /// A state with every provider enabled and no fix yet.
    #[must_use]
    pub fn enabled(location_enabled: bool, permission: PermissionLevel) -> Self {
        let mut providers = BTreeMap::new();
        for name in PROVIDERS {
            providers.insert(
                name,
                ProviderState {
                    enabled: true,
                    last: None,
                    last_elapsed_ns: 0,
                },
            );
        }
        Self {
            profile: None,
            location_enabled,
            permission: Some(permission),
            providers,
            registrations: Vec::new(),
            geocoder: None,
            delivered: VecDeque::new(),
            delivered_count: 0,
            pending_current: Vec::new(),
            cancelled: BTreeSet::new(),
            provider_events: Vec::new(),
            track: None,
            pending: Vec::new(),
            last_error: None,
        }
    }

    /// The permission level, `None` when the module has not been configured.
    #[must_use]
    pub fn permission(&self) -> PermissionLevel {
        self.permission.unwrap_or(PermissionLevel::None)
    }

    /// `true` when the provider exists and is enabled for the current setting.
    ///
    /// gps/network/fused follow the location setting; passive stays enabled
    /// (a real device answers `true` for `isProviderEnabled("passive")` even
    /// with location off).
    #[must_use]
    pub fn provider_enabled(&self, name: &str) -> bool {
        let Some(state) = self.providers.get(name) else {
            return false;
        };
        if !state.enabled {
            return false;
        }
        name == "passive" || self.location_enabled
    }

    /// The last fix a caller with `permission` may see for `provider`.
    #[must_use]
    pub fn permitted_last(&self, provider: &str) -> Option<Fix> {
        if !self.location_enabled {
            return None;
        }
        let fix = self.providers.get(provider)?.last.clone()?;
        Some(permit(&fix, self.permission()))
    }

    /// Record one delivered fix (the verdict's citation).
    pub fn record(&mut self, fix: &Fix, sink: &str) {
        self.delivered_count += 1;
        if self.delivered.len() == RECORD_CAPACITY {
            self.delivered.pop_front();
        }
        self.delivered.push_back(json!({
            "provider": fix.provider,
            "lat": fix.latitude,
            "lng": fix.longitude,
            "accuracy": fix.accuracy,
            "time": fix.time_ms,
            "sink": sink,
        }));
    }

    /// The last delivered fix, for the report.
    #[must_use]
    pub fn last_delivered(&self) -> Option<Value> {
        self.delivered.back().cloned()
    }
}

/// Coarsen or pass through `fix` for `permission` (`LocationFudger.createCoarse`).
///
/// The random offset the framework adds is generated by `SecureRandom`; the
/// module instead snaps to the 200 m grid only and says so in the report. That
/// keeps a scripted run reproducible (the agent asserts the delivered point),
/// and it never returns a *finer* fix than the coarse permission allows.
#[must_use]
pub fn permit(fix: &Fix, permission: PermissionLevel) -> Fix {
    if permission != PermissionLevel::Coarse {
        return fix.clone();
    }
    let mut coarse = fix.clone();
    coarse.bearing = None;
    coarse.speed = None;
    coarse.altitude = None;
    let lat_granularity = COARSE_MIN_ACCURACY_M as f64 / METERS_PER_DEGREE;
    let latitude = (coarse.latitude / lat_granularity).round() * lat_granularity;
    let lon_scale = (latitude.to_radians().cos()).max(1.0 / METERS_PER_DEGREE);
    let lon_granularity = COARSE_MIN_ACCURACY_M as f64 / (METERS_PER_DEGREE * lon_scale);
    let longitude = (coarse.longitude / lon_granularity).round() * lon_granularity;
    coarse.latitude = latitude.clamp(-90.0, 90.0);
    coarse.longitude = longitude.clamp(-180.0, 180.0);
    coarse.accuracy = coarse.accuracy.max(COARSE_MIN_ACCURACY_M);
    coarse
}

/// Write one `Location` exactly as `Location.writeToParcel` does.
pub fn write_location(parcel: &mut Parcel, fix: &Fix) -> Result<()> {
    parcel.write_string8(&fix.provider)?;
    let mut fields = mask::ACCURACY;
    if fix.altitude.is_some() {
        fields |= mask::ALTITUDE;
    }
    if fix.speed.is_some() {
        fields |= mask::SPEED;
    }
    if fix.bearing.is_some() {
        fields |= mask::BEARING;
    }
    if fix.mock {
        fields |= mask::MOCK;
    }
    parcel.write_int32(fields);
    parcel.write_int64(fix.time_ms);
    parcel.write_int64(fix.elapsed_realtime_ns);
    parcel.write_double(fix.latitude);
    parcel.write_double(fix.longitude);
    if let Some(altitude) = fix.altitude.filter(|_| fix.mock || fields & mask::ALTITUDE != 0) {
        parcel.write_double(altitude);
    }
    if let Some(speed) = fix.speed {
        parcel.write_float(speed);
    }
    if let Some(bearing) = fix.bearing {
        parcel.write_float(bearing);
    }
    parcel.write_float(fix.accuracy);
    // mExtras: a null Bundle is one int32 `-1`.
    parcel.write_int32(-1);
    Ok(())
}

/// Write a `Location` as a nullable typed object (`readTypedObject`).
pub fn write_location_opt(parcel: &mut Parcel, fix: Option<&Fix>) -> Result<()> {
    match fix {
        None => parcel.write_int32(0),
        Some(fix) => {
            parcel.write_int32(1);
            write_location(parcel, fix)?;
        }
    }
    Ok(())
}

/// Write an `Address` exactly as `Address.writeToParcel` does.
pub fn write_address(parcel: &mut Parcel, address: &AddressRecord) -> Result<()> {
    parcel.write_string16(&address.language)?;
    parcel.write_string16(&address.country)?;
    parcel.write_int32(i32::try_from(address.lines.len()).unwrap_or(0));
    for (index, line) in address.lines.iter().enumerate() {
        parcel.write_int32(i32::try_from(index).unwrap_or(0));
        parcel.write_string16(line)?;
    }
    for field in [
        &address.feature_name,
        &address.admin_area,
        &address.sub_admin_area,
        &address.locality,
        &address.sub_locality,
        &address.thoroughfare,
        &address.sub_thoroughfare,
        &address.premises,
        &address.postal_code,
        &address.country_code,
        &address.country_name,
    ] {
        parcel.write_string16_opt(field.as_deref())?;
    }
    match address.latitude {
        Some(latitude) => {
            parcel.write_int32(1);
            parcel.write_double(latitude);
        }
        None => parcel.write_int32(0),
    }
    match address.longitude {
        Some(longitude) => {
            parcel.write_int32(1);
            parcel.write_double(longitude);
        }
        None => parcel.write_int32(0),
    }
    parcel.write_string16_opt(address.phone.as_deref())?;
    parcel.write_string16_opt(address.url.as_deref())?;
    // extras: a null Bundle.
    parcel.write_int32(-1);
    Ok(())
}

/// Write `List<Address>` as `writeTypedList` does.
pub fn write_addresses(parcel: &mut Parcel, addresses: &[AddressRecord]) -> Result<()> {
    parcel.write_int32(i32::try_from(addresses.len()).unwrap_or(0));
    for address in addresses {
        // `writeTypedObject` writes the presence word first.
        parcel.write_int32(1);
        write_address(parcel, address)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Control operations
// ---------------------------------------------------------------------------

const CONTROL_OPS: [ControlOp; 9] = [
    ControlOp {
        name: "set_location",
        args: "{lat, lng, provider?, accuracy?, altitude?, speed?, bearing?, mock?} — one fix",
    },
    ControlOp {
        name: "play_track",
        args: "{points: [{lat, lng, ...}], interval_ms, loop?, provider?} — updates at real timing",
    },
    ControlOp {
        name: "stop_track",
        args: "none — stops a running play_track",
    },
    ControlOp {
        name: "set_provider_enabled",
        args: "{provider, enabled} — toggles a provider and notifies its listeners",
    },
    ControlOp {
        name: "set_location_enabled",
        args: "{enabled} — the Settings.Secure location setting",
    },
    ControlOp {
        name: "set_permission",
        args: "{level: fine|coarse|none} — what callers may receive",
    },
    ControlOp {
        name: "set_geocoder",
        args: "{present?, places?, by_name?} — scripted geocoder answers (`present:false` = none)",
    },
    ControlOp {
        name: "delivered",
        args: "none — every fix delivered in this session (a verdict cites this)",
    },
    ControlOp {
        name: "reset",
        args: "none — clears delivered fixes and the geocoder script",
    },
];

/// The manifest. Static, so `Module::manifest` can hand out `&'static`.
fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "location: scripted providers, fixes, tracks and geocoder (no GPS in a sandbox)",
        services: &[ServiceProvide {
            name: SERVICE_NAME,
            descriptor: DESCRIPTOR,
        }],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Stub, Profile::Scripted],
        budget: Budget {
            ram_mib: 2,
            startup_ms: 10,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module: shared state plus the track thread.
pub struct LocationModule {
    state: Arc<Mutex<LocationState>>,
    stop: Arc<AtomicBool>,
    track: Option<std::thread::JoinHandle<()>>,
    /// Monotonic stamp source shared with the service node and the track thread.
    last_stamp: Stamp,
    profile: Profile,
}

impl LocationModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LocationState::default())),
            stop: Arc::new(AtomicBool::new(false)),
            track: None,
            last_stamp: Stamp::new(),
            profile: Profile::Off,
        }
    }

    /// The manifest (tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }

    fn lock(&self) -> MutexGuard<'_, LocationState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stop the track thread (detach and `stop_track`).
    fn stop_track_thread(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.track.take() {
            let _ = handle.join();
        }
        self.stop = Arc::new(AtomicBool::new(false));
    }

    /// The `delivered` control answer.
    fn delivered_json(&self) -> Value {
        let state = self.lock();
        json!({
            "count": state.delivered_count,
            "fixes": state.delivered.iter().cloned().collect::<Vec<_>>(),
            "registrations": state.registrations.len(),
            "track": state.track.as_ref().map(|track| json!({
                "label": track.label,
                "points": track.points.len(),
                "interval_ms": track.interval.as_millis(),
                "loop": track.repeat,
                "delivered": track.delivered,
            })),
        })
    }
}

impl Default for LocationModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Push `fix` to the registrations that match it and resolve pending
/// `getCurrentLocation` callbacks. Returns the number of deliveries queued.
fn deliver_to_registrations(state: &mut LocationState, stamp: &Stamp, mut fix: Fix) -> usize {
    if !state.location_enabled {
        return 0;
    }
    fix.elapsed_realtime_ns = stamp.next();
    if let Some(provider) = state.providers.get_mut(fix.provider.as_str()) {
        provider.last = Some(fix.clone());
        provider.last_elapsed_ns = fix.elapsed_realtime_ns;
    } else {
        state.last_error = Some(format!(
            "provider {:?} does not exist; fix not stored",
            fix.provider
        ));
        return 0;
    }
    let permission = state.permission();
    let mut delivered = 0;
    let mut i = 0;
    while i < state.registrations.len() {
        let (provider, sink) = {
            let registration = &state.registrations[i];
            (registration.provider.clone(), registration.sink)
        };
        // gps/network receive their own fixes; fused and passive mirror every
        // provider (the scripted fused stream is the union of all fixes).
        let interested = provider == fix.provider
            || provider == "fused"
            || provider == "passive";
        let allowed = state.provider_enabled(&fix.provider);
        if !interested || !allowed {
            i += 1;
            continue;
        }
        let permitted = permit(&fix, permission);
        let delivery = match sink {
            Sink::Listener(binder) => location_delivery(binder, &permitted),
            Sink::Current(binder) => current_delivery(binder, &permitted),
        };
        state.pending.push(delivery);
        state.delivered_count += 1;
        if state.delivered.len() == RECORD_CAPACITY {
            state.delivered.pop_front();
        }
        state.delivered.push_back(json!({
            "provider": permitted.provider,
            "lat": permitted.latitude,
            "lng": permitted.longitude,
            "accuracy": permitted.accuracy,
            "time": permitted.time_ms,
            "sink": match sink { Sink::Listener(_) => "listener", Sink::Current(_) => "current" },
        }));
        delivered += 1;
        if matches!(sink, Sink::Current(_)) {
            state.registrations.remove(i);
        } else {
            i += 1;
        }
    }
    // One-shot callbacks that were waiting for any fix.
    let mut still_waiting = Vec::new();
    for (callback, provider, _) in std::mem::take(&mut state.pending_current) {
        if provider == fix.provider || provider == "fused" || provider == "passive" {
            let permitted = permit(&fix, permission);
            state.pending.push(current_delivery(callback, &permitted));
            state.delivered_count += 1;
            if state.delivered.len() == RECORD_CAPACITY {
                state.delivered.pop_front();
            }
            state.delivered.push_back(json!({
                "provider": permitted.provider,
                "lat": permitted.latitude,
                "lng": permitted.longitude,
                "accuracy": permitted.accuracy,
                "time": permitted.time_ms,
                "sink": "current",
            }));
            delivered += 1;
        } else {
            still_waiting.push((callback, provider, None));
        }
    }
    state.pending_current = still_waiting;
    delivered
}

/// `ILocationListener.onLocationChanged(List<Location>, IRemoteCallback)`.
fn location_delivery(target: BinderId, fix: &Fix) -> PendingDelivery {
    let mut data = Parcel::new();
    // oneway: no exception header on the wire for the request.
    let _ = data.write_interface_token(LISTENER_DESCRIPTOR);
    let _ = data.write_int32(1); // writeTypedList size
    let _ = data.write_int32(1); // element present
    let _ = write_location(&mut data, fix);
    let _ = data.write_strong_binder_opt(None); // IRemoteCallback
    PendingDelivery {
        target,
        interface: LISTENER_DESCRIPTOR.to_string(),
        code: listener_codes::ON_LOCATION_CHANGED,
        method: "onLocationChanged".into(),
        data,
    }
}

/// `ILocationCallback.onLocation(Location)`.
fn current_delivery(target: BinderId, fix: &Fix) -> PendingDelivery {
    let mut data = Parcel::new();
    let _ = data.write_interface_token(CALLBACK_DESCRIPTOR);
    let _ = write_location_opt(&mut data, Some(fix));
    PendingDelivery {
        target,
        interface: CALLBACK_DESCRIPTOR.to_string(),
        code: callback_codes::ON_LOCATION,
        method: "onLocation".into(),
        data,
    }
}

/// `ILocationListener.onProviderEnabledChanged(String, boolean)`.
fn provider_delivery(target: BinderId, provider: &str, enabled: bool) -> PendingDelivery {
    let mut data = Parcel::new();
    let _ = data.write_interface_token(LISTENER_DESCRIPTOR);
    let _ = data.write_string16(provider);
    let _ = data.write_int32(i32::from(enabled));
    PendingDelivery {
        target,
        interface: LISTENER_DESCRIPTOR.to_string(),
        code: listener_codes::ON_PROVIDER_ENABLED_CHANGED,
        method: "onProviderEnabledChanged".into(),
        data,
    }
}

/// `IGeocodeCallback.onError(String)` / `onResults(List<Address>)`.
fn geocode_delivery(
    target: BinderId,
    result: std::result::Result<&[AddressRecord], Option<&str>>,
) -> PendingDelivery {
    let mut data = Parcel::new();
    let _ = data.write_interface_token(GEOCODE_CALLBACK_DESCRIPTOR);
    match result {
        Ok(addresses) => {
            // Oneway callbacks carry only their arguments: the transaction code
            // is the code, never a parcel word.
            let _ = write_addresses(&mut data, addresses);
            PendingDelivery {
                target,
                interface: GEOCODE_CALLBACK_DESCRIPTOR.to_string(),
                code: geocode_codes::ON_RESULTS,
                method: "onResults".into(),
                data,
            }
        }
        Err(message) => {
            let _ = data.write_string16_opt(message);
            PendingDelivery {
                target,
                interface: GEOCODE_CALLBACK_DESCRIPTOR.to_string(),
                code: geocode_codes::ON_ERROR,
                method: "onError".into(),
                data,
            }
        }
    }
}

/// The `ILocationManager` node.
pub struct LocationService {
    state: Arc<Mutex<LocationState>>,
}

impl LocationService {
    fn lock(&self) -> MutexGuard<'_, LocationState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn security(&self, detail: &str) -> ServiceError {
        let mut state = self.lock();
        state.last_error = Some(detail.to_string());
        ServiceError::android(
            EX_SECURITY,
            format!(
                "{detail}: neither ACCESS_FINE_LOCATION nor ACCESS_COARSE_LOCATION was granted \
                 to this session (the launch config grants it: services: \
                 [{{id: \"location\", profile: \"scripted\", config: {{\"permissions\": \"fine\"}}}}])"
            ),
        )
    }

    fn check_permission(&self, what: &str) -> Result<()> {
        let level = self.lock().permission();
        if level == PermissionLevel::None {
            return Err(self.security(what));
        }
        Ok(())
    }

    fn illegal_argument(&self, detail: String) -> ServiceError {
        let mut state = self.lock();
        state.last_error = Some(detail.clone());
        ServiceError::android(EX_ILLEGAL_ARGUMENT, detail)
    }

    /// Read `provider`, `LastLocationRequest`, `packageName`, `attributionTag`.
    fn read_last_location_args(
        &self,
        data: &mut Parcel,
    ) -> Result<(String, String)> {
        let provider = data.read_string16()?.unwrap_or_default();
        // LastLocationRequest: three booleans.
        let _hidden_from_app_ops = data.read_int32()?;
        let _adas_gnss_bypass = data.read_int32()?;
        let _location_settings_ignored = data.read_int32()?;
        let package = data.read_string16()?.unwrap_or_default();
        let _attribution_tag = data.read_string8()?;
        Ok((provider, package))
    }

    /// Read a `LocationRequest` and return `(provider, min_interval_ms,
    /// interval_ms)`.
    fn read_location_request(data: &mut Parcel) -> Result<(String, i64, i64)> {
        let provider = data.read_string16()?.unwrap_or_default();
        let interval_ms = data.read_int64()?;
        let _quality = data.read_int32()?;
        let _expire_at = data.read_int64()?;
        let _duration = data.read_int64()?;
        let _max_updates = data.read_int32()?;
        let min_interval_ms = data.read_int64()?;
        let _min_update_distance = data.read_float()?;
        let _max_update_delay = data.read_int64()?;
        let _hide_from_app_ops = data.read_int32()?;
        let _adas_gnss_bypass = data.read_int32()?;
        let _bypass = data.read_int32()?;
        let _low_power = data.read_int32()?;
        // WorkSource: a typed object (0 when null) the module does not act on.
        let _work_source_present = data.read_int32_lenient();
        Ok((provider, min_interval_ms, interval_ms))
    }
}

impl ServiceObject for LocationService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!(
                "{MODULE_ID}: {} is not a decoded AIDL method; this node answers the raw wire form",
                method.name
            ),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            // @nullable Location getLastLocation(String, LastLocationRequest,
            // String, String)
            codes::GET_LAST_LOCATION => {
                self.check_permission("getLastKnownLocation")?;
                let (provider, _package) = self.read_last_location_args(data)?;
                if !self.lock().providers.contains_key(provider.as_str()) {
                    return Err(self.illegal_argument(format!(
                        "invalid provider \"{provider}\": provider does not exist"
                    )));
                }
                let fix = self.lock().permitted_last(&provider);
                write_no_exception(reply);
                write_location_opt(reply, fix.as_ref())?;
            }
            // @nullable ICancellationSignal getCurrentLocation(String,
            // LocationRequest, ILocationCallback, String, String, String)
            codes::GET_CURRENT_LOCATION => {
                self.check_permission("getCurrentLocation")?;
                let provider = data.read_string16()?.unwrap_or_default();
                let (_request_provider, _min_interval, _interval) =
                    Self::read_location_request(data)?;
                let Some(callback) = data.read_strong_binder()? else {
                    return Err(self.illegal_argument(
                        "getCurrentLocation: null ILocationCallback".into(),
                    ));
                };
                let _package = data.read_string16()?;
                let _attribution_tag = data.read_string8()?;
                let _listener_id = data.read_string16()?;
                if !self.lock().providers.contains_key(provider.as_str()) {
                    return Err(self.illegal_argument(format!(
                        "provider \"{provider}\" does not exist"
                    )));
                }
                {
                    let mut state = self.lock();
                    match state.permitted_last(&provider) {
                        Some(fix) => {
                            // A cached fix answers immediately, exactly like a
                            // provider that already has one (the callback is
                            // one-shot and is never registered).
                            state.pending.push(current_delivery(callback, &fix));
                            state.record(&fix, "current");
                        }
                        None => state
                            .pending_current
                            .push((callback, provider.clone(), None)),
                    }
                }
                // `@nullable ICancellationSignal`: this module answers null and
                // says so (`cancellation: "unsupported"` in the report). The
                // client's `CancellationSignal.setRemote(null)` is a no-op, so a
                // cancelled request still resolves on the next fix rather than
                // hanging.
                write_no_exception(reply);
                reply.write_int32(0);
            }
            // void registerLocationListener(String, LocationRequest,
            // ILocationListener, String, String, String)
            codes::REGISTER_LOCATION_LISTENER => {
                self.check_permission("requestLocationUpdates")?;
                let provider = data.read_string16()?.unwrap_or_default();
                let (_request_provider, min_interval_ms, interval_ms) =
                    Self::read_location_request(data)?;
                let Some(listener) = data.read_strong_binder()? else {
                    return Err(self.illegal_argument(
                        "registerLocationListener: null ILocationListener".into(),
                    ));
                };
                let package = data.read_string16()?.unwrap_or_default();
                let _attribution_tag = data.read_string8()?;
                let _listener_id = data.read_string16()?;
                if !self.lock().providers.contains_key(provider.as_str()) {
                    return Err(self.illegal_argument(format!(
                        "provider \"{provider}\" does not exist"
                    )));
                }
                {
                    let mut state = self.lock();
                    state.registrations.retain(|registration| {
                        !matches!(registration.sink, Sink::Listener(existing) if existing == listener)
                    });
                    state.registrations.push(Registration {
                        sink: Sink::Listener(listener),
                        provider: provider.clone(),
                        package,
                        min_interval_ms,
                        interval_ms,
                        last_delivered: None,
                    });
                    // A registered listener also gets the provider's enabled
                    // state as a first callback: the framework's provider sends
                    // `onProviderEnabledChanged` when the registration is added
                    // (`LocationProviderManager.onProviderEnabledChanged`).
                    if let Some(enabled) = state.providers.get(provider.as_str()).map(|_| {
                        state.provider_enabled(&provider)
                    }) {
                        state.pending.push(provider_delivery(listener, &provider, enabled));
                        state.provider_events.push(json!({
                            "provider": provider,
                            "enabled": enabled,
                            "reason": "registration",
                        }));
                    }
                }
                write_no_exception(reply);
            }
            // void unregisterLocationListener(ILocationListener)
            codes::UNREGISTER_LOCATION_LISTENER => {
                let listener = data.read_strong_binder()?;
                let mut state = self.lock();
                state.registrations.retain(|registration| {
                    !matches!(registration.sink, Sink::Listener(existing) if Some(existing) == listener)
                });
                write_no_exception(reply);
            }
            // void registerLocationPendingIntent(String, LocationRequest,
            // PendingIntent, String, String)
            codes::REGISTER_LOCATION_PENDING_INTENT => {
                self.check_permission("requestLocationUpdates(PendingIntent)")?;
                let provider = data.read_string16()?.unwrap_or_default();
                let (_request_provider, _min_interval, _interval) =
                    Self::read_location_request(data)?;
                let pending_present = data.read_int32_lenient();
                let target = if pending_present != 0 {
                    data.read_strong_binder()?
                } else {
                    None
                };
                let _package = data.read_string16()?;
                let _attribution_tag = data.read_string8()?;
                let mut state = self.lock();
                state.last_error = Some(format!(
                    "registerLocationPendingIntent({provider}) refused: delivering location \
                     through a PendingIntent needs IIntentSender.send with a marshalled Intent, \
                     and this build has no Intent parcel writer (ATMS has no getIntentSender)"
                ));
                if target.is_none() {
                    return Err(self.illegal_argument(
                        "registerLocationPendingIntent: null PendingIntent".into(),
                    ));
                }
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: DESCRIPTOR.to_string(),
                    code: INTENT_SENDER_CODE,
                    method: format!(
                        "{MODULE_ID}: registerLocationPendingIntent({provider}) has no delivery \
                         path in this build (no IIntentSender.send / Intent parcel writer); use \
                         the LocationListener form"
                    ),
                });
            }
            // void unregisterLocationPendingIntent(PendingIntent)
            codes::UNREGISTER_LOCATION_PENDING_INTENT => {
                let _present = data.read_int32_lenient();
                let _target = data.read_strong_binder()?;
                write_no_exception(reply);
            }
            // boolean isGeocodeAvailable()
            codes::IS_GEOCODE_AVAILABLE => {
                let available = self
                    .lock()
                    .geocoder
                    .as_ref()
                    .is_some_and(|script| script.present);
                write_no_exception(reply);
                reply.write_int32(i32::from(available));
            }
            // void reverseGeocode(ReverseGeocodeRequest, IGeocodeCallback)
            codes::REVERSE_GEOCODE => {
                let latitude = data.read_double()?;
                let longitude = data.read_double()?;
                let max_results = data.read_int32()?;
                let language = data.read_string8()?;
                let country = data.read_string8()?;
                let variant = data.read_string8()?;
                let _uid = data.read_int32()?;
                let package = data.read_string8()?.unwrap_or_default();
                let _attribution_tag = data.read_string8()?;
                let callback = data.read_strong_binder()?.ok_or_else(|| {
                    self.illegal_argument("reverseGeocode: null IGeocodeCallback".into())
                })?;
                let answers = {
                    let mut state = self.lock();
                    let answer = reverse_geocode(
                        state.geocoder.as_ref(),
                        latitude,
                        longitude,
                        max_results.max(1) as usize,
                    );
                    state.geocoder.as_mut().map(|script| {
                        script.calls.push(json!({
                            "op": "reverse",
                            "lat": latitude,
                            "lng": longitude,
                            "max_results": max_results,
                            "language": language,
                            "country": country,
                            "variant": variant,
                            "package": package,
                            "results": answer.as_ref().map(|a| a.len()).unwrap_or(0),
                        }))
                    });
                    answer
                };
                let delivery = match answers {
                    Some(addresses) => geocode_delivery(callback, Ok(&addresses)),
                    // The framework's no-geocoder path: `callback.onError(null)`.
                    None => geocode_delivery(callback, Err(None)),
                };
                self.lock().pending.push(delivery);
                write_no_exception(reply);
            }
            // void forwardGeocode(ForwardGeocodeRequest, IGeocodeCallback)
            codes::FORWARD_GEOCODE => {
                let name = data.read_string8()?.unwrap_or_default();
                let _lower_left_lat = data.read_double()?;
                let _lower_left_lng = data.read_double()?;
                let _upper_right_lat = data.read_double()?;
                let _upper_right_lng = data.read_double()?;
                let max_results = data.read_int32()?;
                let language = data.read_string8()?;
                let country = data.read_string8()?;
                let variant = data.read_string8()?;
                let _uid = data.read_int32()?;
                let package = data.read_string8()?.unwrap_or_default();
                let _attribution_tag = data.read_string8()?;
                let callback = data.read_strong_binder()?.ok_or_else(|| {
                    self.illegal_argument("forwardGeocode: null IGeocodeCallback".into())
                })?;
                let answers = {
                    let mut state = self.lock();
                    let answer = forward_geocode(
                        state.geocoder.as_ref(),
                        &name,
                        max_results.max(1) as usize,
                    );
                    state.geocoder.as_mut().map(|script| {
                        script.calls.push(json!({
                            "op": "forward",
                            "name": name,
                            "max_results": max_results,
                            "language": language,
                            "country": country,
                            "variant": variant,
                            "package": package,
                            "results": answer.as_ref().map(|a| a.len()).unwrap_or(0),
                        }))
                    });
                    answer
                };
                let delivery = match answers {
                    Some(addresses) => geocode_delivery(callback, Ok(&addresses)),
                    None => geocode_delivery(callback, Err(None)),
                };
                self.lock().pending.push(delivery);
                write_no_exception(reply);
            }
            // boolean hasProvider(String)
            codes::HAS_PROVIDER => {
                let provider = data.read_string16()?.unwrap_or_default();
                let has = self.lock().providers.contains_key(provider.as_str());
                write_no_exception(reply);
                reply.write_int32(i32::from(has));
            }
            // List<String> getAllProviders()
            codes::GET_ALL_PROVIDERS => {
                let names: Vec<&str> = self
                    .lock()
                    .providers
                    .keys()
                    .copied()
                    .collect();
                write_no_exception(reply);
                reply.write_string16_array(Some(&names))?;
            }
            // List<String> getProviders(Criteria, boolean)
            codes::GET_PROVIDERS => {
                // Criteria: a typed object (0 when null), then `enabledOnly`.
                let criteria_present = data.read_int32_lenient();
                if criteria_present != 0 {
                    // The criteria body is not needed: KitKat and later always
                    // resolve criteria requests to the fused provider, and this
                    // module does not filter by accuracy/power.
                }
                let enabled_only = data.read_int32_lenient() != 0;
                let level = self.lock().permission();
                let names: Vec<&str> = if level == PermissionLevel::None {
                    // `LocationManagerService.getProviders` answers an empty list
                    // without a location permission.
                    Vec::new()
                } else {
                    let state = self.lock();
                    state
                        .providers
                        .keys()
                        .copied()
                        .filter(|name| !enabled_only || state.provider_enabled(name))
                        .collect()
                };
                write_no_exception(reply);
                reply.write_string16_array(Some(&names))?;
            }
            // boolean isProviderEnabledForUser(String, int)
            codes::IS_PROVIDER_ENABLED_FOR_USER => {
                let provider = data.read_string16()?.unwrap_or_default();
                let _user = data.read_int32()?;
                let enabled = self.lock().provider_enabled(&provider);
                write_no_exception(reply);
                reply.write_int32(i32::from(enabled));
            }
            // boolean isLocationEnabledForUser(int)
            codes::IS_LOCATION_ENABLED_FOR_USER => {
                let _user = data.read_int32()?;
                let enabled = self.lock().location_enabled;
                write_no_exception(reply);
                reply.write_int32(i32::from(enabled));
            }
            // void setLocationEnabledForUser(boolean, int) — system API the
            // framework enforces WRITE_SECURE_SETTINGS for; this module only
            // accepts it from the control channel, so a guest call is refused
            // rather than silently changing the session's state.
            codes::SET_LOCATION_ENABLED_FOR_USER => {
                return Err(ServiceError::android(
                    EX_SECURITY,
                    "setLocationEnabledForUser requires WRITE_SECURE_SETTINGS; drive it with \
                     acos_service_ctl {module: \"location\", op: \"set_location_enabled\"}"
                        .to_string(),
                ));
            }
            // List<GnssAntennaInfo> getGnssAntennaInfos() — no GNSS hardware.
            codes::GET_GNSS_ANTENNA_INFOS => {
                // A null typed list is `-1` (`Parcel.writeTypedList(null)`), which
                // is what a device without antenna info reports.
                write_no_exception(reply);
                reply.write_int32(-1);
            }
            // int getGnssYearOfHardware()
            codes::GET_GNSS_YEAR_OF_HARDWARE => {
                write_no_exception(reply);
                reply.write_int32(0);
            }
            // String getGnssHardwareModelName()
            codes::GET_GNSS_HARDWARE_MODEL_NAME => {
                write_no_exception(reply);
                reply.write_string16("")?;
            }
            // void requestListenerFlush(String, ILocationListener, int): the
            // scripted feed has no batch to flush, and the framework's flush
            // contract is "answer onFlushComplete". Refuse instead of claiming a
            // flush that never happened.
            codes::REQUEST_LISTENER_FLUSH => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: DESCRIPTOR.to_string(),
                    code,
                    method: format!(
                        "{MODULE_ID}: requestListenerFlush has no scripted batch to flush"
                    ),
                });
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.lock().pending)
    }
}

/// Transaction code used in the `registerLocationPendingIntent` refusal message
/// (it names the missing delivery path, not an interface this module serves).
const INTENT_SENDER_CODE: u32 = 0;

/// `LocationManagerService.reverseGeocode`: nearest scripted place within its
/// radius, or `None` for the framework's no-geocoder path.
#[must_use]
pub fn reverse_geocode(
    script: Option<&GeocoderScript>,
    latitude: f64,
    longitude: f64,
    max_results: usize,
) -> Option<Vec<AddressRecord>> {
    let script = script?;
    if !script.present {
        return None;
    }
    let mut best: Option<(&ScriptedPlace, f64)> = None;
    for place in &script.places {
        let distance = distance_m(latitude, longitude, place.latitude, place.longitude);
        let radius = if place.max_distance_m > 0.0 {
            place.max_distance_m
        } else {
            1000.0
        };
        if distance <= radius && best.map(|(_, d)| distance < d).unwrap_or(true) {
            best = Some((place, distance));
        }
    }
    Some(
        best.map(|(place, _)| place.addresses.clone())
            .unwrap_or_default()
            .into_iter()
            .take(max_results)
            .collect(),
    )
}

/// `LocationManagerService.forwardGeocode`: name lookup, or `None` for the
/// framework's no-geocoder path.
#[must_use]
pub fn forward_geocode(
    script: Option<&GeocoderScript>,
    name: &str,
    max_results: usize,
) -> Option<Vec<AddressRecord>> {
    let script = script?;
    if !script.present {
        return None;
    }
    let key = name.trim().to_lowercase();
    let addresses = script
        .by_name
        .iter()
        .find(|(candidate, _)| candidate.to_lowercase() == key)
        .map(|(_, addresses)| addresses.clone())
        .unwrap_or_default();
    Some(addresses.into_iter().take(max_results).collect())
}

/// Great-circle distance in metres (haversine).
#[must_use]
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let dphi = (lat2 - lat1).to_radians();
    let dlambda = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (dlambda / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

// Control op parsing helpers -------------------------------------------------

fn require_f64(args: &Value, key: &str) -> Result<f64> {
    args.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("op needs a numeric `{key}`, got {args}"),
        })
}

fn optional_f32(args: &Value, key: &str) -> Result<Option<f32>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(|v| Some(v as f32))
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("`{key}` must be a number, got {value}"),
            }),
    }
}

fn optional_f64(args: &Value, key: &str) -> Result<Option<f64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("`{key}` must be a number, got {value}"),
            }),
    }
}

/// Build a [`Fix`] from a control payload.
fn fix_from_json(args: &Value, default_provider: &str) -> Result<Fix> {
    let latitude = require_f64(args, "lat")?;
    let longitude = require_f64(args, "lng")?;
    if !(-90.0..=90.0).contains(&latitude) {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("`lat` must be within -90..90, got {latitude}"),
        });
    }
    if !(-180.0..=180.0).contains(&longitude) {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("`lng` must be within -180..180, got {longitude}"),
        });
    }
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or(default_provider)
        .to_string();
    if !PROVIDERS.contains(&provider.as_str()) {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!(
                "unknown provider {provider:?}; this module has {}",
                PROVIDERS.join(", ")
            ),
        });
    }
    let (time_ms, elapsed_realtime_ns) = now_stamp();
    Ok(Fix {
        provider,
        latitude,
        longitude,
        accuracy: args
            .get("accuracy")
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .unwrap_or(5.0),
        altitude: optional_f64(args, "altitude")?,
        speed: optional_f32(args, "speed")?,
        bearing: optional_f32(args, "bearing")?,
        time_ms,
        elapsed_realtime_ns,
        mock: args.get("mock").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Parse one `Address` from JSON (the control payload).
fn address_from_json(value: &Value) -> Result<AddressRecord> {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let lines = value
        .get("lines")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(AddressRecord {
        language: string("language").unwrap_or_else(|| "en".into()),
        country: string("country").unwrap_or_else(|| "US".into()),
        lines,
        feature_name: string("feature_name"),
        admin_area: string("admin_area"),
        sub_admin_area: string("sub_admin_area"),
        locality: string("locality"),
        sub_locality: string("sub_locality"),
        thoroughfare: string("thoroughfare"),
        sub_thoroughfare: string("sub_thoroughfare"),
        premises: string("premises"),
        postal_code: string("postal_code"),
        country_code: string("country_code"),
        country_name: string("country_name"),
        latitude: value.get("latitude").and_then(Value::as_f64),
        longitude: value.get("longitude").and_then(Value::as_f64),
        phone: string("phone"),
        url: string("url"),
    })
}

/// Parse a `set_geocoder` payload.
fn geocoder_from_json(args: &Value) -> Result<GeocoderScript> {
    let mut script = GeocoderScript {
        present: args
            .get("present")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        ..GeocoderScript::default()
    };
    if let Some(places) = args.get("places").and_then(Value::as_array) {
        for place in places {
            let addresses = place
                .get("addresses")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(address_from_json)
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            script.places.push(ScriptedPlace {
                latitude: require_f64(place, "lat")?,
                longitude: require_f64(place, "lng")?,
                max_distance_m: place
                    .get("max_distance_m")
                    .and_then(Value::as_f64)
                    .unwrap_or(1000.0),
                addresses,
            });
        }
    }
    if let Some(by_name) = args.get("by_name").and_then(Value::as_object) {
        for (name, addresses) in by_name {
            let addresses = addresses
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(address_from_json)
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            script.by_name.insert(name.clone(), addresses);
        }
    }
    Ok(script)
}

impl Module for LocationModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: a sandbox has no GPS, no network \
                     trilateration and no geocoder backend; use `stub` or `scripted`"
                ),
            });
        }
        let location_enabled = config
            .get("location_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let permission = match config.string("permissions") {
            Some(text) => PermissionLevel::parse(text).ok_or_else(|| {
                ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!(
                        "`permissions` is {text:?}; use one of none, coarse, fine"
                    ),
                }
            })?,
            // The launch config grants the manifest's location permissions until
            // the permissions module exists; `fine` is what a fixture that
            // declares ACCESS_FINE_LOCATION gets.
            None => PermissionLevel::Fine,
        };
        let mut state = LocationState::enabled(location_enabled, permission);
        state.profile = Some(profile);
        let enabled_providers = config.string_array("providers");
        if !enabled_providers.is_empty() {
            for name in &enabled_providers {
                if !PROVIDERS.contains(&name.as_str()) {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "config.providers names unknown provider {name:?}; this module has {}",
                            PROVIDERS.join(", ")
                        ),
                    });
                }
                if let Some(provider) = state.providers.get_mut(name.as_str()) {
                    provider.enabled = true;
                }
            }
            for provider in state.providers.values_mut() {
                provider.enabled = false;
            }
            for name in &enabled_providers {
                if let Some(provider) = state.providers.get_mut(name.as_str()) {
                    provider.enabled = true;
                }
            }
        }
        if let Some(geocoder) = config.get("geocoder") {
            state.geocoder = Some(geocoder_from_json(geocoder)?);
        }
        if let Some(initial) = config.get("initial") {
            let fix = fix_from_json(initial, "gps")?;
            if let Some(provider) = state.providers.get_mut(fix.provider.as_str()) {
                provider.last = Some(fix);
            }
        }
        // The attached state replaces the module's own, so a second attach
        // always starts from the config (never from a stale session).
        self.state = Arc::new(Mutex::new(state));
        self.last_stamp = Stamp::new();
        // `register_dynamic_named` replaces the core install's offline node for
        // this name (a device with no location module still publishes it), so a
        // session never carries two binders with one descriptor.
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(LocationService {
                state: Arc::clone(&self.state),
            }),
        );
        self.profile = profile;
        Ok(())
    }

    /// While the module is off, `location` keeps answering the disabled device
    /// state: this is core's offline node (`extras::LocationService`), which is
    /// what a phone with every provider off answers — `isGeocodeAvailable`
    /// false, and a structured error for every provider call.
    fn off_node(&self) -> Option<Box<dyn ServiceObject>> {
        Some(Box::new(crate::extras::LocationService::new()))
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.stop_track_thread();
        {
            let mut state = self.lock();
            state.pending.clear();
            state.registrations.clear();
            state.pending_current.clear();
            state.track = None;
        }
        self.state = Arc::new(Mutex::new(LocationState::default()));
        self.last_stamp = Stamp::new();
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, op: &str, args: &Value) -> Result<Value> {
        if !self.profile.is_attached() {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        }
        match op {
            "set_location" => {
                let fix = fix_from_json(args, "gps")?;
                let stored = {
                    let mut state = self.lock();
                    if !state.providers.contains_key(fix.provider.as_str()) {
                        return Err(ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail: format!("provider {:?} does not exist", fix.provider),
                        });
                    }
                    let enabled = state.provider_enabled(&fix.provider);
                    let location_enabled = state.location_enabled;
                    let delivered = deliver_to_registrations(&mut state, &self.last_stamp, fix.clone());
                    let stored = state.providers.get(fix.provider.as_str()).and_then(|p| p.last.clone());
                    (delivered, stored, enabled, location_enabled)
                };
                let (delivered, stored, enabled, location_enabled) = stored;
                Ok(json!({
                    "provider": fix.provider,
                    "lat": fix.latitude,
                    "lng": fix.longitude,
                    "accuracy": fix.accuracy,
                    "stored": stored.is_some(),
                    "provider_enabled": enabled,
                    "location_enabled": location_enabled,
                    "delivered": delivered,
                }))
            }
            "play_track" => {
                let interval_ms = args
                    .get("interval_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(1000)
                    .max(1);
                let repeat = args.get("loop").and_then(Value::as_bool).unwrap_or(false);
                let default_provider = args
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("gps");
                let points = args
                    .get("points")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("play_track needs `points: [...]`, got {args}"),
                    })?;
                if points.is_empty() {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "play_track needs at least one point".into(),
                    });
                }
                let mut fixes = Vec::new();
                for point in points {
                    fixes.push(fix_from_json(point, default_provider)?);
                }
                let label = args
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("track")
                    .to_string();
                self.stop_track_thread();
                let stop = Arc::clone(&self.stop);
                let state = Arc::clone(&self.state);
                let stamp = self.last_stamp.clone();
                let track = Track {
                    label: label.clone(),
                    points: fixes.clone(),
                    interval: Duration::from_millis(interval_ms),
                    repeat,
                    delivered: 0,
                };
                self.lock().track = Some(track.clone());
                let handle = std::thread::Builder::new()
                    .name("acos-location-track".into())
                    .spawn(move || {
                        let mut index = 0usize;
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }
                            std::thread::sleep(track.interval);
                            if stop.load(Ordering::Acquire) {
                                return;
                            }
                            let Some(mut fix) = track.points.get(index).cloned() else {
                                return;
                            };
                            fix.time_ms = now_stamp().0;
                            {
                                let mut state = state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                deliver_to_registrations(&mut state, &stamp, fix);
                                if let Some(track) = state.track.as_mut() {
                                    track.delivered += 1;
                                }
                            }
                            index += 1;
                            if index >= track.points.len() {
                                if track.repeat {
                                    index = 0;
                                } else {
                                    return;
                                }
                            }
                        }
                    })
                    .map_err(|error| ServiceError::Internal(format!(
                        "play_track thread: {error}"
                    )))?;
                self.track = Some(handle);
                Ok(json!({
                    "track": label,
                    "points": fixes.len(),
                    "interval_ms": interval_ms,
                    "loop": repeat,
                }))
            }
            "stop_track" => {
                self.stop_track_thread();
                let stopped = self.lock().track.take().is_some();
                Ok(json!({"stopped": stopped}))
            }
            "set_provider_enabled" => {
                let provider = args
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("set_provider_enabled needs `provider`, got {args}"),
                    })?
                    .to_string();
                let enabled = args
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("set_provider_enabled needs `enabled`, got {args}"),
                    })?;
                let mut state = self.lock();
                let Some(provider_state) = state.providers.get_mut(provider.as_str()) else {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "unknown provider {provider:?}; this module has {}",
                            PROVIDERS.join(", ")
                        ),
                    });
                };
                provider_state.enabled = enabled;
                let effective = state.provider_enabled(&provider);
                let sinks: Vec<BinderId> = state
                    .registrations
                    .iter()
                    .filter(|registration| registration.provider == provider)
                    .filter_map(|registration| match registration.sink {
                        Sink::Listener(binder) => Some(binder),
                        Sink::Current(_) => None,
                    })
                    .collect();
                for sink in &sinks {
                    state
                        .pending
                        .push(provider_delivery(*sink, &provider, effective));
                }
                state.provider_events.push(json!({
                    "provider": provider,
                    "enabled": effective,
                    "configured": enabled,
                    "listeners": sinks.len(),
                }));
                Ok(json!({
                    "provider": provider,
                    "enabled": effective,
                    "configured": enabled,
                    "listeners_notified": sinks.len(),
                }))
            }
            "set_location_enabled" => {
                let enabled = args
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("set_location_enabled needs `enabled`, got {args}"),
                    })?;
                let mut state = self.lock();
                state.location_enabled = enabled;
                // gps/network/fused follow the setting; passive does not.
                let changes: Vec<(&'static str, bool)> = PROVIDERS
                    .iter()
                    .map(|name| (*name, state.provider_enabled(name)))
                    .collect();
                let mut notified = 0usize;
                for (name, effective) in changes {
                    let sinks: Vec<BinderId> = state
                        .registrations
                        .iter()
                        .filter(|registration| registration.provider == name)
                        .filter_map(|registration| match registration.sink {
                            Sink::Listener(binder) => Some(binder),
                            Sink::Current(_) => None,
                        })
                        .collect();
                    for sink in &sinks {
                        state
                            .pending
                            .push(provider_delivery(*sink, name, effective));
                    }
                    notified += sinks.len();
                    state.provider_events.push(json!({
                        "provider": name,
                        "enabled": effective,
                        "reason": "location_setting",
                    }));
                }
                Ok(json!({"location_enabled": enabled, "listeners_notified": notified}))
            }
            "set_permission" => {
                let level = args
                    .get("level")
                    .and_then(Value::as_str)
                    .and_then(PermissionLevel::parse)
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "set_permission needs `level: fine|coarse|none`, got {args}"
                        ),
                    })?;
                self.lock().permission = Some(level);
                Ok(json!({"permission": level.as_str()}))
            }
            "set_geocoder" => {
                let script = geocoder_from_json(args)?;
                let present = script.present;
                let places = script.places.len();
                let names = script.by_name.len();
                self.lock().geocoder = Some(script);
                Ok(json!({"present": present, "places": places, "names": names}))
            }
            "delivered" => Ok(self.delivered_json()),
            "reset" => {
                let mut state = self.lock();
                state.delivered.clear();
                state.delivered_count = 0;
                state.geocoder = None;
                Ok(json!({"reset": true}))
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module implements {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let manifest = manifest_ref();
        let mut report = ModuleReport::of(manifest, self.profile);
        if !self.profile.is_attached() {
            return report;
        }
        let state = self.lock();
        let last = state.last_delivered();
        let last_text = last
            .map(|row| {
                format!(
                    "last {} lat {} lng {}",
                    row.get("provider").and_then(Value::as_str).unwrap_or("?"),
                    row.get("lat").and_then(Value::as_f64).unwrap_or(0.0),
                    row.get("lng").and_then(Value::as_f64).unwrap_or(0.0)
                )
            })
            .unwrap_or_else(|| "no fix delivered yet".into());
        report.reason = Some(format!(
            "{} profile: scripted providers ({}), location setting {}, permission {}, \
             {} register(s), {} fix(es) delivered to the app ({last_text}); geocoder {}",
            self.profile,
            PROVIDERS
                .iter()
                .map(|name| format!("{name}={}", state.provider_enabled(name)))
                .collect::<Vec<_>>()
                .join(","),
            if state.location_enabled { "on" } else { "off" },
            state.permission().as_str(),
            state.registrations.len(),
            state.delivered_count,
            match state.geocoder.as_ref() {
                Some(script) if script.present => "scripted",
                Some(_) => "not present",
                None => "not present (framework no-geocoder path)",
            }
        ));
        if let Some(error) = &state.last_error {
            report.reason = Some(format!(
                "{}; last error: {error}",
                report.reason.unwrap_or_default()
            ));
        }
        report
    }
}


// ---------------------------------------------------------------------------
// `profile: off` node
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(permission: PermissionLevel) -> LocationState {
        let mut state = LocationState::enabled(true, permission);
        state.profile = Some(Profile::Scripted);
        state
    }

    #[test]
    fn a_fix_round_trips_through_the_location_parcel_layout() {
        let mut fix = Fix::now("gps", 37.4220, -122.0841, 4.0);
        fix.altitude = Some(12.5);
        fix.speed = Some(3.5);
        fix.bearing = Some(90.0);
        let mut parcel = Parcel::new();
        write_location(&mut parcel, &fix).unwrap();
        parcel.set_data_position(0);
        assert_eq!(parcel.read_string8().unwrap().as_deref(), Some("gps"));
        let mask = parcel.read_int32().unwrap();
        assert_eq!(mask & mask::ACCURACY, mask::ACCURACY);
        assert_eq!(mask & mask::ALTITUDE, mask::ALTITUDE);
        assert_eq!(mask & mask::SPEED, mask::SPEED);
        assert_eq!(mask & mask::BEARING, mask::BEARING);
        assert_eq!(parcel.read_int64().unwrap(), fix.time_ms);
        assert_eq!(parcel.read_int64().unwrap(), fix.elapsed_realtime_ns);
        assert_eq!(parcel.read_double().unwrap(), 37.4220);
        assert_eq!(parcel.read_double().unwrap(), -122.0841);
        assert_eq!(parcel.read_double().unwrap(), 12.5);
        assert_eq!(parcel.read_float().unwrap(), 3.5);
        assert_eq!(parcel.read_float().unwrap(), 90.0);
        assert_eq!(parcel.read_float().unwrap(), 4.0);
        assert_eq!(parcel.read_int32().unwrap(), -1); // null extras
        assert!(parcel.read_int32().is_err(), "parcel must be fully consumed");
    }

    #[test]
    fn a_coarse_caller_gets_a_gridded_fix_without_bearing_speed_or_altitude() {
        let mut fix = Fix::now("gps", 37.4223, -122.0842, 3.0);
        fix.altitude = Some(10.0);
        fix.speed = Some(2.0);
        fix.bearing = Some(45.0);
        let coarse = permit(&fix, PermissionLevel::Coarse);
        assert!(coarse.accuracy >= COARSE_MIN_ACCURACY_M);
        assert_eq!(coarse.altitude, None);
        assert_eq!(coarse.speed, None);
        assert_eq!(coarse.bearing, None);
        assert!(distance_m(fix.latitude, fix.longitude, coarse.latitude, coarse.longitude) < 150.0);
        // Snapping is stable: the same fix twice lands on the same grid point.
        assert_eq!(coarse.latitude, permit(&fix, PermissionLevel::Coarse).latitude);
        // A fine caller sees the scripted point untouched.
        assert_eq!(permit(&fix, PermissionLevel::Fine), fix);
    }

    #[test]
    fn providers_follow_the_location_setting_except_passive() {
        let mut state = state_with(PermissionLevel::Fine);
        assert!(state.provider_enabled("gps"));
        state.location_enabled = false;
        assert!(!state.provider_enabled("gps"));
        assert!(!state.provider_enabled("network"));
        assert!(!state.provider_enabled("fused"));
        assert!(state.provider_enabled("passive"));
        assert!(!state.provider_enabled("nonesuch"));
        assert!(state.permitted_last("gps").is_none());
    }

    #[test]
    fn a_registered_listener_receives_each_fix_once_in_order() {
        let mut state = state_with(PermissionLevel::Fine);
        let listener = BinderId(0x1000_0001);
        state.registrations.push(Registration {
            sink: Sink::Listener(listener),
            provider: "gps".into(),
            package: "dev.acos.fixtures.locationprobe".into(),
            min_interval_ms: 0,
            interval_ms: 0,
            last_delivered: None,
        });
        let stamp = Stamp::new();
        let first = Fix::now("gps", 37.4220, -122.0841, 4.0);
        assert_eq!(deliver_to_registrations(&mut state, &stamp, first.clone()), 1);
        let second = Fix::now("network", 37.4221, -122.0840, 20.0);
        assert_eq!(
            deliver_to_registrations(&mut state, &stamp, second),
            0,
            "a gps listener must not receive a network fix"
        );
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.delivered.len(), 1);
        assert_eq!(
            state.pending[0].code,
            listener_codes::ON_LOCATION_CHANGED
        );
        // The delivered parcel carries the fix.
        let mut parcel = state.pending[0].data.clone();
        parcel.set_data_position(0);
        parcel.enforce_interface(LISTENER_DESCRIPTOR).unwrap();
        assert_eq!(parcel.read_int32().unwrap(), 1); // list size
        assert_eq!(parcel.read_int32().unwrap(), 1); // present
        assert_eq!(parcel.read_string8().unwrap().as_deref(), Some("gps"));
    }

    #[test]
    fn a_fused_listener_mirrors_every_provider() {
        let mut state = state_with(PermissionLevel::Fine);
        state.registrations.push(Registration {
            sink: Sink::Listener(BinderId(0x1000_0002)),
            provider: "fused".into(),
            package: "dev.acos.fixtures.locationprobe".into(),
            min_interval_ms: 0,
            interval_ms: 0,
            last_delivered: None,
        });
        let stamp = Stamp::new();
        assert_eq!(
            deliver_to_registrations(
                &mut state,
                &stamp,
                Fix::now("network", 1.0, 2.0, 30.0)
            ),
            1
        );
        assert_eq!(
            deliver_to_registrations(&mut state, &stamp, Fix::now("gps", 1.0, 2.0, 5.0)),
            1
        );
        assert_eq!(state.delivered_count, 2);
    }

    #[test]
    fn a_disabled_location_setting_delivers_nothing_and_keeps_the_last_fix() {
        let mut state = state_with(PermissionLevel::Fine);
        state.registrations.push(Registration {
            sink: Sink::Listener(BinderId(0x1000_0003)),
            provider: "gps".into(),
            package: "p".into(),
            min_interval_ms: 0,
            interval_ms: 0,
            last_delivered: None,
        });
        let stamp = Stamp::new();
        deliver_to_registrations(&mut state, &stamp, Fix::now("gps", 1.0, 2.0, 5.0));
        state.location_enabled = false;
        let delivered = deliver_to_registrations(
            &mut state,
            &stamp,
            Fix::now("gps", 3.0, 4.0, 5.0),
        );
        assert_eq!(delivered, 0);
        assert!(state.permitted_last("gps").is_none());
    }

    #[test]
    fn the_no_geocoder_path_answers_none_and_a_scripted_one_answers_addresses() {
        assert!(reverse_geocode(None, 1.0, 2.0, 1).is_none());
        assert!(forward_geocode(None, "x", 1).is_none());
        let script = GeocoderScript {
            present: true,
            places: vec![ScriptedPlace {
                latitude: 37.4220,
                longitude: -122.0841,
                max_distance_m: 500.0,
                addresses: vec![AddressRecord {
                    lines: vec!["1600 Amphitheatre Parkway".into()],
                    locality: Some("Mountain View".into()),
                    ..AddressRecord::default()
                }],
            }],
            by_name: BTreeMap::new(),
            calls: Vec::new(),
        };
        let answers = reverse_geocode(Some(&script), 37.4221, -122.0840, 5).unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].locality.as_deref(), Some("Mountain View"));
        // Outside the radius: an empty list, not an error (the query succeeded).
        let answers = reverse_geocode(Some(&script), 0.0, 0.0, 5).unwrap();
        assert!(answers.is_empty());
        // `present: false` is the framework's no-geocoder path.
        let absent = GeocoderScript {
            present: false,
            ..script
        };
        assert!(reverse_geocode(Some(&absent), 37.4221, -122.0840, 5).is_none());
    }

    #[test]
    fn an_address_parcel_matches_the_framework_field_order() {
        let address = AddressRecord {
            language: "en".into(),
            country: "US".into(),
            lines: vec!["1 Infinite Loop".into(), "Cupertino".into()],
            feature_name: Some("Apple Park".into()),
            admin_area: Some("CA".into()),
            locality: Some("Cupertino".into()),
            country_code: Some("US".into()),
            country_name: Some("United States".into()),
            latitude: Some(37.3318),
            longitude: Some(-122.0312),
            ..AddressRecord::default()
        };
        let mut parcel = Parcel::new();
        write_address(&mut parcel, &address).unwrap();
        parcel.set_data_position(0);
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("en"));
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("US"));
        assert_eq!(parcel.read_int32().unwrap(), 2); // address lines
        assert_eq!(parcel.read_int32().unwrap(), 0);
        assert_eq!(
            parcel.read_string16().unwrap().as_deref(),
            Some("1 Infinite Loop")
        );
        assert_eq!(parcel.read_int32().unwrap(), 1);
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("Cupertino"));
        assert_eq!(
            parcel.read_string16().unwrap().as_deref(),
            Some("Apple Park")
        );
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("CA"));
        assert!(parcel.read_string16().unwrap().is_none()); // sub-admin area
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("Cupertino"));
        assert!(parcel.read_string16().unwrap().is_none()); // sub-locality
        assert!(parcel.read_string16().unwrap().is_none()); // thoroughfare
        assert!(parcel.read_string16().unwrap().is_none()); // sub-thoroughfare
        assert!(parcel.read_string16().unwrap().is_none()); // premises
        assert!(parcel.read_string16().unwrap().is_none()); // postal code
        assert_eq!(parcel.read_string16().unwrap().as_deref(), Some("US"));
        assert_eq!(
            parcel.read_string16().unwrap().as_deref(),
            Some("United States")
        );
        assert_eq!(parcel.read_int32().unwrap(), 1);
        assert_eq!(parcel.read_double().unwrap(), 37.3318);
        assert_eq!(parcel.read_int32().unwrap(), 1);
        assert_eq!(parcel.read_double().unwrap(), -122.0312);
        assert!(parcel.read_string16().unwrap().is_none()); // phone
        assert!(parcel.read_string16().unwrap().is_none()); // url
        assert_eq!(parcel.read_int32().unwrap(), -1); // null extras
    }
}
