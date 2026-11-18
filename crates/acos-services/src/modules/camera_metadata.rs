//! `camera_metadata_t` — the packed key/value blob Camera2 reads its
//! `CameraCharacteristics` (and, later, capture results) from, and the parcel
//! framing `CameraMetadataNative` puts it in.
//!
//! Reuse first: the *reader* of this blob is Google's own native code. The
//! image's `libandroid_runtime.so` registers
//! `android.hardware.camera2.impl.CameraMetadataNative`'s JNI
//! (`register_android_hardware_camera2_CameraMetadata`), which calls
//! `libcamera_client.so`'s `CameraMetadata::readFromParcel`, which in turn runs
//! `libcamera_metadata.so`'s `validate_camera_metadata_structure` on a copy. So
//! this file does not invent a format; it *writes* the one that library reads,
//! and the proof it is right is the guest accepting it (`scenarios/camera-probe.yaml`).
//! The unit tests carry a Rust port of the same structural checks so a malformed
//! blob fails here, in milliseconds, instead of in the guest.
//!
//! # Layouts (AOSP `android-14.0.0_r75`, `system/media/camera/src/camera_metadata.c`
//! and `frameworks/av/camera/CameraMetadata.cpp`)
//!
//! `camera_metadata_t` (48 bytes, little endian): `size u32`, `version u32 (=1)`,
//! `flags u32 (bit 0 = entries sorted by tag)`, `entry_count u32`,
//! `entry_capacity u32`, `entries_start u32`, `data_count u32`, `data_capacity
//! u32`, `data_start u32`, `padding u32`, `vendor_id u64`
//! (`CAMERA_METADATA_INVALID_VENDOR_ID` = `u64::MAX`). Entries follow at
//! `entries_start`: 16 bytes each — `tag u32`, `count u32`, `data` (`offset u32`
//! into the data region, or up to 4 inline value bytes when
//! `count * type_size <= 4`), `type u8`, three reserved bytes. Out-of-line data
//! is 8-byte aligned. A tag's type is fixed by the tag table
//! (`camera_metadata_tag_info.c`); a mismatch fails validation.
//!
//! Parcel framing (`CameraMetadata::writeToParcel(Parcel&, const camera_metadata_t*)`):
//! `int32 blobSize` (= metadata size + 8, the packet alignment), then a blob —
//! `int32 0` (`BLOB_INPLACE`) followed by `blobSize` bytes, padded to 4 — then
//! `int32 offset`, the number of padding bytes in front of the metadata inside
//! the blob so it starts 8-aligned. `CameraMetadataNative` is written as a
//! typed object by the AIDL stub (`writeTypedObject(x, 1)`: `int32 1`, then
//! the above). An empty metadata is `int32 0`.

use acos_parcel::Parcel;

/// The `camera_metadata_t` header size.
pub const HEADER_SIZE: usize = 48;
/// One `camera_metadata_buffer_entry_t`.
pub const ENTRY_SIZE: usize = 16;
/// `get_camera_metadata_alignment()`.
pub const PACKET_ALIGNMENT: usize = 8;
/// `CAMERA_METADATA_INVALID_VENDOR_ID`.
pub const INVALID_VENDOR_ID: u64 = u64::MAX;
/// `FLAG_SORTED`.
pub const FLAG_SORTED: u32 = 1;

/// A tag's value type (`camera_metadata_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// `TYPE_BYTE`.
    Byte = 0,
    /// `TYPE_INT32`.
    Int32 = 1,
    /// `TYPE_FLOAT`.
    Float = 2,
    /// `TYPE_INT64`.
    Int64 = 3,
    /// `TYPE_DOUBLE`.
    Double = 4,
    /// `TYPE_RATIONAL` (two `i32`: numerator, denominator).
    Rational = 5,
}

impl Type {
    /// `camera_metadata_type_size`.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Int32 | Self::Float => 4,
            Self::Int64 | Self::Double | Self::Rational => 8,
        }
    }

    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Byte,
            1 => Self::Int32,
            2 => Self::Float,
            3 => Self::Int64,
            4 => Self::Double,
            5 => Self::Rational,
            _ => return None,
        })
    }
}

/// A metadata tag: `section << 16 | index`, and the type the tag table fixes for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The tag number (`camera_metadata_tag_t`).
    pub id: u32,
    /// Its type.
    pub ty: Type,
}

/// The tags this module writes. Numbers and types are read from
/// `system/media/camera/include/system/camera_metadata_tags.h` and
/// `camera_metadata_tag_info.c` at the same tag as the rest of the reference
/// cache (`tools/fetch-aosp-reference.sh`, the `media` checkout); the unit tests
/// assert the ones a Java `CameraCharacteristics.Key` name resolves to.
pub mod tags {
    use super::{Tag, Type};

    /// `ANDROID_CONTROL_AE_AVAILABLE_ANTIBANDING_MODES`.
    pub const CONTROL_AE_AVAILABLE_ANTIBANDING_MODES: Tag = Tag { id: 0x10012, ty: Type::Byte };
    /// `ANDROID_CONTROL_AE_AVAILABLE_MODES`.
    pub const CONTROL_AE_AVAILABLE_MODES: Tag = Tag { id: 0x10013, ty: Type::Byte };
    /// `ANDROID_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES`.
    pub const CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES: Tag = Tag { id: 0x10014, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_COMPENSATION_RANGE`.
    pub const CONTROL_AE_COMPENSATION_RANGE: Tag = Tag { id: 0x10015, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_COMPENSATION_STEP`.
    pub const CONTROL_AE_COMPENSATION_STEP: Tag = Tag { id: 0x10016, ty: Type::Rational };
    /// `ANDROID_CONTROL_AF_AVAILABLE_MODES`.
    pub const CONTROL_AF_AVAILABLE_MODES: Tag = Tag { id: 0x10017, ty: Type::Byte };
    /// `ANDROID_CONTROL_AVAILABLE_EFFECTS`.
    pub const CONTROL_AVAILABLE_EFFECTS: Tag = Tag { id: 0x10018, ty: Type::Byte };
    /// `ANDROID_CONTROL_AVAILABLE_SCENE_MODES`.
    pub const CONTROL_AVAILABLE_SCENE_MODES: Tag = Tag { id: 0x10019, ty: Type::Byte };
    /// `ANDROID_CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES`.
    pub const CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES: Tag = Tag { id: 0x1001a, ty: Type::Byte };
    /// `ANDROID_CONTROL_AWB_AVAILABLE_MODES`.
    pub const CONTROL_AWB_AVAILABLE_MODES: Tag = Tag { id: 0x1001b, ty: Type::Byte };
    /// `ANDROID_CONTROL_MAX_REGIONS`.
    pub const CONTROL_MAX_REGIONS: Tag = Tag { id: 0x1001c, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_LOCK_AVAILABLE`.
    pub const CONTROL_AE_LOCK_AVAILABLE: Tag = Tag { id: 0x10024, ty: Type::Byte };
    /// `ANDROID_CONTROL_AWB_LOCK_AVAILABLE`.
    pub const CONTROL_AWB_LOCK_AVAILABLE: Tag = Tag { id: 0x10025, ty: Type::Byte };
    /// `ANDROID_FLASH_INFO_AVAILABLE`.
    pub const FLASH_INFO_AVAILABLE: Tag = Tag { id: 0x50000, ty: Type::Byte };
    /// `ANDROID_JPEG_AVAILABLE_THUMBNAIL_SIZES`.
    pub const JPEG_AVAILABLE_THUMBNAIL_SIZES: Tag = Tag { id: 0x70007, ty: Type::Int32 };
    /// `ANDROID_LENS_FACING`.
    pub const LENS_FACING: Tag = Tag { id: 0x80005, ty: Type::Byte };
    /// `ANDROID_LENS_INFO_AVAILABLE_APERTURES`.
    pub const LENS_INFO_AVAILABLE_APERTURES: Tag = Tag { id: 0x90000, ty: Type::Float };
    /// `ANDROID_LENS_INFO_AVAILABLE_FILTER_DENSITIES`.
    pub const LENS_INFO_AVAILABLE_FILTER_DENSITIES: Tag = Tag { id: 0x90001, ty: Type::Float };
    /// `ANDROID_LENS_INFO_AVAILABLE_FOCAL_LENGTHS`.
    pub const LENS_INFO_AVAILABLE_FOCAL_LENGTHS: Tag = Tag { id: 0x90002, ty: Type::Float };
    /// `ANDROID_LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION`.
    pub const LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION: Tag = Tag { id: 0x90003, ty: Type::Byte };
    /// `ANDROID_LENS_INFO_FOCUS_DISTANCE_CALIBRATION`.
    pub const LENS_INFO_FOCUS_DISTANCE_CALIBRATION: Tag = Tag { id: 0x90007, ty: Type::Byte };
    /// `ANDROID_LENS_INFO_HYPERFOCAL_DISTANCE`.
    pub const LENS_INFO_HYPERFOCAL_DISTANCE: Tag = Tag { id: 0x90004, ty: Type::Float };
    /// `ANDROID_LENS_INFO_MINIMUM_FOCUS_DISTANCE`.
    pub const LENS_INFO_MINIMUM_FOCUS_DISTANCE: Tag = Tag { id: 0x90005, ty: Type::Float };
    /// `ANDROID_NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES`.
    pub const NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES: Tag = Tag { id: 0xa0002, ty: Type::Byte };
    /// `ANDROID_REQUEST_AVAILABLE_CAPABILITIES`.
    pub const REQUEST_AVAILABLE_CAPABILITIES: Tag = Tag { id: 0xc000c, ty: Type::Byte };
    /// `ANDROID_REQUEST_AVAILABLE_CHARACTERISTICS_KEYS`.
    pub const REQUEST_AVAILABLE_CHARACTERISTICS_KEYS: Tag = Tag { id: 0xc000f, ty: Type::Int32 };
    /// `ANDROID_REQUEST_AVAILABLE_REQUEST_KEYS`.
    pub const REQUEST_AVAILABLE_REQUEST_KEYS: Tag = Tag { id: 0xc000d, ty: Type::Int32 };
    /// `ANDROID_REQUEST_AVAILABLE_RESULT_KEYS`.
    pub const REQUEST_AVAILABLE_RESULT_KEYS: Tag = Tag { id: 0xc000e, ty: Type::Int32 };
    /// `ANDROID_REQUEST_MAX_NUM_OUTPUT_STREAMS`.
    pub const REQUEST_MAX_NUM_OUTPUT_STREAMS: Tag = Tag { id: 0xc0006, ty: Type::Int32 };
    /// `ANDROID_REQUEST_PARTIAL_RESULT_COUNT`.
    pub const REQUEST_PARTIAL_RESULT_COUNT: Tag = Tag { id: 0xc000b, ty: Type::Int32 };
    /// `ANDROID_REQUEST_PIPELINE_MAX_DEPTH`.
    pub const REQUEST_PIPELINE_MAX_DEPTH: Tag = Tag { id: 0xc000a, ty: Type::Byte };
    /// `ANDROID_SCALER_AVAILABLE_MAX_DIGITAL_ZOOM`.
    pub const SCALER_AVAILABLE_MAX_DIGITAL_ZOOM: Tag = Tag { id: 0xd0004, ty: Type::Float };
    /// `ANDROID_SCALER_AVAILABLE_MIN_FRAME_DURATIONS`.
    pub const SCALER_AVAILABLE_MIN_FRAME_DURATIONS: Tag = Tag { id: 0xd000b, ty: Type::Int64 };
    /// `ANDROID_SCALER_AVAILABLE_STALL_DURATIONS`.
    pub const SCALER_AVAILABLE_STALL_DURATIONS: Tag = Tag { id: 0xd000c, ty: Type::Int64 };
    /// `ANDROID_SCALER_AVAILABLE_STREAM_CONFIGURATIONS`.
    pub const SCALER_AVAILABLE_STREAM_CONFIGURATIONS: Tag = Tag { id: 0xd000a, ty: Type::Int32 };
    /// `ANDROID_SCALER_CROPPING_TYPE`.
    pub const SCALER_CROPPING_TYPE: Tag = Tag { id: 0xd000d, ty: Type::Byte };
    /// `ANDROID_SENSOR_INFO_ACTIVE_ARRAY_SIZE`.
    pub const SENSOR_INFO_ACTIVE_ARRAY_SIZE: Tag = Tag { id: 0xf0000, ty: Type::Int32 };
    /// `ANDROID_SENSOR_INFO_COLOR_FILTER_ARRANGEMENT`.
    pub const SENSOR_INFO_COLOR_FILTER_ARRANGEMENT: Tag = Tag { id: 0xf0002, ty: Type::Byte };
    /// `ANDROID_SENSOR_INFO_EXPOSURE_TIME_RANGE`.
    pub const SENSOR_INFO_EXPOSURE_TIME_RANGE: Tag = Tag { id: 0xf0003, ty: Type::Int64 };
    /// `ANDROID_SENSOR_INFO_LENS_SHADING_APPLIED`.
    pub const SENSOR_INFO_LENS_SHADING_APPLIED: Tag = Tag { id: 0xf0009, ty: Type::Byte };
    /// `ANDROID_SENSOR_INFO_MAX_FRAME_DURATION`.
    pub const SENSOR_INFO_MAX_FRAME_DURATION: Tag = Tag { id: 0xf0004, ty: Type::Int64 };
    /// `ANDROID_SENSOR_INFO_PHYSICAL_SIZE`.
    pub const SENSOR_INFO_PHYSICAL_SIZE: Tag = Tag { id: 0xf0005, ty: Type::Float };
    /// `ANDROID_SENSOR_INFO_PIXEL_ARRAY_SIZE`.
    pub const SENSOR_INFO_PIXEL_ARRAY_SIZE: Tag = Tag { id: 0xf0006, ty: Type::Int32 };
    /// `ANDROID_SENSOR_INFO_SENSITIVITY_RANGE`.
    pub const SENSOR_INFO_SENSITIVITY_RANGE: Tag = Tag { id: 0xf0001, ty: Type::Int32 };
    /// `ANDROID_SENSOR_INFO_TIMESTAMP_SOURCE`.
    pub const SENSOR_INFO_TIMESTAMP_SOURCE: Tag = Tag { id: 0xf0008, ty: Type::Byte };
    /// `ANDROID_SENSOR_INFO_WHITE_LEVEL`.
    pub const SENSOR_INFO_WHITE_LEVEL: Tag = Tag { id: 0xf0007, ty: Type::Int32 };
    /// `ANDROID_SENSOR_MAX_ANALOG_SENSITIVITY`.
    pub const SENSOR_MAX_ANALOG_SENSITIVITY: Tag = Tag { id: 0xe000d, ty: Type::Int32 };
    /// `ANDROID_SENSOR_ORIENTATION`.
    pub const SENSOR_ORIENTATION: Tag = Tag { id: 0xe000e, ty: Type::Int32 };
    /// `ANDROID_STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES`.
    pub const STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES: Tag = Tag { id: 0x120000, ty: Type::Byte };
    /// `ANDROID_STATISTICS_INFO_MAX_FACE_COUNT`.
    pub const STATISTICS_INFO_MAX_FACE_COUNT: Tag = Tag { id: 0x120002, ty: Type::Int32 };
    /// `ANDROID_INFO_SUPPORTED_HARDWARE_LEVEL`.
    pub const INFO_SUPPORTED_HARDWARE_LEVEL: Tag = Tag { id: 0x150000, ty: Type::Byte };
    /// `ANDROID_SYNC_MAX_LATENCY`.
    pub const SYNC_MAX_LATENCY: Tag = Tag { id: 0x170001, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_MODE`.
    pub const CONTROL_AE_MODE: Tag = Tag { id: 0x10003, ty: Type::Byte };
    /// `ANDROID_CONTROL_AF_MODE`.
    pub const CONTROL_AF_MODE: Tag = Tag { id: 0x10007, ty: Type::Byte };
    /// `ANDROID_CONTROL_AWB_MODE`.
    pub const CONTROL_AWB_MODE: Tag = Tag { id: 0x1000b, ty: Type::Byte };
    /// `ANDROID_CONTROL_MODE`.
    pub const CONTROL_MODE: Tag = Tag { id: 0x1000f, ty: Type::Byte };
    /// `ANDROID_FLASH_MODE`.
    pub const FLASH_MODE: Tag = Tag { id: 0x40002, ty: Type::Byte };
    /// `ANDROID_JPEG_ORIENTATION`.
    pub const JPEG_ORIENTATION: Tag = Tag { id: 0x70003, ty: Type::Int32 };
    /// `ANDROID_JPEG_QUALITY`.
    pub const JPEG_QUALITY: Tag = Tag { id: 0x70004, ty: Type::Byte };
    /// `ANDROID_SCALER_CROP_REGION`.
    pub const SCALER_CROP_REGION: Tag = Tag { id: 0xd0000, ty: Type::Int32 };
    /// `ANDROID_SENSOR_TIMESTAMP`.
    pub const SENSOR_TIMESTAMP: Tag = Tag { id: 0xe0010, ty: Type::Int64 };
    /// `ANDROID_SENSOR_EXPOSURE_TIME`.
    pub const SENSOR_EXPOSURE_TIME: Tag = Tag { id: 0xe0000, ty: Type::Int64 };
    /// `ANDROID_SENSOR_SENSITIVITY`.
    pub const SENSOR_SENSITIVITY: Tag = Tag { id: 0xe0002, ty: Type::Int32 };
    /// `ANDROID_STATISTICS_FACE_DETECT_MODE`.
    pub const STATISTICS_FACE_DETECT_MODE: Tag = Tag { id: 0x110000, ty: Type::Byte };
    /// `ANDROID_LENS_FOCAL_LENGTH`.
    pub const LENS_FOCAL_LENGTH: Tag = Tag { id: 0x80002, ty: Type::Float };
    /// `ANDROID_LENS_APERTURE`.
    pub const LENS_APERTURE: Tag = Tag { id: 0x80000, ty: Type::Float };
    /// `ANDROID_CONTROL_AE_STATE`.
    pub const CONTROL_AE_STATE: Tag = Tag { id: 0x1001f, ty: Type::Byte };
    /// `ANDROID_CONTROL_AF_STATE`.
    pub const CONTROL_AF_STATE: Tag = Tag { id: 0x10020, ty: Type::Byte };
    /// `ANDROID_CONTROL_AWB_STATE`.
    pub const CONTROL_AWB_STATE: Tag = Tag { id: 0x10022, ty: Type::Byte };
    /// `ANDROID_CONTROL_CAPTURE_INTENT`.
    pub const CONTROL_CAPTURE_INTENT: Tag = Tag { id: 0x1000d, ty: Type::Byte };
    /// `ANDROID_CONTROL_AE_TARGET_FPS_RANGE`.
    pub const CONTROL_AE_TARGET_FPS_RANGE: Tag = Tag { id: 0x10005, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_EXPOSURE_COMPENSATION`.
    pub const CONTROL_AE_EXPOSURE_COMPENSATION: Tag = Tag { id: 0x10001, ty: Type::Int32 };
    /// `ANDROID_CONTROL_AE_ANTIBANDING_MODE`.
    pub const CONTROL_AE_ANTIBANDING_MODE: Tag = Tag { id: 0x10000, ty: Type::Byte };
    /// `ANDROID_CONTROL_AE_LOCK`.
    pub const CONTROL_AE_LOCK: Tag = Tag { id: 0x10002, ty: Type::Byte };
    /// `ANDROID_CONTROL_AWB_LOCK`.
    pub const CONTROL_AWB_LOCK: Tag = Tag { id: 0x1000a, ty: Type::Byte };
    /// `ANDROID_NOISE_REDUCTION_MODE`.
    pub const NOISE_REDUCTION_MODE: Tag = Tag { id: 0xa0000, ty: Type::Byte };
    /// `ANDROID_SENSOR_FRAME_DURATION`.
    pub const SENSOR_FRAME_DURATION: Tag = Tag { id: 0xe0001, ty: Type::Int64 };
    /// `ANDROID_REQUEST_FRAME_COUNT`.
    pub const REQUEST_FRAME_COUNT: Tag = Tag { id: 0xc0000, ty: Type::Int32 };
    /// `ANDROID_LENS_STATE`.
    pub const LENS_STATE: Tag = Tag { id: 0x80009, ty: Type::Byte };

    /// Resolve a tag number to the table row, when this module knows it.
    #[must_use]
    pub fn lookup(id: u32) -> Option<Tag> {
        ALL.iter().copied().find(|t| t.id == id)
    }

    /// Every tag this module knows.
    pub const ALL: &[Tag] = &[
        CONTROL_AE_AVAILABLE_ANTIBANDING_MODES,
        CONTROL_AE_AVAILABLE_MODES,
        CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES,
        CONTROL_AE_COMPENSATION_RANGE,
        CONTROL_AE_COMPENSATION_STEP,
        CONTROL_AF_AVAILABLE_MODES,
        CONTROL_AVAILABLE_EFFECTS,
        CONTROL_AVAILABLE_SCENE_MODES,
        CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES,
        CONTROL_AWB_AVAILABLE_MODES,
        CONTROL_MAX_REGIONS,
        CONTROL_AE_LOCK_AVAILABLE,
        CONTROL_AWB_LOCK_AVAILABLE,
        FLASH_INFO_AVAILABLE,
        JPEG_AVAILABLE_THUMBNAIL_SIZES,
        LENS_FACING,
        LENS_INFO_AVAILABLE_APERTURES,
        LENS_INFO_AVAILABLE_FILTER_DENSITIES,
        LENS_INFO_AVAILABLE_FOCAL_LENGTHS,
        LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION,
        LENS_INFO_FOCUS_DISTANCE_CALIBRATION,
        LENS_INFO_HYPERFOCAL_DISTANCE,
        LENS_INFO_MINIMUM_FOCUS_DISTANCE,
        NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES,
        REQUEST_AVAILABLE_CAPABILITIES,
        REQUEST_AVAILABLE_CHARACTERISTICS_KEYS,
        REQUEST_AVAILABLE_REQUEST_KEYS,
        REQUEST_AVAILABLE_RESULT_KEYS,
        REQUEST_MAX_NUM_OUTPUT_STREAMS,
        REQUEST_PARTIAL_RESULT_COUNT,
        REQUEST_PIPELINE_MAX_DEPTH,
        SCALER_AVAILABLE_MAX_DIGITAL_ZOOM,
        SCALER_AVAILABLE_MIN_FRAME_DURATIONS,
        SCALER_AVAILABLE_STALL_DURATIONS,
        SCALER_AVAILABLE_STREAM_CONFIGURATIONS,
        SCALER_CROPPING_TYPE,
        SENSOR_INFO_ACTIVE_ARRAY_SIZE,
        SENSOR_INFO_COLOR_FILTER_ARRANGEMENT,
        SENSOR_INFO_EXPOSURE_TIME_RANGE,
        SENSOR_INFO_LENS_SHADING_APPLIED,
        SENSOR_INFO_MAX_FRAME_DURATION,
        SENSOR_INFO_PHYSICAL_SIZE,
        SENSOR_INFO_PIXEL_ARRAY_SIZE,
        SENSOR_INFO_SENSITIVITY_RANGE,
        SENSOR_INFO_TIMESTAMP_SOURCE,
        SENSOR_INFO_WHITE_LEVEL,
        SENSOR_MAX_ANALOG_SENSITIVITY,
        SENSOR_ORIENTATION,
        STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES,
        STATISTICS_INFO_MAX_FACE_COUNT,
        INFO_SUPPORTED_HARDWARE_LEVEL,
        SYNC_MAX_LATENCY,
        CONTROL_AE_MODE,
        CONTROL_AF_MODE,
        CONTROL_AWB_MODE,
        CONTROL_MODE,
        FLASH_MODE,
        JPEG_ORIENTATION,
        JPEG_QUALITY,
        SCALER_CROP_REGION,
        SENSOR_TIMESTAMP,
        SENSOR_EXPOSURE_TIME,
        SENSOR_SENSITIVITY,
        STATISTICS_FACE_DETECT_MODE,
        LENS_FOCAL_LENGTH,
        LENS_APERTURE,
        CONTROL_AE_STATE,
        CONTROL_AF_STATE,
        CONTROL_AWB_STATE,
        CONTROL_CAPTURE_INTENT,
        CONTROL_AE_TARGET_FPS_RANGE,
        CONTROL_AE_EXPOSURE_COMPENSATION,
        CONTROL_AE_ANTIBANDING_MODE,
        CONTROL_AE_LOCK,
        CONTROL_AWB_LOCK,
        NOISE_REDUCTION_MODE,
        SENSOR_FRAME_DURATION,
        REQUEST_FRAME_COUNT,
        LENS_STATE,
    ];
}

fn align_to(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

/// `calculate_camera_metadata_size(entry_count, data_count)`.
#[must_use]
pub fn packed_size(entry_count: usize, data_count: usize) -> usize {
    let entries_end = align_to(HEADER_SIZE, 4) + ENTRY_SIZE * entry_count;
    align_to(align_to(entries_end, 8) + data_count, PACKET_ALIGNMENT)
}

/// A set of tag -> values, written as one `camera_metadata_t`.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    entries: Vec<(Tag, usize, Vec<u8>)>,
}

impl Metadata {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn put(&mut self, tag: Tag, ty: Type, count: usize, bytes: Vec<u8>) -> &mut Self {
        assert_eq!(tag.ty, ty, "tag {:#x} is {:?}, not {ty:?}", tag.id, tag.ty);
        assert_eq!(bytes.len(), count * ty.size());
        if let Some(slot) = self.entries.iter_mut().find(|(t, _, _)| t.id == tag.id) {
            *slot = (tag, count, bytes);
        } else {
            self.entries.push((tag, count, bytes));
        }
        self
    }

    /// A `TYPE_BYTE` array.
    pub fn bytes(&mut self, tag: Tag, values: &[u8]) -> &mut Self {
        self.put(tag, Type::Byte, values.len(), values.to_vec())
    }

    /// A `TYPE_INT32` array.
    pub fn i32s(&mut self, tag: Tag, values: &[i32]) -> &mut Self {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put(tag, Type::Int32, values.len(), bytes)
    }

    /// A `TYPE_FLOAT` array.
    pub fn f32s(&mut self, tag: Tag, values: &[f32]) -> &mut Self {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put(tag, Type::Float, values.len(), bytes)
    }

    /// A `TYPE_INT64` array.
    pub fn i64s(&mut self, tag: Tag, values: &[i64]) -> &mut Self {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put(tag, Type::Int64, values.len(), bytes)
    }

    /// A `TYPE_RATIONAL` array of `(numerator, denominator)`.
    pub fn rationals(&mut self, tag: Tag, values: &[(i32, i32)]) -> &mut Self {
        let bytes = values
            .iter()
            .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
            .collect();
        self.put(tag, Type::Rational, values.len(), bytes)
    }

    /// The tags present, ascending.
    #[must_use]
    pub fn tag_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.entries.iter().map(|(t, _, _)| t.id).collect();
        ids.sort_unstable();
        ids
    }

    /// The entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there is nothing in the set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The packed `camera_metadata_t` (entries sorted by tag, `FLAG_SORTED` set,
    /// capacity equal to count so `get_camera_metadata_compact_size` is the size).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sorted: Vec<&(Tag, usize, Vec<u8>)> = self.entries.iter().collect();
        sorted.sort_by_key(|(t, _, _)| t.id);
        let mut data: Vec<u8> = Vec::new();
        let mut entries: Vec<u8> = Vec::with_capacity(sorted.len() * ENTRY_SIZE);
        for (tag, count, bytes) in &sorted {
            entries.extend_from_slice(&tag.id.to_le_bytes());
            entries.extend_from_slice(&(*count as u32).to_le_bytes());
            if bytes.len() <= 4 {
                let mut inline = [0_u8; 4];
                inline[..bytes.len()].copy_from_slice(bytes);
                entries.extend_from_slice(&inline);
            } else {
                while data.len() % 8 != 0 {
                    data.push(0);
                }
                entries.extend_from_slice(&(data.len() as u32).to_le_bytes());
                data.extend_from_slice(bytes);
            }
            entries.extend_from_slice(&[tag.ty as u8, 0, 0, 0]);
        }
        while data.len() % 8 != 0 {
            data.push(0);
        }
        let entry_count = sorted.len();
        let data_start = align_to(HEADER_SIZE + ENTRY_SIZE * entry_count, 8);
        let size = packed_size(entry_count, data.len());
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&(size as u32).to_le_bytes()); // size
        out.extend_from_slice(&1_u32.to_le_bytes()); // version
        out.extend_from_slice(&FLAG_SORTED.to_le_bytes()); // flags
        out.extend_from_slice(&(entry_count as u32).to_le_bytes()); // entry_count
        out.extend_from_slice(&(entry_count as u32).to_le_bytes()); // entry_capacity
        out.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes()); // entries_start
        out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // data_count
        out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // data_capacity
        out.extend_from_slice(&(data_start as u32).to_le_bytes()); // data_start
        out.extend_from_slice(&0_u32.to_le_bytes()); // padding
        out.extend_from_slice(&INVALID_VENDOR_ID.to_le_bytes()); // vendor_id
        out.extend_from_slice(&entries);
        out.resize(data_start, 0);
        out.extend_from_slice(&data);
        out.resize(size, 0);
        out
    }
}

/// A parsed `camera_metadata_t`, for the unit tests and for `control` ops that
/// report what the module serves.
#[derive(Clone, Debug)]
pub struct Parsed {
    bytes: Vec<u8>,
    entry_count: usize,
    data_start: usize,
}

/// One entry of a [`Parsed`] blob.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The tag number.
    pub tag: u32,
    /// The value type.
    pub ty: Type,
    /// Number of values.
    pub count: usize,
    /// The raw value bytes (little endian).
    pub value: Vec<u8>,
}

impl Entry {
    /// The values as `i32` (`Int32` and `Byte` entries).
    #[must_use]
    pub fn as_i32(&self) -> Vec<i32> {
        match self.ty {
            Type::Byte => self.value.iter().map(|b| i32::from(*b)).collect(),
            _ => self
                .value
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        }
    }

    /// The values as `i64`.
    #[must_use]
    pub fn as_i64(&self) -> Vec<i64> {
        self.value
            .chunks_exact(8)
            .map(|c| i64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect()
    }

    /// The values as `f32`.
    #[must_use]
    pub fn as_f32(&self) -> Vec<f32> {
        self.value
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A Rust port of `validate_camera_metadata_structure` (without the vendor-tag
/// lookup): every check the guest's `libcamera_metadata.so` makes on a blob
/// before `CameraMetadata::readFromParcel` accepts it. `known_type` resolves a
/// tag's type from the tables this module carries; an unknown non-vendor tag is
/// accepted (the guest's table is the full AOSP one) but its type must be valid.
///
/// # Errors
///
/// A description of the first violated invariant.
pub fn validate(bytes: &[u8]) -> Result<Parsed, String> {
    if bytes.len() < HEADER_SIZE {
        return Err(format!("{} bytes is smaller than the 48-byte header", bytes.len()));
    }
    let word = |at: usize| u32_at(bytes, at).ok_or_else(|| format!("short read at {at}"));
    let size = word(0)? as usize;
    let version = word(4)?;
    let entry_count = word(12)? as usize;
    let entry_capacity = word(16)? as usize;
    let entries_start = word(20)? as usize;
    let data_count = word(24)? as usize;
    let data_capacity = word(28)? as usize;
    let data_start = word(32)? as usize;
    if version != 1 {
        return Err(format!("version {version}, expected 1"));
    }
    if size > bytes.len() {
        return Err(format!("size {size} > buffer {}", bytes.len()));
    }
    if entry_count > entry_capacity {
        return Err("entry_count > entry_capacity".to_string());
    }
    if data_count > data_capacity {
        return Err("data_count > data_capacity".to_string());
    }
    if entries_start % 4 != 0 || data_start % 8 != 0 {
        return Err("entries/data region misaligned".to_string());
    }
    if entries_start + entry_capacity * ENTRY_SIZE > data_start {
        return Err("entries overlap the data region".to_string());
    }
    if data_start + data_capacity > size {
        return Err("data region exceeds size".to_string());
    }
    for i in 0..entry_count {
        let at = entries_start + i * ENTRY_SIZE;
        let tag = word(at)?;
        let count = word(at + 4)? as usize;
        let ty = bytes
            .get(at + 12)
            .and_then(|b| Type::from_u8(*b))
            .ok_or_else(|| format!("entry {i}: bad type"))?;
        if let Some(known) = tags::lookup(tag) {
            if known.ty != ty {
                return Err(format!("entry {i} ({tag:#x}): tag type {:?}, entry type {ty:?}", known.ty));
            }
        }
        let value_bytes = count
            .checked_mul(ty.size())
            .ok_or_else(|| format!("entry {i}: count overflow"))?;
        if value_bytes > 4 {
            let offset = word(at + 8)? as usize;
            if offset % 8 != 0 {
                return Err(format!("entry {i} ({tag:#x}): data offset {offset} not 8-aligned"));
            }
            if offset + align_to(value_bytes, 8) > data_capacity {
                return Err(format!("entry {i} ({tag:#x}): data runs past the capacity"));
            }
        } else if count == 0 && word(at + 8)? != 0 {
            return Err(format!("entry {i}: empty entry with a non-zero offset"));
        }
    }
    Ok(Parsed {
        bytes: bytes[..size].to_vec(),
        entry_count,
        data_start,
    })
}

impl Parsed {
    /// The entries, in blob order.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        let word = |at: usize| u32_at(&self.bytes, at).expect("validated");
        let entries_start = word(20) as usize;
        (0..self.entry_count)
            .map(|i| {
                let at = entries_start + i * ENTRY_SIZE;
                let ty = Type::from_u8(self.bytes[at + 12]).expect("validated");
                let count = word(at + 4) as usize;
                let n = count * ty.size();
                let value = if n <= 4 {
                    self.bytes[at + 8..at + 8 + n].to_vec()
                } else {
                    let off = self.data_start + word(at + 8) as usize;
                    self.bytes[off..off + n].to_vec()
                };
                Entry {
                    tag: word(at),
                    ty,
                    count,
                    value,
                }
            })
            .collect()
    }

    /// The entry for `tag`.
    #[must_use]
    pub fn get(&self, tag: Tag) -> Option<Entry> {
        self.entries().into_iter().find(|e| e.tag == tag.id)
    }
}

/// Write `metadata` the way `CameraMetadata::writeToParcel` does (the body of a
/// `CameraMetadataNative`; the caller writes the AIDL typed-object marker).
///
/// `blobSize = metadata + 8`, an inline blob (`int32 0` + bytes + pad) and the
/// 8-alignment `offset`. The blob stays inline: `Parcel::writeBlob` only goes
/// through ashmem above `BLOB_INPLACE_LIMIT` (16 KiB) and this module keeps the
/// characteristics under it (asserted by a unit test).
pub fn write_parcel(parcel: &mut Parcel, metadata: &[u8]) {
    if metadata.is_empty() {
        parcel.write_int32(0);
        return;
    }
    let blob_size = metadata.len() + PACKET_ALIGNMENT;
    parcel.write_int32(blob_size as i32);
    // The blob data starts after its own `int32 0` marker; the metadata must
    // start 8-aligned *in the reader's buffer*, whose base is malloc-aligned.
    let blob_start = parcel.data_position() + 4;
    let offset = (PACKET_ALIGNMENT - blob_start % PACKET_ALIGNMENT) % PACKET_ALIGNMENT;
    let mut blob = vec![0_u8; blob_size];
    blob[offset..offset + metadata.len()].copy_from_slice(metadata);
    parcel
        .write_blob(&blob)
        .expect("a blob below i32::MAX bytes is writable");
    parcel.write_int32(offset as i32);
}

/// Read what [`write_parcel`] wrote (the unit tests' stand-in for
/// `CameraMetadata::readFromParcel`): `None` for an empty metadata.
///
/// # Errors
///
/// A description of the malformed framing.
pub fn read_parcel(parcel: &mut Parcel) -> Result<Option<Vec<u8>>, String> {
    let blob_size = parcel.read_int32().map_err(|e| e.to_string())?;
    if blob_size == 0 {
        return Ok(None);
    }
    let blob_size = usize::try_from(blob_size).map_err(|_| "negative blob size".to_string())?;
    if blob_size <= PACKET_ALIGNMENT {
        return Err(format!("blob size {blob_size} <= alignment"));
    }
    let blob = parcel
        .read_blob(blob_size)
        .map_err(|e| e.to_string())?
        .ok_or("null blob")?;
    let offset = parcel.read_int32().map_err(|e| e.to_string())?;
    let offset = usize::try_from(offset).map_err(|_| "negative offset".to_string())?;
    if offset >= PACKET_ALIGNMENT {
        return Err(format!("offset {offset} >= alignment"));
    }
    let size = blob_size - PACKET_ALIGNMENT;
    Ok(Some(blob[offset..offset + size].to_vec()))
}

/// What the stub camera is, in the units `CameraCharacteristics` reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Sensor {
    /// `LENS_FACING_BACK` when true, `LENS_FACING_FRONT` otherwise.
    pub back: bool,
    /// `SENSOR_ORIENTATION`, degrees clockwise.
    pub orientation: i32,
    /// Pixel array width.
    pub width: i32,
    /// Pixel array height.
    pub height: i32,
}

impl Default for Sensor {
    fn default() -> Self {
        Self {
            back: true,
            orientation: 90,
            width: 4000,
            height: 3000,
        }
    }
}

/// `HAL_PIXEL_FORMAT_*` values the stream tables use (JPEG is `BLOB` in metadata).
pub mod format {
    /// `HAL_PIXEL_FORMAT_BLOB` (what `ImageFormat.JPEG` maps to).
    pub const BLOB: i32 = 0x21;
    /// `HAL_PIXEL_FORMAT_IMPLEMENTATION_DEFINED` (`ImageFormat.PRIVATE`).
    pub const IMPLEMENTATION_DEFINED: i32 = 0x22;
    /// `HAL_PIXEL_FORMAT_YCbCr_420_888`.
    pub const YCBCR_420_888: i32 = 0x23;
}

/// `INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED`. A stub that never produces a real
/// frame claims the middle level: BACKWARD_COMPATIBLE capability, fixed focus,
/// no flash, no manual sensor control, no RAW.
pub const HARDWARE_LEVEL_LIMITED: u8 = 1;

/// The output sizes a stub of `sensor`'s resolution offers: the full array and
/// the usual 4:3 / 16:9 ladder at or below it, largest first.
#[must_use]
pub fn output_sizes(sensor: &Sensor) -> Vec<(i32, i32)> {
    let mut sizes = vec![(sensor.width, sensor.height)];
    for (w, h) in [
        (1920, 1080),
        (1600, 1200),
        (1280, 960),
        (1280, 720),
        (800, 600),
        (640, 480),
        (320, 240),
        (176, 144),
    ] {
        if w <= sensor.width && h <= sensor.height && !sizes.contains(&(w, h)) {
            sizes.push((w, h));
        }
    }
    sizes
}

/// The `CameraCharacteristics` of the stub camera.
///
/// Every value is one a real phone's back camera reports in the same units; the
/// capability claims are the smallest set the stub can honour: fixed focus (AF
/// mode `OFF` only, minimum focus distance 0), no flash, 1.0x digital zoom,
/// auto AE/AWB, `YUV_420_888` / `PRIVATE` / `JPEG` outputs at 30 fps, hardware
/// level `LIMITED` with only the `BACKWARD_COMPATIBLE` capability.
#[must_use]
pub fn characteristics(sensor: &Sensor) -> Metadata {
    use tags as t;
    let sizes = output_sizes(sensor);
    let mut m = Metadata::new();

    // --- lens -------------------------------------------------------------
    m.bytes(t::LENS_FACING, &[if sensor.back { 1 } else { 0 }]);
    m.f32s(t::LENS_INFO_AVAILABLE_APERTURES, &[1.8]);
    m.f32s(t::LENS_INFO_AVAILABLE_FILTER_DENSITIES, &[0.0]);
    m.f32s(t::LENS_INFO_AVAILABLE_FOCAL_LENGTHS, &[4.38]);
    m.bytes(t::LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION, &[0]);
    m.bytes(t::LENS_INFO_FOCUS_DISTANCE_CALIBRATION, &[0]);
    m.f32s(t::LENS_INFO_HYPERFOCAL_DISTANCE, &[0.0]);
    m.f32s(t::LENS_INFO_MINIMUM_FOCUS_DISTANCE, &[0.0]);

    // --- flash ------------------------------------------------------------
    m.bytes(t::FLASH_INFO_AVAILABLE, &[0]);

    // --- control ----------------------------------------------------------
    m.bytes(t::CONTROL_AE_AVAILABLE_ANTIBANDING_MODES, &[0, 1, 2, 3]);
    m.bytes(t::CONTROL_AE_AVAILABLE_MODES, &[1]);
    m.i32s(t::CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, &[15, 30, 30, 30]);
    m.i32s(t::CONTROL_AE_COMPENSATION_RANGE, &[-12, 12]);
    m.rationals(t::CONTROL_AE_COMPENSATION_STEP, &[(1, 6)]);
    m.bytes(t::CONTROL_AE_LOCK_AVAILABLE, &[1]);
    m.bytes(t::CONTROL_AF_AVAILABLE_MODES, &[0]);
    m.bytes(t::CONTROL_AVAILABLE_EFFECTS, &[0]);
    m.bytes(t::CONTROL_AVAILABLE_SCENE_MODES, &[0]);
    m.bytes(t::CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES, &[0]);
    m.bytes(t::CONTROL_AWB_AVAILABLE_MODES, &[1]);
    m.bytes(t::CONTROL_AWB_LOCK_AVAILABLE, &[1]);
    m.i32s(t::CONTROL_MAX_REGIONS, &[0, 0, 0]);

    // --- jpeg / noise reduction / statistics ------------------------------
    m.i32s(t::JPEG_AVAILABLE_THUMBNAIL_SIZES, &[0, 0, 160, 120, 320, 240]);
    m.bytes(t::NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES, &[0, 1]);
    m.bytes(t::STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES, &[0]);
    m.i32s(t::STATISTICS_INFO_MAX_FACE_COUNT, &[0]);

    // --- scaler: the stream tables `StreamConfigurationMap` is built from ---
    m.f32s(t::SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, &[1.0]);
    m.bytes(t::SCALER_CROPPING_TYPE, &[0]);
    let mut stream_configs: Vec<i32> = Vec::new();
    let mut frame_durations: Vec<i64> = Vec::new();
    let mut stall_durations: Vec<i64> = Vec::new();
    for format in [format::BLOB, format::YCBCR_420_888, format::IMPLEMENTATION_DEFINED] {
        for &(w, h) in &sizes {
            // OUTPUT direction = 0.
            stream_configs.extend([format, w, h, 0]);
            frame_durations.extend([i64::from(format), i64::from(w), i64::from(h), 33_333_333]);
            if format == format::BLOB {
                // A JPEG encode stalls the pipeline; 12 MP ~ 100 ms here.
                let pixels = i64::from(w) * i64::from(h);
                let stall = 10_000_000 + pixels * 8;
                stall_durations.extend([i64::from(format), i64::from(w), i64::from(h), stall]);
            }
        }
    }
    m.i32s(t::SCALER_AVAILABLE_STREAM_CONFIGURATIONS, &stream_configs);
    m.i64s(t::SCALER_AVAILABLE_MIN_FRAME_DURATIONS, &frame_durations);
    m.i64s(t::SCALER_AVAILABLE_STALL_DURATIONS, &stall_durations);

    // --- sensor -----------------------------------------------------------
    m.i32s(t::SENSOR_ORIENTATION, &[sensor.orientation]);
    m.i32s(t::SENSOR_INFO_ACTIVE_ARRAY_SIZE, &[0, 0, sensor.width, sensor.height]);
    m.i32s(t::SENSOR_INFO_PIXEL_ARRAY_SIZE, &[sensor.width, sensor.height]);
    m.f32s(t::SENSOR_INFO_PHYSICAL_SIZE, &[5.76, 4.29]);
    m.i32s(t::SENSOR_INFO_SENSITIVITY_RANGE, &[100, 1600]);
    m.i64s(t::SENSOR_INFO_EXPOSURE_TIME_RANGE, &[100_000, 100_000_000]);
    m.i64s(t::SENSOR_INFO_MAX_FRAME_DURATION, &[300_000_000]);
    m.bytes(t::SENSOR_INFO_COLOR_FILTER_ARRANGEMENT, &[0]);
    m.bytes(t::SENSOR_INFO_TIMESTAMP_SOURCE, &[1]); // REALTIME
    m.bytes(t::SENSOR_INFO_LENS_SHADING_APPLIED, &[0]);
    m.i32s(t::SENSOR_INFO_WHITE_LEVEL, &[1023]);
    m.i32s(t::SENSOR_MAX_ANALOG_SENSITIVITY, &[800]);

    // --- request / info / sync --------------------------------------------
    m.bytes(t::REQUEST_AVAILABLE_CAPABILITIES, &[0]); // BACKWARD_COMPATIBLE
    m.i32s(t::REQUEST_MAX_NUM_OUTPUT_STREAMS, &[0, 3, 1]);
    m.i32s(t::REQUEST_PARTIAL_RESULT_COUNT, &[1]);
    m.bytes(t::REQUEST_PIPELINE_MAX_DEPTH, &[4]);
    m.bytes(t::INFO_SUPPORTED_HARDWARE_LEVEL, &[HARDWARE_LEVEL_LIMITED]);
    m.i32s(t::SYNC_MAX_LATENCY, &[0]); // PER_FRAME_CONTROL

    let request_keys = [
        t::CONTROL_AE_ANTIBANDING_MODE,
        t::CONTROL_AE_EXPOSURE_COMPENSATION,
        t::CONTROL_AE_LOCK,
        t::CONTROL_AE_MODE,
        t::CONTROL_AE_TARGET_FPS_RANGE,
        t::CONTROL_AF_MODE,
        t::CONTROL_AWB_LOCK,
        t::CONTROL_AWB_MODE,
        t::CONTROL_CAPTURE_INTENT,
        t::CONTROL_MODE,
        t::JPEG_ORIENTATION,
        t::JPEG_QUALITY,
        t::NOISE_REDUCTION_MODE,
        t::SCALER_CROP_REGION,
        t::STATISTICS_FACE_DETECT_MODE,
    ];
    let mut result_keys: Vec<Tag> = request_keys.to_vec();
    result_keys.extend([
        t::CONTROL_AE_STATE,
        t::CONTROL_AF_STATE,
        t::CONTROL_AWB_STATE,
        t::SENSOR_TIMESTAMP,
    ]);
    let ids = |tags: &[Tag]| tags.iter().map(|t| t.id as i32).collect::<Vec<_>>();
    m.i32s(t::REQUEST_AVAILABLE_REQUEST_KEYS, &ids(&request_keys));
    m.i32s(t::REQUEST_AVAILABLE_RESULT_KEYS, &ids(&result_keys));
    // `CameraCharacteristics.getKeys()` asserts this list is present and it names
    // every characteristics tag in the blob (the list included).
    let mut characteristics: Vec<i32> = m.tag_ids().into_iter().map(|t| t as i32).collect();
    characteristics.push(t::REQUEST_AVAILABLE_CHARACTERISTICS_KEYS.id as i32);
    characteristics.sort_unstable();
    m.i32s(t::REQUEST_AVAILABLE_CHARACTERISTICS_KEYS, &characteristics);
    m
}
