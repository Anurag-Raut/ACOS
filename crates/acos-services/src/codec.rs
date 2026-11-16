//! `WireOp`-driven `Parcel` codec.
//!
//! This is the part that makes a wrong field order fail loudly: the dispatcher
//! walks the `WireOp` list decoded by `acos-aidl` from the real
//! `Foo$Stub$Proxy` bytecode, and the handler's `Vec<Value>` is zipped against
//! the same list. A handler cannot accidentally write fields in a different
//! order, and an incoming parcel that does not match the decoded layout is
//! rejected with [`ServiceError::BadRequest`].

use acos_aidl::WireOp;
use acos_parcel::Parcel;

use crate::error::{Result, ServiceError};
use crate::parcelable;
use crate::value::{Bundle, DynamicValue, Parcelable, RawFdValue, Value};

/// Read a request argument list in decoded order.
/// Element class of a decoded typed array/list parameter.
///
/// `[Landroid.net.Uri;` -> `android.net.Uri`; an already-dotted element class is
/// returned unchanged.
fn element_class(class: &str) -> String {
    let trimmed = class.trim_start_matches('[');
    trimmed
        .strip_prefix('L')
        .map(|rest| rest.strip_suffix(';').unwrap_or(rest))
        .unwrap_or(trimmed)
        .replace('/', ".")
}

pub fn read_args(
    parcel: &mut Parcel,
    ops: &[WireOp],
    interface: &str,
    method: &str,
) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(ops.len());
    for (index, op) in ops.iter().enumerate() {
        match read_op(parcel, op) {
            Ok(v) => out.push(v),
            Err(e) => {
                return Err(ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: method.to_string(),
                    detail: format!("arg {index} ({}): {e}", op.render()),
                })
            }
        }
    }
    Ok(out)
}

/// Transaction code of `IPackageManager.notifyDexLoad` on the API-34 image
/// (`IPackageManager$Stub$Proxy.notifyDexLoad`, `const/16 v4, #int 106`).
pub const IPACKAGE_MANAGER_NOTIFY_DEX_LOAD: u32 = 106;

/// Image-verified argument layouts for the few methods whose generated
/// `acos-aidl` table entry does not match the image's writer.
///
/// The generated tables are produced by scanning the image's `<Interface>$Stub`
/// bytecode. Where the scan could not follow a `Map`/`forEach` body it recorded
/// a shape that is *self-consistent* but wrong, and the transaction then fails
/// at decode time with a `Str16` underflow (W25 §4 blocker 5). The image is the
/// ground truth (`docs/AGENT_RESOURCES.md` §2), so the exact layouts live here,
/// next to the codec, and the dispatcher prefers them over the table.
///
/// `None` means "the generated table is correct, use it".
pub fn read_args_override(
    interface: &str,
    code: u32,
    parcel: &mut Parcel,
) -> Result<Option<Vec<Value>>> {
    match (interface, code) {
        ("android.content.pm.IPackageManager", IPACKAGE_MANAGER_NOTIFY_DEX_LOAD) => {
            Ok(Some(read_notify_dex_load(parcel)?))
        }
        // ADR-0029: `sendAccessibilityEvent(AccessibilityEvent, int)` is
        // oneway. The event parcel has no codec (it nests a record list), and
        // the module only needs the type: `isSealed` then `eventType` are the
        // first two ints of `AccessibilityEvent.writeToParcel`. Consume the
        // rest opaquely so the event is recorded instead of rejected.
        ("android.view.accessibility.IAccessibilityManager", 2) => {
            let _sealed = parcel.read_int32()?;
            let event_type = parcel.read_int32()?;
            skip_remaining(parcel);
            Ok(Some(vec![Value::Int(event_type), Value::Long(0)]))
        }
        // K2: `ParceledListSlice<NotificationChannel>` is the last argument of
        // the notification create calls. Decode it with the real
        // `NotificationChannel` codec when that codec can follow the image
        // bytes (so the service can store the channels); if a field this image
        // writes is not modelled, consume the slice opaquely and still let the
        // service answer, exactly as a real NMS accepts the call.
        ("android.app.INotificationManager", 30 | 31 | 32) => {
            let decoded = acos_aidl::lookup(interface, code).and_then(|method| {
                let mut probe = parcel.clone();
                read_args(&mut probe, &method.args, interface, &method.name)
                    .ok()
                    .map(|values| (values, probe.data_position()))
            });
            if let Some((values, position)) = decoded {
                parcel.set_data_position(position);
                return Ok(Some(values));
            }
            let package = parcel.read_string16()?;
            if code == 32 {
                let uid = parcel.read_int32()?;
                skip_remaining(parcel);
                Ok(Some(vec![
                    Value::Str(package),
                    Value::Int(uid),
                    Value::Skipped,
                ]))
            } else {
                skip_remaining(parcel);
                Ok(Some(vec![Value::Str(package), Value::Skipped]))
            }
        }
        // K2: `setDynamicShortcuts`/`addDynamicShortcuts`/`updateShortcuts`
        // carry a `ParceledListSlice<ShortcutInfo>` followed by the user id.
        // The shortcut payload has no flat codec yet; consume it opaquely and
        // keep the trailing user id so the service's answer stays complete.
        ("android.content.pm.IShortcutService", 1 | 2 | 5) => {
            let package = parcel.read_string16()?;
            let remaining = parcel.remaining();
            if remaining < 4 {
                return Err(ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: format!("code {code}"),
                    detail: "shortcut list leaves no room for the user id".into(),
                });
            }
            let keep = parcel.data_position() + remaining - 4;
            parcel.set_data_position(keep);
            let user_id = parcel.read_int32()?;
            Ok(Some(vec![
                Value::Str(package),
                Value::Skipped,
                Value::Int(user_id),
            ]))
        }
        // K2: `schedule(pkg, JobInfo)`/`enqueue(pkg, JobInfo, JobWorkItem)`
        // end with the job payload. ACOS records the schedule and answers
        // `RESULT_SUCCESS`; decoding `JobInfo`'s 30+ fields is not needed for
        // that answer, so the payload is consumed opaquely.
        ("android.app.job.IJobScheduler", 1 | 2) => {
            let package = parcel.read_string16()?;
            skip_remaining(parcel);
            Ok(Some(vec![Value::Str(package), Value::Skipped]))
        }
        // K3c: `publishContentProviders(IApplicationThread, List<ContentProviderHolder>)`
        // is how `ActivityThread.installContentProviders` reports the providers
        // it just created. The decoded table lost the generic (the image's
        // proxy writes an `ArrayList` through `writeTypedList`, so the DEX
        // type is the erasure `java.util.List`); the AIDL declaration is
        // `List<ContentProviderHolder>`, so the element class is transcribed
        // here rather than inferred from a register type that cannot carry it.
        ("android.app.IActivityManager", 31) => {
            let thread = parcel.read_strong_binder()?;
            let count = parcel.read_int32()?;
            let mut holders = Vec::new();
            for _ in 0..count.max(0) {
                let present = parcel.read_int32()?;
                if present == 0 {
                    holders.push(Value::Parcelable(Parcelable::Null));
                } else {
                    holders.push(Value::Parcelable(parcelable::read_payload(
                        "android.app.ContentProviderHolder",
                        parcel,
                    )?));
                }
            }
            Ok(Some(vec![
                Value::Binder(thread),
                Value::List(Some(holders)),
            ]))
        }
        _ => Ok(None),
    }
}

/// Consume the rest of the request without interpreting it.
fn skip_remaining(parcel: &mut Parcel) {
    parcel.set_data_position(parcel.data_size());
}

/// `notifyDexLoad(String loadingPackageName, in Map<String,String>
/// classLoaderContextMap, String loaderIsa)`.
///
/// Read from the image (`classes.dex`,
/// `Landroid/content/pm/IPackageManager$Stub$Proxy;->notifyDexLoad`):
///
/// ```text
/// writeString(loadingPackageName)
/// writeInt(map == null ? -1 : map.size())
/// per entry (for each, in iteration order): writeString(key); writeString(value)
/// writeString(loaderIsa)
/// transact(106, data, null, FLAG_ONEWAY)      // fire-and-forget
/// ```
///
/// The generated table recorded `[Str16, I32, I32, Str16]` — the map count then
/// one word read as an int and one string read out of the first key's bytes,
/// which is the `need 13107296 byte(s) at 124, have 128` underflow W25 captured
/// on a 31-dex APK. A `-1` count is a null map and is kept as `Value::Map(None)`.
fn read_notify_dex_load(parcel: &mut Parcel) -> Result<Vec<Value>> {
    let bad = |detail: String| ServiceError::BadRequest {
        interface: "android.content.pm.IPackageManager".to_string(),
        method: "notifyDexLoad".to_string(),
        detail,
    };
    let package = parcel
        .read_string16()
        .map_err(|e| bad(format!("arg 0 (loadingPackageName): {e}")))?;
    let count = parcel
        .read_int32()
        .map_err(|e| bad(format!("arg 1 (classLoaderContextMap count): {e}")))?;
    if count < -1 {
        return Err(bad(format!(
            "arg 1 (classLoaderContextMap count): {count} is neither -1 nor a size"
        )));
    }
    let mut entries = Vec::new();
    if count >= 0 {
        let count = count as usize;
        // Each entry writes two `String16`s, so it needs at least 8 bytes on the
        // wire (2 length words for two empty strings). A garbage count is
        // rejected before it can drive a large allocation or a long loop.
        let needed = count.saturating_mul(8);
        if needed > parcel.data_avail() {
            return Err(bad(format!(
                "arg 1 (classLoaderContextMap): {count} entries need at least {needed} byte(s), have {}",
                parcel.data_avail()
            )));
        }
        for index in 0..count {
            let key = parcel
                .read_string16()
                .map_err(|e| bad(format!("arg 1 entry {index} key: {e}")))?;
            let value = parcel
                .read_string16()
                .map_err(|e| bad(format!("arg 1 entry {index} value: {e}")))?;
            entries.push((
                crate::value::DynamicValue::String(key),
                crate::value::DynamicValue::String(value),
            ));
        }
    }
    let loader = parcel
        .read_string16()
        .map_err(|e| bad(format!("arg 2 (loaderIsa): {e}")))?;
    Ok(vec![
        Value::Str(package),
        Value::Map(if count < 0 { None } else { Some(entries) }),
        Value::Str(loader),
    ])
}

/// Write a reply value list in decoded order.
pub fn write_reply(
    parcel: &mut Parcel,
    ops: &[WireOp],
    values: &[Value],
    interface: &str,
    method: &str,
) -> Result<()> {
    if ops.len() != values.len() {
        return Err(ServiceError::BadReply {
            interface: interface.to_string(),
            method: method.to_string(),
            detail: format!(
                "layout has {} ops, handler returned {} values",
                ops.len(),
                values.len()
            ),
        });
    }
    for (index, (op, value)) in ops.iter().zip(values).enumerate() {
        write_op(parcel, op, value).map_err(|e| match e {
            ServiceError::BadReply { detail, .. } => ServiceError::BadReply {
                interface: interface.to_string(),
                method: method.to_string(),
                detail: format!("reply {index} ({}): {detail}", op.render()),
            },
            other => ServiceError::BadReply {
                interface: interface.to_string(),
                method: method.to_string(),
                detail: format!("reply {index} ({}): {other}", op.render()),
            },
        })?;
    }
    Ok(())
}

/// Read one value described by `op`.
pub fn read_op(parcel: &mut Parcel, op: &WireOp) -> Result<Value> {
    Ok(match op {
        WireOp::I32 => Value::Int(parcel.read_int32()?),
        WireOp::I64 => Value::Long(parcel.read_int64()?),
        WireOp::F32 => Value::Float(parcel.read_float()?),
        WireOp::F64 => Value::Double(parcel.read_double()?),
        WireOp::Bool => Value::Bool(parcel.read_bool()?),
        WireOp::Byte => Value::Byte(parcel.read_byte()?),
        WireOp::Char => Value::Char(parcel.read_char()?),
        WireOp::Str16 => Value::Str(parcel.read_string16()?),
        WireOp::Str8 => Value::Str8(parcel.read_string8()?),
        WireOp::Binder { .. } => Value::Binder(parcel.read_strong_binder()?),
        WireOp::IntArray => Value::IntArray(parcel.read_int32_array()?),
        WireOp::LongArray => Value::LongArray(parcel.read_int64_array()?),
        WireOp::FloatArray => Value::FloatArray(parcel.read_float_array()?),
        WireOp::DoubleArray => Value::DoubleArray(parcel.read_double_array()?),
        WireOp::ByteArray => Value::ByteArray(parcel.read_byte_array()?),
        WireOp::BooleanArray => Value::Skipped,
        WireOp::CharArray => Value::Skipped,
        WireOp::StringArray => Value::StringArray(parcel.read_string16_array()?),
        WireOp::StringList => Value::StringList(parcel.read_string16_array()?),
        WireOp::BinderList => {
            let n = parcel.read_int32()?;
            let mut out = Vec::new();
            for _ in 0..n.max(0) {
                out.push(parcel.read_strong_binder()?);
            }
            Value::List(Some(out.into_iter().map(Value::Binder).collect()))
        }
        WireOp::Parcelable { class } => {
            // AIDL parcelable arguments are written with the object's own
            // `writeToParcel` (no class-name wire prefix, unlike
            // `Parcel.writeParcelable`).
            Value::Parcelable(parcelable::read_payload(class, parcel)?)
        }
        WireOp::TypedObject { class, .. } => {
            let present = parcel.read_int32()?;
            if present == 0 {
                Value::TypedObject(Parcelable::Null)
            } else if class == "android.os.IBinder" {
                Value::TypedObject(Parcelable::ordered(
                    class.clone(),
                    vec![Value::Int(
                        parcel
                            .read_strong_binder()?
                            .map(|b| b.0 as i32)
                            .unwrap_or(-1),
                    )],
                ))
            } else {
                Value::TypedObject(parcelable::read_payload(class, parcel)?)
            }
        }
        WireOp::Bundle => Value::Bundle(parcelable::read_bundle(parcel)?),
        WireOp::RawFd => Value::RawFd(read_fd(parcel)?.map(RawFdValue)),
        WireOp::Value => Value::Dynamic(parcelable::read_dynamic(parcel)?),
        WireOp::Cond { marker, ops } => {
            let m = read_op(parcel, marker)?;
            let present = truthy(&m);
            let mut values = Vec::with_capacity(ops.len());
            if present {
                for o in ops {
                    values.push(read_op(parcel, o)?);
                }
            }
            Value::Cond { present, values }
        }
        WireOp::List { .. } | WireOp::Array { .. } => {
            let n = parcel.read_int32()?;
            let mut out = Vec::new();
            for _ in 0..n.max(0) {
                out.push(Value::Dynamic(parcelable::read_dynamic(parcel)?));
            }
            Value::List(Some(out))
        }
        WireOp::Map { .. } => {
            let n = parcel.read_int32()?;
            let mut out = Vec::new();
            for _ in 0..n.max(0) {
                let k = parcelable::read_dynamic(parcel)?;
                let v = parcelable::read_dynamic(parcel)?;
                out.push((k, v));
            }
            Value::Map(Some(out))
        }
        WireOp::TypedArray { class } | WireOp::TypedList { class } => {
            // The decoded table records the parameter type, so a typed array of
            // `Uri` arrives as `[Landroid.net.Uri` rather than the element
            // class. Normalise to the element descriptor the parcelable codec
            // knows (`android.net.Uri`).
            let element = element_class(class);
            let n = parcel.read_int32()?;
            let mut out = Vec::new();
            for _ in 0..n.max(0) {
                // AIDL writes each element with `writeTypedObject`, i.e. an
                // explicit presence marker before the payload.
                let present = parcel.read_int32()?;
                if present == 0 {
                    out.push(Value::Parcelable(Parcelable::Null));
                } else {
                    out.push(Value::Parcelable(parcelable::read_payload(
                        &element, parcel,
                    )?));
                }
            }
            Value::List(Some(out))
        }
        WireOp::Unknown { note } => {
            return Err(ServiceError::BadRequest {
                interface: "aidl".into(),
                method: "decode".into(),
                detail: format!("undecoded wire op: {note}"),
            })
        }
    })
}

/// Write one value described by `op`.
pub fn write_op(parcel: &mut Parcel, op: &WireOp, value: &Value) -> Result<()> {
    match op {
        WireOp::I32 => parcel.write_int32(expect_int(value)?),
        WireOp::I64 => parcel.write_int64(expect_long(value)?),
        WireOp::F32 => parcel.write_float(expect_float(value)?),
        WireOp::F64 => parcel.write_double(expect_double(value)?),
        WireOp::Bool => parcel.write_bool(expect_bool(value)?),
        WireOp::Byte => parcel.write_byte(expect_byte(value)?),
        WireOp::Char => parcel.write_char(expect_char(value)?),
        WireOp::Str16 => parcel.write_string16_opt(expect_str(value)?)?,
        WireOp::Str8 => parcel.write_string8_opt(expect_str(value)?)?,
        WireOp::Binder { .. } => parcel.write_strong_binder_opt(value.as_binder()),
        WireOp::IntArray => parcel.write_int32_array(expect_int_array(value)?)?,
        WireOp::LongArray => parcel.write_int64_array(expect_long_array(value)?)?,
        WireOp::FloatArray => parcel.write_float_array(expect_float_array(value)?)?,
        WireOp::DoubleArray => parcel.write_double_array(expect_double_array(value)?)?,
        WireOp::ByteArray => parcel.write_byte_array(expect_byte_array(value)?)?,
        WireOp::BooleanArray | WireOp::CharArray => {
            return Err(bad("boolean[]/char[] are not modelled"))
        }
        WireOp::StringArray | WireOp::StringList => {
            let arr = expect_string_list(value)?;
            let refs: Option<Vec<&str>> = arr
                .as_ref()
                .map(|v| v.iter().map(|s| s.as_deref().unwrap_or("")).collect());
            parcel.write_string16_array(refs.as_deref())?;
        }
        WireOp::BinderList => {
            let Some(values) = expect_list(value)? else {
                parcel.write_int32(-1);
                return Ok(());
            };
            parcel.write_int32(i32::try_from(values.len()).map_err(|_| bad("list too long"))?);
            for v in values {
                parcel.write_strong_binder_opt(v.as_binder());
            }
        }
        WireOp::Parcelable { class: _ } => {
            let p: &Parcelable = match value {
                Value::Parcelable(p) | Value::TypedObject(p) | Value::NamedParcelable(p) => p,
                Value::Skipped | Value::Unit => {
                    return Err(bad(
                        "null is not representable for a direct writeToParcel argument",
                    ))
                }
                other => return Err(bad(format!("expected parcelable, got {other:?}"))),
            };
            match p {
                Parcelable::Null => {
                    return Err(bad(
                        "null is not representable for a direct writeToParcel argument",
                    ))
                }
                Parcelable::Ordered { .. } => parcelable::write_payload(parcel, p)?,
                Parcelable::Fd(_) => return Err(bad("fd is not a parcelable argument")),
            }
        }
        WireOp::TypedObject { class, .. } => {
            let fallback;
            let p: &Parcelable = match value {
                Value::Parcelable(p) | Value::TypedObject(p) | Value::NamedParcelable(p) => p,
                Value::Bundle(b) => {
                    fallback = b
                        .as_ref()
                        .map(Bundle::parcelable)
                        .unwrap_or(Parcelable::Null);
                    &fallback
                }
                Value::Skipped | Value::Unit => {
                    fallback = Parcelable::Null;
                    &fallback
                }
                other => return Err(bad(format!("expected parcelable, got {other:?}"))),
            };
            match p {
                Parcelable::Null => parcel.write_int32(0),
                Parcelable::Ordered { .. } => {
                    parcel.write_int32(1);
                    if class == "android.os.IBinder" {
                        let id = p
                            .field(0)
                            .and_then(Value::as_int)
                            .and_then(|v| u64::try_from(v).ok())
                            .map(acos_parcel::BinderId);
                        parcel.write_strong_binder_opt(id);
                    } else {
                        parcelable::write_payload(parcel, p)?;
                    }
                }
                Parcelable::Fd(_) => return Err(bad("fd is not a typed object")),
            }
        }
        WireOp::Bundle => {
            let b: Option<&Bundle> = match value {
                Value::Bundle(b) => b.as_ref(),
                Value::Skipped => None,
                _ => return Err(bad("expected bundle")),
            };
            parcelable::write_bundle(parcel, b)?;
        }
        WireOp::RawFd => write_fd(parcel, value)?,
        WireOp::Value => match value {
            Value::Dynamic(d) => parcelable::write_dynamic(parcel, d)?,
            _ => return Err(bad("expected dynamic value")),
        },
        WireOp::Cond { marker, ops } => {
            let Value::Cond { present, values } = value else {
                return Err(bad("expected conditional reply group"));
            };
            if values.len() != ops.len() && *present {
                return Err(bad("conditional body length mismatch"));
            }
            write_op(parcel, marker, &Value::Int(i32::from(*present)))?;
            if *present {
                for (o, v) in ops.iter().zip(values) {
                    write_op(parcel, o, v)?;
                }
            }
        }
        // An empty dynamic list/map is the honest "nothing there" reply for
        // queries such as `IJobScheduler.getAllPendingJobs` and
        // `getStartedJobs`: the wire form is only the `-1`/`0` count. A
        // non-empty body still fails loudly because its element type is not in
        // the decoded table.
        WireOp::List { .. } | WireOp::Array { .. } => match value {
            Value::List(None) => parcel.write_int32(-1),
            Value::List(Some(items)) if items.is_empty() => parcel.write_int32(0),
            _ => return Err(bad("dynamic list replies are only modelled empty")),
        },
        WireOp::Map { .. } => match value {
            Value::Map(None) => parcel.write_int32(-1),
            Value::Map(Some(entries)) if entries.is_empty() => parcel.write_int32(0),
            _ => return Err(bad("dynamic map replies are only modelled empty")),
        },
        WireOp::TypedArray { class } | WireOp::TypedList { class } => {
            let Some(values) = expect_list(value)? else {
                parcel.write_int32(-1);
                return Ok(());
            };
            parcel.write_int32(i32::try_from(values.len()).map_err(|_| bad("list too long"))?);
            for v in values {
                let p = v
                    .as_parcelable()
                    .ok_or_else(|| bad(format!("expected {class} element")))?;
                // AIDL writes an array/list of parcelables with
                // `writeTypedArray`/`writeTypedList`: a per-element presence
                // marker before the payload (the same one the reader expects).
                match p {
                    crate::value::Parcelable::Null => parcel.write_int32(0),
                    _ => {
                        parcel.write_int32(1);
                        parcelable::write_payload(parcel, p)?;
                    }
                }
            }
        }
        WireOp::Unknown { note } => return Err(bad(format!("undecoded wire op: {note}"))),
    }
    Ok(())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Int(i) => *i != 0,
        Value::Bool(b) => *b,
        Value::Long(l) => *l != 0,
        _ => false,
    }
}

fn read_fd(parcel: &mut Parcel) -> Result<Option<acos_parcel::ParcelFd>> {
    match parcel.read_file_descriptor()? {
        None => Ok(None),
        Some(raw) => {
            let borrowed = parcel
                .borrowed_file_descriptor(raw)
                .ok_or_else(|| bad("parcel does not own the returned fd"))?;
            acos_parcel::ParcelFd::dup(borrowed)
                .map(Some)
                .map_err(ServiceError::Parcel)
        }
    }
}

fn write_fd(parcel: &mut Parcel, value: &Value) -> Result<()> {
    match value {
        Value::RawFd(Some(fd)) => parcel
            .write_dup_file_descriptor(fd.as_fd())
            .map_err(ServiceError::Parcel),
        Value::RawFd(None) | Value::Skipped => {
            parcel.write_int32(0);
            Ok(())
        }
        _ => Err(bad("expected fd")),
    }
}

fn bad(detail: impl Into<String>) -> ServiceError {
    ServiceError::BadReply {
        interface: "codec".into(),
        method: "op".into(),
        detail: detail.into(),
    }
}

fn expect_int(v: &Value) -> Result<i32> {
    v.as_int()
        .ok_or_else(|| bad(format!("expected int, got {v:?}")))
}
fn expect_long(v: &Value) -> Result<i64> {
    v.as_long()
        .ok_or_else(|| bad(format!("expected long, got {v:?}")))
}
fn expect_float(v: &Value) -> Result<f32> {
    v.as_float()
        .ok_or_else(|| bad(format!("expected float, got {v:?}")))
}
fn expect_double(v: &Value) -> Result<f64> {
    match v {
        Value::Double(d) => Ok(*d),
        _ => Err(bad(format!("expected double, got {v:?}"))),
    }
}
fn expect_bool(v: &Value) -> Result<bool> {
    v.as_bool()
        .ok_or_else(|| bad(format!("expected bool, got {v:?}")))
}
fn expect_byte(v: &Value) -> Result<i8> {
    match v {
        Value::Byte(b) => Ok(*b),
        _ => Err(bad(format!("expected byte, got {v:?}"))),
    }
}
fn expect_char(v: &Value) -> Result<u16> {
    match v {
        Value::Char(c) => Ok(*c),
        _ => Err(bad(format!("expected char, got {v:?}"))),
    }
}
fn expect_str(v: &Value) -> Result<Option<&str>> {
    match v {
        Value::Str(s) | Value::Str8(s) => Ok(s.as_deref()),
        _ => Err(bad(format!("expected string, got {v:?}"))),
    }
}
fn expect_string_list(v: &Value) -> Result<Option<Vec<Option<String>>>> {
    match v {
        Value::StringArray(a) | Value::StringList(a) => Ok(a.clone()),
        _ => Err(bad(format!("expected string list, got {v:?}"))),
    }
}
fn expect_int_array(v: &Value) -> Result<Option<&[i32]>> {
    match v {
        Value::IntArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected int array, got {v:?}"))),
    }
}
fn expect_long_array(v: &Value) -> Result<Option<&[i64]>> {
    match v {
        Value::LongArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected long array, got {v:?}"))),
    }
}
fn expect_float_array(v: &Value) -> Result<Option<&[f32]>> {
    match v {
        Value::FloatArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected float array, got {v:?}"))),
    }
}
fn expect_double_array(v: &Value) -> Result<Option<&[f64]>> {
    match v {
        Value::DoubleArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected double array, got {v:?}"))),
    }
}
fn expect_byte_array(v: &Value) -> Result<Option<&[u8]>> {
    match v {
        Value::ByteArray(a) => Ok(a.as_deref()),
        _ => Err(bad(format!("expected byte array, got {v:?}"))),
    }
}
fn expect_list(v: &Value) -> Result<Option<&[Value]>> {
    match v {
        Value::List(l) => Ok(l.as_deref()),
        _ => Err(bad(format!("expected list, got {v:?}"))),
    }
}

/// Encode the request side of a measured call exactly as the framework proxy
/// would: interface token then each decoded argument.
/// Write the argument list for a method with an image-verified layout (see
/// [`read_args_override`]). `Ok(false)` means "no override, use the table".
///
/// This is the writer side of the same seam: a caller that builds a request by
/// hand (the replay oracle in `tests/replay.rs`) must produce the image's bytes,
/// not the table's.
pub fn write_args_override(
    interface: &str,
    code: u32,
    args: &[Value],
    parcel: &mut Parcel,
) -> Result<bool> {
    match (interface, code) {
        ("android.content.pm.IPackageManager", IPACKAGE_MANAGER_NOTIFY_DEX_LOAD) => {
            let bad = |detail: String| ServiceError::BadRequest {
                interface: interface.to_string(),
                method: "notifyDexLoad".to_string(),
                detail,
            };
            let [package, map, loader] = args else {
                return Err(bad(format!("expected 3 args, got {}", args.len())));
            };
            let Value::Str(package) = package else {
                return Err(bad("arg 0 is not a String".into()));
            };
            let Value::Str(loader) = loader else {
                return Err(bad("arg 2 is not a String".into()));
            };
            parcel
                .write_string16_opt(package.as_deref())
                .map_err(ServiceError::Parcel)?;
            let Value::Map(entries) = map else {
                return Err(bad("arg 1 is not a Map".into()));
            };
            match entries {
                None => parcel.write_int32(-1),
                Some(entries) => {
                    parcel.write_int32(
                        i32::try_from(entries.len()).map_err(|_| bad("map too large".into()))?,
                    );
                    for (key, value) in entries {
                        for side in [key, value] {
                            let crate::value::DynamicValue::String(s) = side else {
                                return Err(bad("map entry is not String->String".into()));
                            };
                            parcel
                                .write_string16_opt(s.as_deref())
                                .map_err(ServiceError::Parcel)?;
                        }
                    }
                }
            }
            parcel
                .write_string16_opt(loader.as_deref())
                .map_err(ServiceError::Parcel)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Build a request parcel (interface token + decoded args) for a measured call.
pub fn build_request(
    interface: &str,
    method: &acos_aidl::MethodInfo,
    args: &[Value],
) -> Result<Parcel> {
    let mut parcel = Parcel::new();
    parcel
        .write_interface_token(interface)
        .map_err(ServiceError::Parcel)?;
    if !write_args_override(interface, method.code, args, &mut parcel)? {
        write_reply(&mut parcel, &method.args, args, interface, &method.name)?;
    }
    Ok(parcel)
}

/// Decode the reply side of a measured call: exception header then the decoded
/// reply values.
pub fn parse_reply(
    interface: &str,
    method: &acos_aidl::MethodInfo,
    reply: &mut Parcel,
) -> Result<Vec<Value>> {
    let code = reply.read_int32()?;
    if code != crate::exception::NO_EXCEPTION {
        return Err(ServiceError::BadRequest {
            interface: interface.to_string(),
            method: method.name.clone(),
            detail: format!("android exception {code}"),
        });
    }
    read_args(reply, &method.reply, interface, &method.name)
}

/// Convenience: a `DynamicValue` string.
#[must_use]
pub fn dyn_string(s: impl Into<String>) -> DynamicValue {
    DynamicValue::String(Some(s.into()))
}
