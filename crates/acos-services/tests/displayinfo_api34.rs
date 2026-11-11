//! Byte-exact API-34 `IDisplayManager.getDisplayInfo` test.
//!
//! # What is asserted
//!
//! 1. `ServiceHost` answers `IDisplayManager.getDisplayInfo(0)` (transaction
//!    code 1) with the bytes produced by `DisplayInfo.writeToParcel` — field for
//!    field, in the order of
//!    `android-14.0.0_r75 frameworks/base/core/java/android/view/DisplayInfo.java:587-647`.
//!    The expected parcel is **built here by hand**, one `write_*` call per
//!    framework line, and compared to the host's reply byte for byte.
//! 2. The same reply is decoded with a hand-written transcription of
//!    `DisplayInfo.readFromParcel` (`DisplayInfo.java:520-584`) — including
//!    `DisplayCutout.ParcelableWrapper.readCutoutFromParcel`,
//!    `Display.Mode.CREATOR.createFromParcel`, `readInt` loops and
//!    `readSparseArray` — and every field is compared to the served value.
//!    `data_avail()` must be `0`: a stale/extra field desynchronises the parcel
//!    and is exactly the pre-API-34 failure this test exists to catch.
//! 3. The variant-`1` cutout branch (`writeCutoutToParcel`) is proven the same
//!    way with the reference emulator's hole-punch geometry.
//! 4. The codec's field table is pinned to the framework's write order, so a
//!    future edit that reorders it fails here even though the DEX scanner
//!    collapses the two `for` loops.
//!
//! Image vs tag: where they disagree the **image** wins, because the image's
//! reader is what has to stay in sync — see `Display$Mode` (no `mVsyncRate` on
//! the image's `writeToParcel`).

use acos_parcel::{BinderId, Parcel};
use acos_services::display::{CutoutGeometry, DisplayCutout, DisplayInfo, RectI};
use acos_services::parcelable::{self, FieldOp, Verification};
use acos_services::{ServiceHost, Value};

const IDISPLAY_MANAGER: &str = "android.hardware.display.IDisplayManager";
/// `IDisplayManager.getDisplayInfo` transaction code (`IDisplayManager.aidl`).
const GET_DISPLAY_INFO: u32 = 1;
/// `android.app.ActivityManager`-independent fixture uid (`display.txt: A=10192`).
const FIXTURE_UID: i32 = 10192;
/// Full reply size: `writeNoException` (4) + `writeTypedObject` marker (4) +
/// `DisplayInfo.writeToParcel` for the served display (252 bytes: 51 write
/// sites, one mode, one colour mode, `NO_CUTOUT`).
const SERVED_LEN: usize = 260;

fn display_binder(host: &mut ServiceHost) -> BinderId {
    host.check_service("display")
        .expect("check_service")
        .expect("display service")
}

/// `IDisplayManager.getDisplayInfo(0)` as the framework proxy issues it.
fn get_display_info(host: &mut ServiceHost, id: i32) -> Vec<u8> {
    let binder = display_binder(host);
    let mut data = Parcel::new();
    data.write_interface_token(IDISPLAY_MANAGER)
        .expect("writeInterfaceToken");
    data.write_int32(id);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, GET_DISPLAY_INFO, &mut data, &mut reply, 0)
        .expect("getDisplayInfo");
    reply.marshall()
}

/// The reply prefix the AIDL stub writes: `writeNoException` (0) then
/// `writeTypedObject(result)` (presence marker 1).
fn reply_prefix(p: &mut Parcel) {
    p.write_int32(0); // Parcel.writeNoException
    p.write_int32(1); // writeTypedObject: non-null DisplayInfo
}

/// The served `DisplayInfo` in `writeToParcel` order, built by hand.
fn expected_served_parcel() -> Parcel {
    let mut p = Parcel::new();
    reply_prefix(&mut p);
    // DisplayInfo.java:588-592
    p.write_int32(0); // writeInt(layerStack)
    p.write_int32(0); // writeInt(flags)
    p.write_int32(1); // writeInt(type)          TYPE_INTERNAL
    p.write_int32(0); // writeInt(displayId)
    p.write_int32(0); // writeInt(displayGroupId)
    // :593-594 writeParcelable(address) / writeParcelable(deviceProductInfo):
    // a null Parcelable is `writeString(null)` = int32 -1.
    p.write_string16_opt(None).expect("address");
    p.write_string16_opt(None).expect("deviceProductInfo");
    // :595 writeString8(name)
    p.write_string8("Built-in Screen").expect("name");
    // :596-603 six app bounds + two logical bounds
    p.write_int32(1080); // appWidth
    p.write_int32(2209); // appHeight
    p.write_int32(1080); // smallestNominalAppWidth
    p.write_int32(2209); // smallestNominalAppHeight
    p.write_int32(1080); // largestNominalAppWidth
    p.write_int32(2209); // largestNominalAppHeight
    p.write_int32(1080); // logicalWidth
    p.write_int32(2400); // logicalHeight
    // :604 DisplayCutout.ParcelableWrapper.writeCutoutToParcel: variant 0 is
    // `DisplayCutout.NO_CUTOUT` (int32 only).
    p.write_int32(0);
    // :605-609
    p.write_int32(0); // rotation                 ROTATION_0
    p.write_int32(1); // modeId
    p.write_float(60.000004_f32); // renderFrameRate
    p.write_int32(1); // defaultModeId
    p.write_int32(0); // userPreferredModeId
    // :610-613 writeInt(supportedModes.length) + each Mode.writeToParcel
    p.write_int32(1);
    p.write_int32(1); // Mode.mModeId
    p.write_int32(1080); // Mode.mWidth
    p.write_int32(2400); // Mode.mHeight
    p.write_float(60.000004_f32); // Mode.mPeakRefreshRate
    p.write_float_array(Some(&[])).expect("alt refresh rates");
    p.write_int32_array(Some(&[])).expect("Mode HDR types");
    // :614-618
    p.write_int32(0); // colorMode                COLOR_MODE_SRGB
    p.write_int32_array(Some(&[0])).expect("supportedColorModes");
    // :619 writeParcelable(hdrCapabilities) — null
    p.write_string16_opt(None).expect("hdrCapabilities");
    // :620-625
    p.write_bool(false); // minimalPostProcessingSupported
    p.write_int32(420); // logicalDensityDpi
    p.write_float(420.0); // physicalXDpi
    p.write_float(420.0); // physicalYDpi
    p.write_int64(0); // appVsyncOffsetNanos
    p.write_int64(0); // presentationDeadlineNanos
    // :626-635
    p.write_int32(2); // state                    STATE_ON
    p.write_int32(2); // committedState
    p.write_int32(FIXTURE_UID); // ownerUid
    p.write_string8_opt(None).expect("ownerPackageName");
    p.write_string8_opt(None).expect("uniqueId");
    p.write_int32(0); // removeMode
    p.write_float(0.0); // refreshRateOverride
    p.write_float(0.0); // brightnessMinimum
    p.write_float(1.0); // brightnessMaximum
    p.write_float(0.5); // brightnessDefault
    // :636 writeTypedObject(roundedCorners) — null
    p.write_int32(0);
    // :637-640 writeInt(userDisabledHdrTypes.length) + ints (empty)
    p.write_int32_array(Some(&[])).expect("userDisabledHdrTypes");
    // :641-646
    p.write_int32(0); // installOrientation
    p.write_int32(0); // writeTypedObject(displayShape) — null
    p.write_int32(0); // writeTypedObject(layoutLimitedRefreshRate) — null
    p.write_float(f32::NAN); // hdrSdrRatio
    // writeSparseArray(new SparseArray<>()) — the framework default is an
    // *empty* array, which writes a 0 count (not the null -1).
    p.write_int32(0);
    // :647 writeString8(thermalBrightnessThrottlingDataId) — null
    p.write_string8_opt(None).expect("thermalBrightnessThrottlingDataId");
    p
}

/// One decoded `DisplayInfo`, read in `readFromParcel` order.
#[derive(Debug, PartialEq)]
struct Decoded {
    layer_stack: i32,
    flags: i32,
    display_type: i32,
    display_id: i32,
    display_group_id: i32,
    address: Option<String>,
    device_product_info: Option<String>,
    name: Option<String>,
    app_width: i32,
    app_height: i32,
    smallest_nominal_app_width: i32,
    smallest_nominal_app_height: i32,
    largest_nominal_app_width: i32,
    largest_nominal_app_height: i32,
    logical_width: i32,
    logical_height: i32,
    cutout_variant: i32,
    cutout: Option<CutoutGeometry>,
    rotation: i32,
    mode_id: i32,
    render_frame_rate: f32,
    default_mode_id: i32,
    user_preferred_mode_id: i32,
    supported_modes: Vec<Mode>,
    color_mode: i32,
    supported_color_modes: Vec<i32>,
    hdr_capabilities: Option<String>,
    minimal_post_processing_supported: bool,
    logical_density_dpi: i32,
    physical_x_dpi: f32,
    physical_y_dpi: f32,
    app_vsync_offset_nanos: i64,
    presentation_deadline_nanos: i64,
    state: i32,
    committed_state: i32,
    owner_uid: i32,
    owner_package_name: Option<String>,
    unique_id: Option<String>,
    remove_mode: i32,
    refresh_rate_override: f32,
    brightness_minimum: f32,
    brightness_maximum: f32,
    brightness_default: f32,
    rounded_corners_present: bool,
    user_disabled_hdr_types: Vec<i32>,
    install_orientation: i32,
    display_shape_present: bool,
    layout_limited_refresh_rate_present: bool,
    hdr_sdr_ratio: f32,
    thermal_refresh_rate_throttling: Vec<i32>,
    thermal_brightness_throttling_data_id: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Mode {
    mode_id: i32,
    width: i32,
    height: i32,
    peak_refresh_rate: f32,
    alternative_refresh_rates: Vec<f32>,
    supported_hdr_types: Vec<i32>,
}

/// `writeParcelable` = `writeString(className)` + payload; ACOS serves `null`
/// (int32 -1), which has no payload.
fn read_named_null(p: &mut Parcel) -> Option<String> {
    p.read_string16().expect("writeParcelable class name")
}

/// `DisplayCutout.ParcelableWrapper.readCutoutFromParcel`
/// (`DisplayCutout.java:1512-1540`).
fn read_cutout(p: &mut Parcel) -> (i32, Option<CutoutGeometry>) {
    let variant = p.read_int32().expect("cutout variant");
    if variant != 1 {
        return (variant, None);
    }
    let safe = read_typed_rect(p);
    let n = p.read_int32().expect("cutout bounds count");
    let mut bounds = [(0, 0, 0, 0); 4];
    assert_eq!(n, 4, "DisplayCutout has BOUNDS_POSITION_LENGTH = 4 rects");
    for slot in &mut bounds {
        *slot = read_typed_rect(p);
    }
    let waterfall = read_typed_rect(p);
    let geometry = CutoutGeometry {
        safe_insets: safe,
        bounds,
        waterfall_insets: waterfall,
        display_width: p.read_int32().unwrap(),
        display_height: p.read_int32().unwrap(),
        physical_display_width: p.read_int32().unwrap(),
        physical_display_height: p.read_int32().unwrap(),
        density: p.read_float().unwrap(),
        cutout_spec: p.read_string16().unwrap(),
        rotation: p.read_int32().unwrap(),
        scale: p.read_float().unwrap(),
        physical_pixel_display_size_ratio: p.read_float().unwrap(),
    };
    (variant, Some(geometry))
}

fn read_typed_rect(p: &mut Parcel) -> RectI {
    assert_ne!(p.read_int32().expect("typed marker"), 0, "expected a Rect");
    (
        p.read_int32().unwrap(),
        p.read_int32().unwrap(),
        p.read_int32().unwrap(),
        p.read_int32().unwrap(),
    )
}

/// `DisplayInfo.readFromParcel`, in the framework's read order.
fn decode(p: &mut Parcel) -> (Decoded, usize) {
    assert_eq!(p.read_int32().expect("exception code"), 0, "no exception");
    assert_eq!(p.read_int32().expect("typed marker"), 1, "non-null DisplayInfo");
    let layer_stack = p.read_int32().unwrap();
    let flags = p.read_int32().unwrap();
    let display_type = p.read_int32().unwrap();
    let display_id = p.read_int32().unwrap();
    let display_group_id = p.read_int32().unwrap();
    let address = read_named_null(p);
    let device_product_info = read_named_null(p);
    let name = p.read_string8().expect("name");
    let app_width = p.read_int32().unwrap();
    let app_height = p.read_int32().unwrap();
    let smallest_nominal_app_width = p.read_int32().unwrap();
    let smallest_nominal_app_height = p.read_int32().unwrap();
    let largest_nominal_app_width = p.read_int32().unwrap();
    let largest_nominal_app_height = p.read_int32().unwrap();
    let logical_width = p.read_int32().unwrap();
    let logical_height = p.read_int32().unwrap();
    let (cutout_variant, cutout) = read_cutout(p);
    let rotation = p.read_int32().unwrap();
    let mode_id = p.read_int32().unwrap();
    let render_frame_rate = p.read_float().unwrap();
    let default_mode_id = p.read_int32().unwrap();
    let user_preferred_mode_id = p.read_int32().unwrap();
    let n_modes = p.read_int32().expect("supportedModes length");
    let mut supported_modes = Vec::new();
    for _ in 0..n_modes.max(0) {
        supported_modes.push(Mode {
            mode_id: p.read_int32().unwrap(),
            width: p.read_int32().unwrap(),
            height: p.read_int32().unwrap(),
            peak_refresh_rate: p.read_float().unwrap(),
            alternative_refresh_rates: p.read_float_array().unwrap().unwrap_or_default(),
            supported_hdr_types: p.read_int32_array().unwrap().unwrap_or_default(),
        });
    }
    let color_mode = p.read_int32().unwrap();
    let n_color_modes = p.read_int32().expect("supportedColorModes length");
    let mut supported_color_modes = Vec::new();
    for _ in 0..n_color_modes.max(0) {
        supported_color_modes.push(p.read_int32().unwrap());
    }
    let hdr_capabilities = read_named_null(p);
    let minimal_post_processing_supported = p.read_bool().unwrap();
    let logical_density_dpi = p.read_int32().unwrap();
    let physical_x_dpi = p.read_float().unwrap();
    let physical_y_dpi = p.read_float().unwrap();
    let app_vsync_offset_nanos = p.read_int64().unwrap();
    let presentation_deadline_nanos = p.read_int64().unwrap();
    let state = p.read_int32().unwrap();
    let committed_state = p.read_int32().unwrap();
    let owner_uid = p.read_int32().unwrap();
    let owner_package_name = p.read_string8().unwrap();
    let unique_id = p.read_string8().unwrap();
    let remove_mode = p.read_int32().unwrap();
    let refresh_rate_override = p.read_float().unwrap();
    let brightness_minimum = p.read_float().unwrap();
    let brightness_maximum = p.read_float().unwrap();
    let brightness_default = p.read_float().unwrap();
    let rounded_corners_present = p.read_int32().expect("roundedCorners marker") != 0;
    let n_user_disabled = p.read_int32().expect("userDisabledHdrTypes length");
    let mut user_disabled_hdr_types = Vec::new();
    for _ in 0..n_user_disabled.max(0) {
        user_disabled_hdr_types.push(p.read_int32().unwrap());
    }
    let install_orientation = p.read_int32().unwrap();
    let display_shape_present = p.read_int32().expect("displayShape marker") != 0;
    let layout_limited_refresh_rate_present =
        p.read_int32().expect("layoutLimitedRefreshRate marker") != 0;
    let hdr_sdr_ratio = p.read_float().unwrap();
    let n_thermal = p.read_int32().expect("thermalRefreshRateThrottling size");
    let mut thermal = Vec::new();
    for _ in 0..n_thermal.max(0) {
        thermal.push(p.read_int32().unwrap()); // key
        let _min = p.read_float().unwrap(); // RefreshRateRange.min (writeValue)
        let _max = p.read_float().unwrap(); // RefreshRateRange.max
    }
    let thermal_brightness_throttling_data_id = p.read_string8().unwrap();
    let avail = p.data_avail();
    (
        Decoded {
            layer_stack,
            flags,
            display_type,
            display_id,
            display_group_id,
            address,
            device_product_info,
            name,
            app_width,
            app_height,
            smallest_nominal_app_width,
            smallest_nominal_app_height,
            largest_nominal_app_width,
            largest_nominal_app_height,
            logical_width,
            logical_height,
            cutout_variant,
            cutout,
            rotation,
            mode_id,
            render_frame_rate,
            default_mode_id,
            user_preferred_mode_id,
            supported_modes,
            color_mode,
            supported_color_modes,
            hdr_capabilities,
            minimal_post_processing_supported,
            logical_density_dpi,
            physical_x_dpi,
            physical_y_dpi,
            app_vsync_offset_nanos,
            presentation_deadline_nanos,
            state,
            committed_state,
            owner_uid,
            owner_package_name,
            unique_id,
            remove_mode,
            refresh_rate_override,
            brightness_minimum,
            brightness_maximum,
            brightness_default,
            rounded_corners_present,
            user_disabled_hdr_types,
            install_orientation,
            display_shape_present,
            layout_limited_refresh_rate_present,
            hdr_sdr_ratio,
            thermal_refresh_rate_throttling: thermal,
            thermal_brightness_throttling_data_id,
        },
        avail,
    )
}

fn expected_served_decoded() -> Decoded {
    Decoded {
        layer_stack: 0,
        flags: 0,
        display_type: 1,
        display_id: 0,
        display_group_id: 0,
        address: None,
        device_product_info: None,
        name: Some("Built-in Screen".into()),
        app_width: 1080,
        app_height: 2209,
        smallest_nominal_app_width: 1080,
        smallest_nominal_app_height: 2209,
        largest_nominal_app_width: 1080,
        largest_nominal_app_height: 2209,
        logical_width: 1080,
        logical_height: 2400,
        cutout_variant: 0, // DisplayCutout.NO_CUTOUT
        cutout: None,
        rotation: 0,
        mode_id: 1,
        render_frame_rate: 60.000004,
        default_mode_id: 1,
        user_preferred_mode_id: 0,
        supported_modes: vec![Mode {
            mode_id: 1,
            width: 1080,
            height: 2400,
            peak_refresh_rate: 60.000004,
            alternative_refresh_rates: vec![],
            supported_hdr_types: vec![],
        }],
        color_mode: 0,
        supported_color_modes: vec![0],
        hdr_capabilities: None,
        minimal_post_processing_supported: false,
        logical_density_dpi: 420,
        physical_x_dpi: 420.0,
        physical_y_dpi: 420.0,
        app_vsync_offset_nanos: 0,
        presentation_deadline_nanos: 0,
        state: 2,
        committed_state: 2,
        owner_uid: FIXTURE_UID,
        owner_package_name: None,
        unique_id: None,
        remove_mode: 0,
        refresh_rate_override: 0.0,
        brightness_minimum: 0.0,
        brightness_maximum: 1.0,
        brightness_default: 0.5,
        rounded_corners_present: false,
        user_disabled_hdr_types: vec![],
        install_orientation: 0,
        display_shape_present: false,
        layout_limited_refresh_rate_present: false,
        hdr_sdr_ratio: f32::NAN,
        thermal_refresh_rate_throttling: vec![],
        thermal_brightness_throttling_data_id: None,
    }
}

#[test]
fn host_serves_display_info_in_the_api34_write_order() {
    // The display override is process-wide; pin the emulator reference.
    acos_services::display::set_display_override(None);
    let mut host = ServiceHost::standard();
    let reply = get_display_info(&mut host, 0);

    let expected = expected_served_parcel().marshall();
    if std::env::var_os("ACOS_DUMP_DISPLAYINFO").is_some() {
        // Evidence for results/art-runtime/D1.md: the exact reply bytes.
        println!("getDisplayInfo reply ({} bytes):{}", reply.len(), hex(&reply));
    }
    assert_eq!(reply.len(), SERVED_LEN, "DisplayInfo size drift");
    assert_eq!(expected.len(), SERVED_LEN, "hand-built size drift");
    if reply != expected {
        let at = reply
            .iter()
            .zip(&expected)
            .position(|(a, b)| a != b)
            .unwrap_or(reply.len().min(expected.len()));
        panic!(
            "DisplayInfo bytes differ at offset {at} (reply len {}, expected len {})\n\
             reply   : {}\n expected: {}",
            reply.len(),
            expected.len(),
            hex(&reply),
            hex(&expected)
        );
    }
}

#[test]
fn host_reply_decodes_with_the_framework_read_order_and_leaves_no_bytes() {
    acos_services::display::set_display_override(None);
    let mut host = ServiceHost::standard();
    let bytes = get_display_info(&mut host, 0);
    let mut reply = Parcel::new();
    reply.unmarshall(&bytes);
    let (mut decoded, avail) = decode(&mut reply);
    let mut expected = expected_served_decoded();
    assert!(
        decoded.hdr_sdr_ratio.is_nan(),
        "hdrSdrRatio must be the framework's Float.NaN default"
    );
    // NaN != NaN, so normalise the one field that is legitimately NaN.
    decoded.hdr_sdr_ratio = 0.0;
    expected.hdr_sdr_ratio = 0.0;
    assert_eq!(decoded, expected);
    assert_eq!(avail, 0, "DisplayInfo.readFromParcel left {avail} bytes");
}

#[test]
fn unknown_display_id_is_a_null_reply() {
    acos_services::display::set_display_override(None);
    let mut host = ServiceHost::standard();
    let bytes = get_display_info(&mut host, 7);
    // writeNoException + writeTypedObject(null) and nothing else.
    assert_eq!(bytes, vec![0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn variant_1_cutout_is_written_in_write_cutout_to_parcel_order() {
    // The API-34 emulator's own cutout (fixtures/.../display.txt):
    //   insets=Rect(0, 128 - 0, 0) bounds top Rect(492, 0 - 610, 128)
    //   displayWidth=1080 displayHeight=2400 density=2.625
    //   cutoutSpec="M 507,64 a 33,33 0 1 0 66,0 33,33 0 1 0 -66,0 Z @left"
    acos_services::display::set_display_override(None);
    let mut info = DisplayInfo::hello_views();
    info.cutout = DisplayCutout::Some(CutoutGeometry {
        safe_insets: (0, 128, 0, 0),
        bounds: [(0, 0, 0, 0), (492, 0, 610, 128), (0, 0, 0, 0), (0, 0, 0, 0)],
        waterfall_insets: (0, 0, 0, 0),
        display_width: 1080,
        display_height: 2400,
        physical_display_width: 1080,
        physical_display_height: 2400,
        density: 2.625,
        cutout_spec: Some("M 507,64 a 33,33 0 1 0 66,0 33,33 0 1 0 -66,0 Z @left".into()),
        rotation: 0,
        scale: 1.0,
        physical_pixel_display_size_ratio: 1.0,
    });

    let mut parcel = Parcel::new();
    reply_prefix(&mut parcel);
    parcelable::write_payload(&mut parcel, &info.to_parcelable()).expect("write DisplayInfo");
    let bytes = parcel.marshall();

    let mut decoded = Parcel::new();
    decoded.unmarshall(&bytes);
    let (got, avail) = decode(&mut decoded);
    assert_eq!(got.cutout_variant, 1);
    let wrote = match &info.cutout {
        DisplayCutout::Some(g) => Some(g.clone()),
        _ => None,
    };
    assert_eq!(got.cutout, wrote);
    assert_eq!(got.name.as_deref(), Some("Built-in Screen"));
    assert_eq!(avail, 0, "cutout reply left {avail} bytes");

    // And the payload field order is exactly the codec's field table.
    assert_eq!(got.supported_modes.len(), 1);
}


#[test]
fn displayinfo_spec_is_the_api34_write_order() {
    // A literal transcription of `DisplayInfo.writeToParcel` (587-647). The
    // DEX scanner collapses `for` loops, so this list — not the scanner — pins
    // the field order of the codec.
    use FieldOp::{
        Bool, CountedNested, Cutout, Float, Int, IntArray, Long, Named, SparseArray, Str8, Typed,
    };
    let expected: &[FieldOp] = &[
        Int,                                         // layerStack
        Int,                                         // flags
        Int,                                         // type
        Int,                                         // displayId
        Int,                                         // displayGroupId
        Named("android.view.DisplayAddress"),        // address
        Named("android.hardware.display.DeviceProductInfo"), // deviceProductInfo
        Str8,                                        // name
        Int,                                         // appWidth
        Int,                                         // appHeight
        Int,                                         // smallestNominalAppWidth
        Int,                                         // smallestNominalAppHeight
        Int,                                         // largestNominalAppWidth
        Int,                                         // largestNominalAppHeight
        Int,                                         // logicalWidth
        Int,                                         // logicalHeight
        Cutout,                                      // writeCutoutToParcel
        Int,                                         // rotation
        Int,                                         // modeId
        Float,                                       // renderFrameRate
        Int,                                         // defaultModeId
        Int,                                         // userPreferredModeId
        CountedNested("android.view.Display$Mode"),  // supportedModes
        Int,                                         // colorMode
        IntArray,                                    // supportedColorModes
        Named("android.view.Display$HdrCapabilities"), // hdrCapabilities
        Bool,                                        // minimalPostProcessingSupported
        Int,                                         // logicalDensityDpi
        Float,                                       // physicalXDpi
        Float,                                       // physicalYDpi
        Long,                                        // appVsyncOffsetNanos
        Long,                                        // presentationDeadlineNanos
        Int,                                         // state
        Int,                                         // committedState
        Int,                                         // ownerUid
        Str8,                                        // ownerPackageName
        Str8,                                        // uniqueId
        Int,                                         // removeMode
        Float,                                       // refreshRateOverride
        Float,                                       // brightnessMinimum
        Float,                                       // brightnessMaximum
        Float,                                       // brightnessDefault
        Typed("android.view.RoundedCorners"),        // roundedCorners
        IntArray,                                    // userDisabledHdrTypes
        Int,                                         // installOrientation
        Typed("android.view.DisplayShape"),          // displayShape
        Typed("android.view.SurfaceControl$RefreshRateRange"), // layoutLimitedRefreshRate
        Float,                                       // hdrSdrRatio
        SparseArray,                                 // thermalRefreshRateThrottling
        Str8,                                        // thermalBrightnessThrottlingDataId
    ];
    let spec = parcelable::spec_for(parcelable::DISPLAY_INFO_CLASS).expect("DisplayInfo codec");
    assert_eq!(spec.fields, expected, "DisplayInfo field order drifted");
    assert_eq!(spec.verification, Verification::Prefix);
}

#[test]
fn served_value_matches_the_codec_table_field_count() {
    acos_services::display::set_display_override(None);
    let info = DisplayInfo::hello_views();
    let value = info.to_parcelable();
    let spec = parcelable::spec_for(parcelable::DISPLAY_INFO_CLASS).expect("codec");
    assert_eq!(value.fields().expect("fields").len(), spec.fields.len());
    assert_eq!(DisplayInfo::from_parcelable(&value), Some(info.clone()));
    assert!(matches!(value.field(47), Some(Value::Float(f)) if f.is_nan()));
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i % 16 == 0 {
            out.push_str(&format!("\n  {i:04x} "));
        }
        out.push_str(&format!("{b:02x} "));
    }
    out
}
