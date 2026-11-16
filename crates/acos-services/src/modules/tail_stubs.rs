//! The long tail: a `stub` where AOSP's client can be answered honestly, `off`
//! with the AOSP reason everywhere else (plan §3.3, ADR-0029 item 3).
//!
//! Each module here owns one name from `module::TOMBSTONES`, so attaching it
//! replaces that name's tombstone and detaching puts the tombstone back. The
//! rule for what a module may answer is the AOSP decision table: a `stub` may
//! answer a method whose honest answer on a device **without** that feature is a
//! documented empty/false value, and its report says so. A name whose client
//! path AOSP builds out of `null` (or whose transaction table is not in this
//! image) stays `off` with the reason — never a fabricated success.
//!
//! | module | name | profile | answers |
//! |---|---|---|---|
//! | `hw.usb` | `usb` | stub | `getDeviceList` → empty `Bundle` (no USB device), `getCurrentFunctions` → `0` |
//! | `system.dropbox` | `dropbox` | stub | `isTagEnabled` → false, `getNextEntry[WithAttribution]` → null (no entry) |
//! | `hw.nfc` | `nfc` | off | `NfcAdapter` throws `UnsupportedOperationException` before any binder (it checks `FEATURE_NFC`), so there is no client path to answer |
//! | `system.wallpaper` | `wallpaper` | off | a null binder is AOSP's own `DisabledWallpaperManager`, a working manager |
//! | `system.print` | `print` | off | the client only looks the name up when `FEATURE_PRINTING` is reported |
//! | `system.search` | `search` | off | `SearchManager` builds nothing useful from the binder without a searchable index |
//! | `system.usage` | `usagestats` | off | the queries return `ParceledListSlice<UsageStats>`; a fabricated slice would look like recorded usage |
//! | `system.dpm` | `device_policy` | off | there is no managed device/profile to describe |
//! | `system.role` | `role` | off | `IRoleManager` lives in the Permission APEX, whose transaction table is not in this image's framework dex; a stub would be guessed codes |
//! | `geo.country` | `country_detector` | off | a country answer would be a location claim ACOS cannot back |

use serde_json::Value;

use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

/// What a stub service answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubAnswer {
    /// `IUsbManager`: no USB device is attached or configured.
    Usb,
    /// `IDropBoxManagerService`: no DropBox entry exists, no tag is enabled.
    DropBox,
}

/// The generic tail module.
pub struct TailStubModule {
    manifest: &'static ModuleManifest,
    profile: Profile,
    /// `Some` for a module that only has an `off` profile; it is the reason.
    off_reason: Option<&'static str>,
    answer: Option<StubAnswer>,
}

impl TailStubModule {
    /// The static manifest.
    #[must_use]
    pub fn manifest_ref(&self) -> &'static ModuleManifest {
        self.manifest
    }
}

const NO_CONTROLS: [ControlOp; 0] = [];

fn rejected(manifest: &'static ModuleManifest, detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: manifest.id,
        detail: detail.into(),
    }
}

macro_rules! stub_module {
    ($name:ident, $id:literal, $summary:literal, $service:literal, $descriptor:literal, $answer:expr) => {
        /// The manifest of this module.
        pub static $name: ModuleManifest = ModuleManifest {
            id: $id,
            version: "1.0.0",
            summary: $summary,
            features: &[],
            services: &[ServiceProvide {
                name: $service,
                descriptor: $descriptor,
            }],
            providers: &[],
            broadcasts: &[],
            windows: &[],
            system_apps: &[],
            requires: &[],
            requires_host_tools: &[],
            profiles: &[Profile::Stub],
            budget: Budget {
                ram_mib: 1,
                startup_ms: 2,
            },
            attach: AttachMode::Hot,
            controls: &NO_CONTROLS,
            security: SecurityClass::InProcess,
        };
    };
}

macro_rules! off_module {
    ($name:ident, $id:literal, $summary:literal, $service:literal, $descriptor:literal, $reason:literal) => {
        /// The manifest of this module.
        pub static $name: ModuleManifest = ModuleManifest {
            id: $id,
            version: "1.0.0",
            summary: $summary,
            features: &[],
            services: &[ServiceProvide {
                name: $service,
                descriptor: $descriptor,
            }],
            providers: &[],
            broadcasts: &[],
            windows: &[],
            system_apps: &[],
            requires: &[],
            requires_host_tools: &[],
            // No profile but `off`: this module cannot answer honestly yet.
            profiles: &[],
            // An `off`-only module never attaches, so its session cost is zero;
            // the manifest floor is 1 (a module that cannot state a cost is not
            // schedulable), and the report says the profile is off.
            budget: Budget {
                ram_mib: 1,
                startup_ms: 1,
            },
            attach: AttachMode::Hot,
            controls: &NO_CONTROLS,
            security: SecurityClass::InProcess,
        };
    };
}

stub_module!(
    USB,
    "hw.usb",
    "USB host: stub (no USB hardware), empty device list and no configured functions",
    "usb",
    "android.hardware.usb.IUsbManager",
    StubAnswer::Usb
);
stub_module!(
    DROPBOX,
    "system.dropbox",
    "DropBox: stub (no system DropBox), no entries and no enabled tags",
    "dropbox",
    "com.android.internal.os.IDropBoxManagerService",
    StubAnswer::DropBox
);

off_module!(
    NFC,
    "hw.nfc",
    "NFC: off (no NFC hardware)",
    "nfc",
    "android.nfc.INfcAdapter",
    "NfcAdapter checks FEATURE_NFC before any ServiceManager lookup and throws UnsupportedOperationException on a device without it, so there is no binder path a stub could answer"
);
off_module!(
    WALLPAPER,
    "system.wallpaper",
    "wallpaper: off (no wallpaper service; AOSP's DisabledWallpaperManager is the honest manager)",
    "wallpaper",
    "android.app.IWallpaperManager",
    "SystemServiceRegistry turns a null binder into DisabledWallpaperManager (config_enableWallpaperService false), AOSP's own working disabled manager; an IWallpaperManager stub would have to invent wallpaper state"
);
off_module!(
    PRINT,
    "system.print",
    "print: off (no print service; the client is feature-gated)",
    "print",
    "android.print.IPrintManager",
    "SystemServiceRegistry only looks the name up when PackageManager reports FEATURE_PRINTING; ACOS reports the feature absent, so a stub would be unreachable"
);
off_module!(
    SEARCH,
    "system.search",
    "search: off (no search service and no searchable index)",
    "search",
    "android.app.ISearchManager",
    "SearchManager answers from a searchable index ACOS does not have; a stub would return empty SearchableInfo lists that look like an installed search provider"
);
off_module!(
    USAGE,
    "system.usage",
    "usage stats: off (nothing records app usage)",
    "usagestats",
    "android.app.usage.IUsageStatsManager",
    "the queries return ParceledListSlice<UsageStats>; a fabricated empty slice would be indistinguishable from 'the app really was not used'"
);
off_module!(
    DPM,
    "system.dpm",
    "device policy: off (no managed device or profile)",
    "device_policy",
    "android.app.admin.IDevicePolicyManager",
    "there is no managed device or work profile to describe; every answer (isDeviceManaged, owners, admins) would be invented policy state"
);
off_module!(
    ROLE,
    "system.role",
    "role manager: off (no role manager; the AIDL table is outside this image's framework dex)",
    "role",
    "android.app.role.IRoleManager",
    "IRoleManager lives in the Permission APEX (framework-permission), whose transaction table is not in the framework dex this build reads; a stub would have to guess transaction codes"
);
off_module!(
    COUNTRY,
    "geo.country",
    "country detector: off (no location/country service)",
    "country_detector",
    "android.location.ICountryDetector",
    "a detected country is a location claim ACOS cannot back; the module stays off rather than returning an invented ISO code"
);

/// Every tail module, in a stable order.
#[must_use]
pub fn modules() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(TailStubModule {
            manifest: &USB,
            profile: Profile::Off,
            off_reason: None,
            answer: Some(StubAnswer::Usb),
        }),
        Box::new(TailStubModule {
            manifest: &DROPBOX,
            profile: Profile::Off,
            off_reason: None,
            answer: Some(StubAnswer::DropBox),
        }),
        Box::new(TailStubModule {
            manifest: &NFC,
            profile: Profile::Off,
            off_reason: Some(
                "no NFC hardware: NfcAdapter throws UnsupportedOperationException before any binder (FEATURE_NFC is absent)",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &WALLPAPER,
            profile: Profile::Off,
            off_reason: Some(
                "no wallpaper service; AOSP's own DisabledWallpaperManager is the working missing path (config_enableWallpaperService is false)",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &PRINT,
            profile: Profile::Off,
            off_reason: Some(
                "no print service; the client only resolves the name when FEATURE_PRINTING is reported, and ACOS reports it absent",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &SEARCH,
            profile: Profile::Off,
            off_reason: Some(
                "no search service and no searchable index; an empty SearchableInfo list would look like an installed search provider",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &USAGE,
            profile: Profile::Off,
            off_reason: Some(
                "nothing records app usage; an empty ParceledListSlice<UsageStats> would be indistinguishable from 'not used'",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &DPM,
            profile: Profile::Off,
            off_reason: Some(
                "no managed device or profile; every device-policy answer would be invented policy state",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &ROLE,
            profile: Profile::Off,
            off_reason: Some(
                "IRoleManager lives in the Permission APEX; its transaction table is not in the framework dex this build reads, so a stub would guess codes",
            ),
            answer: None,
        }),
        Box::new(TailStubModule {
            manifest: &COUNTRY,
            profile: Profile::Off,
            off_reason: Some(
                "a detected country is a location claim ACOS cannot back",
            ),
            answer: None,
        }),
    ]
}

struct StubService {
    manifest: &'static ModuleManifest,
    answer: StubAnswer,
}

impl ServiceObject for StubService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{}: {} has no stub answer", self.manifest.id, method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        let expected = self.manifest.services[0].descriptor;
        if interface != expected {
            return Ok(false);
        }
        let bad = |method: &str, detail: String| ServiceError::BadRequest {
            interface: expected.to_string(),
            method: method.to_string(),
            detail,
        };
        match (self.answer, code) {
            // `void getDeviceList(out Bundle devices)` (IUsbManager code 1):
            // an empty Bundle is "no USB device attached".
            (StubAnswer::Usb, 1) => {
                write_no_exception(reply);
                crate::parcelable::write_bundle(reply, Some(&crate::value::Bundle::new()))?;
                Ok(true)
            }
            // `long getCurrentFunctions()` (IUsbManager code 26): 0 = none.
            (StubAnswer::Usb, 26) => {
                write_no_exception(reply);
                reply.write_int64(0);
                Ok(true)
            }
            // `boolean isTagEnabled(String tag)` (IDropBoxManagerService code 3):
            // a system without the DropBox service enables no tag.
            (StubAnswer::DropBox, 3) => {
                let _tag = data
                    .read_string16()
                    .map_err(|error| bad("isTagEnabled", error.to_string()))?;
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // `Entry getNextEntry(String tag, long msec, int flags)` (code 4) and
            // `getNextEntryWithAttribution(...)` (code 5): a null typed object.
            (StubAnswer::DropBox, 4) => {
                let _tag = data
                    .read_string16()
                    .map_err(|error| bad("getNextEntry", error.to_string()))?;
                let _msec = data
                    .read_int64()
                    .map_err(|error| bad("getNextEntry", error.to_string()))?;
                let _flags = data
                    .read_int32()
                    .map_err(|error| bad("getNextEntry", error.to_string()))?;
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            (StubAnswer::DropBox, 5) => {
                let _tag = data
                    .read_string16()
                    .map_err(|error| bad("getNextEntryWithAttribution", error.to_string()))?;
                let _msec = data
                    .read_int64()
                    .map_err(|error| bad("getNextEntryWithAttribution", error.to_string()))?;
                let _pkg = data
                    .read_string16()
                    .map_err(|error| bad("getNextEntryWithAttribution", error.to_string()))?;
                let _feature = data
                    .read_string16()
                    .map_err(|error| bad("getNextEntryWithAttribution", error.to_string()))?;
                let _flags = data
                    .read_int32()
                    .map_err(|error| bad("getNextEntryWithAttribution", error.to_string()))?;
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl Module for TailStubModule {
    fn manifest(&self) -> &'static ModuleManifest {
        self.manifest
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        _config: &ModuleConfig,
    ) -> Result<()> {
        if let Some(reason) = self.off_reason {
            return Err(rejected(
                self.manifest,
                format!("{} has no `{profile}` profile: {reason}", self.manifest.id),
            ));
        }
        if profile != Profile::Stub {
            return Err(rejected(
                self.manifest,
                format!("{} implements only `stub`", self.manifest.id),
            ));
        }
        let Some(answer) = self.answer else {
            return Err(rejected(self.manifest, "module has no stub answer"));
        };
        let service = &self.manifest.services[0];
        host.register_dynamic_named(
            service.name,
            service.descriptor,
            Box::new(StubService {
                manifest: self.manifest,
                answer,
            }),
        );
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        Err(rejected(
            self.manifest,
            format!("{} has no control ops (asked for {op:?})", self.manifest.id),
        ))
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(self.manifest, self.profile);
        if !self.profile.is_attached() {
            report.reason = Some(match self.off_reason {
                Some(reason) => format!("off: {reason}"),
                None => format!(
                    "module is off; attach it: {}",
                    crate::module::attach_call(self.manifest.id, Profile::Stub)
                ),
            });
        } else {
            report.reason = Some(match self.answer {
                Some(StubAnswer::Usb) => {
                    "stub profile: no USB host/device model; getDeviceList answers an empty Bundle and getCurrentFunctions 0"
                        .to_string()
                }
                Some(StubAnswer::DropBox) => {
                    "stub profile: no system DropBox; isTagEnabled is false and every getNextEntry is null"
                        .to_string()
                }
                None => "stub profile: no answer".to_string(),
            });
        }
        report
    }
}
