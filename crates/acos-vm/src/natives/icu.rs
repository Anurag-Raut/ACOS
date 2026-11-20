//! `com.android.icu.charset.NativeConverter` (ICU charset converters).
//!
//! The M1 console tier only needs UTF-8 / ISO-8859-1 / US-ASCII (+ UTF-16).
//! Converters are implemented in Rust and keyed by an opaque `jlong` handle;
//! ICU data files are never opened.

use std::sync::{Mutex, OnceLock};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Object, NULL};
use crate::vm::{InvokeKind, Vm};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Lcom/android/icu/charset/NativeConverter;";
    vm.register_native(
        c,
        "charsetForName",
        "(Ljava/lang/String;)Ljava/nio/charset/Charset;",
        charset_for_name,
    );
    vm.register_native(c, "openConverter", "(Ljava/lang/String;)J", open_converter);
    vm.register_native(c, "closeConverter", "(J)V", close_converter);
    vm.register_native(c, "getMaxBytesPerChar", "(J)I", get_max_bytes);
    vm.register_native(c, "getAveBytesPerChar", "(J)F", get_ave_bytes);
    vm.register_native(c, "getAveCharsPerByte", "(J)F", get_ave_chars);
    vm.register_native(c, "getNativeFinalizer", "()J", native_finalizer);
    vm.register_native(c, "setCallbackEncode", "(JII[B)V", set_callback_encode);
    vm.register_native(
        c,
        "setCallbackDecode",
        "(JIILjava/lang/String;)V",
        set_callback_decode,
    );
    vm.register_native(c, "resetCharToByte", "(J)V", reset_converter);
    vm.register_native(c, "resetByteToChar", "(J)V", reset_converter);
    vm.register_native(c, "encode", "(J[CI[BI[IZ)I", encode);
    vm.register_native(c, "decode", "(J[BI[CI[IZ)I", decode);
    vm.register_native(
        c,
        "contains",
        "(Ljava/lang/String;Ljava/lang/String;)Z",
        contains,
    );
    vm.register_native(
        c,
        "getAvailableCharsetNames",
        "()[Ljava/lang/String;",
        available_names,
    );
    vm.register_native(c, "getSubstitutionBytes", "(J)[B", substitution_bytes);
    // ICU default-locale state. ACOS's ICU is a Rust shim with no ICU data, so
    // the identity is recorded rather than handed to a native ICU library. The
    // Java side still owns every observable decision (`Locale.getDefault`);
    // this only mirrors it into the shim.
    let l = "Lcom/android/icu/util/LocaleNative;";
    vm.register_native(
        l,
        "setDefaultNative",
        "(Ljava/lang/String;)V",
        locale_set_default,
    );
    // `com.android.icu.util.regex.PatternNative` builds a `NativeAllocationRegistry`
    // in `<clinit>`. ACOS has no native ICU regex objects, so there is no free
    // function to return (`0` = no finalizer). Actual compilation/matching stays
    // a structured `UNSUPPORTED_JAVA_API`.
    vm.register_native(
        "Lcom/android/icu/util/regex/PatternNative;",
        "getNativeFinalizer",
        "()J",
        pattern_no_finalizer,
    );
    Ok(())
}

fn pattern_no_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn default_locale_slot() -> &'static Mutex<String> {
    static LOCALE: OnceLock<Mutex<String>> = OnceLock::new();
    LOCALE.get_or_init(|| Mutex::new(String::from("en-US")))
}

fn locale_set_default(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tag = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    *default_locale_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = tag;
    Ok(())
}

/// Canonicalise a charset name to the subset ACOS implements.
fn canonical(name: &str) -> Option<&'static str> {
    let up = name.trim().to_ascii_uppercase().replace('_', "-");
    match up.as_str() {
        "UTF-8" | "UTF8" => Some("UTF-8"),
        "UTF-16" | "UTF-16LE" | "UTF16" => Some("UTF-16LE"),
        "UTF-16BE" => Some("UTF-16BE"),
        "ISO-8859-1" | "ISO8859-1" | "LATIN1" | "LATIN-1" | "8859_1" => Some("ISO-8859-1"),
        "US-ASCII" | "ASCII" | "USASCII" | "646" | "ANSI-X3.4-1968" | "ISO646-US" => {
            Some("US-ASCII")
        }
        _ => None,
    }
}

fn charset_for_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name_h = vm.arg_ref(ctx, 0);
    let raw = vm.string_to_rust(name_h).unwrap_or_default();
    let canon = canonical(&raw).unwrap_or("UTF-8");
    // Materialise a real `CharsetICU` so the framework's `newEncoder()` path
    // works unchanged; the ICU data itself is never consulted.
    let cid = vm.load_class("Lcom/android/icu/charset/CharsetICU;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let name = vm.new_string(canon)?;
        vm.native_roots.push(name);
        let aliases = vm.alloc(Object::Array(ArrayObj {
            elem_desc: "Ljava/lang/String;".to_string(),
            data: ArrayData::Ref(Vec::new()),
        }))?;
        vm.native_roots.push(aliases);
        let ctor = vm.find_method(
            cid,
            "<init>",
            "(Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V",
            InvokeKind::Direct,
        )?;
        crate::interp::call_method(vm, ctor, obj, &[name as u64, name as u64, aliases as u64])?;
        vm.native_roots.pop();
        vm.native_roots.pop();
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn open_converter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name_h = vm.arg_ref(ctx, 0);
    let raw = vm.string_to_rust(name_h).unwrap_or_default();
    let canon = canonical(&raw).ok_or_else(|| {
        VmError::unsupported_api(
            format!("UNSUPPORTED_CHARSET: {raw}"),
            "com.android.icu.charset.NativeConverter.openConverter".to_string(),
        )
    })?;
    let id = vm.next_converter;
    vm.next_converter += 1;
    vm.converters.insert(id, canon.to_string());
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn close_converter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    vm.converters.remove(&id);
    Ok(())
}

fn native_finalizer(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    _vm.ret_i64(ctx, 0);
    Ok(())
}

fn set_callback_encode(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn set_callback_decode(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn reset_converter(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn converter_name(vm: &Vm, id: u64) -> String {
    vm.converters
        .get(&id)
        .cloned()
        .unwrap_or_else(|| "UTF-8".to_string())
}

fn get_max_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    let n = match converter_name(vm, id).as_str() {
        "UTF-8" => 4,
        "UTF-16LE" | "UTF-16BE" => 2,
        _ => 1,
    };
    vm.ret_i32(ctx, n);
    Ok(())
}

fn get_ave_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    let f = match converter_name(vm, id).as_str() {
        "UTF-8" => 1.1f32,
        "UTF-16LE" | "UTF-16BE" => 2.0,
        _ => 1.0,
    };
    vm.ret_f32(ctx, f);
    Ok(())
}

fn get_ave_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    let f = match converter_name(vm, id).as_str() {
        "UTF-8" => 1.0f32,
        "UTF-16LE" | "UTF-16BE" => 0.5,
        _ => 1.0,
    };
    vm.ret_f32(ctx, f);
    Ok(())
}

fn contains(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_ref(ctx, 0);
    let b = vm.arg_ref(ctx, 1);
    let a = vm.string_to_rust(a).unwrap_or_default();
    let b = vm.string_to_rust(b).unwrap_or_default();
    vm.ret_bool(ctx, canonical(&a) == canonical(&b));
    Ok(())
}

fn available_names(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mut elems = Vec::new();
    for n in ["UTF-8", "ISO-8859-1", "US-ASCII", "UTF-16LE", "UTF-16BE"] {
        let h = vm.new_string(n)?;
        elems.push(h);
    }
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(elems),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn substitution_bytes(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = _vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(vec![b'?' as i8]),
    }))?;
    _vm.ret_ref(ctx, h);
    Ok(())
}

// --- encode / decode ------------------------------------------------------

const U_BUFFER_OVERFLOW: i32 = 15;
const U_INVALID_CHAR_FOUND: i32 = 10;
const U_ILLEGAL_CHAR_FOUND: i32 = 12;

fn encode(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    let input = vm.arg_ref(ctx, 1);
    let in_end = vm.arg_i32(ctx, 2).max(0) as usize;
    let output = vm.arg_ref(ctx, 3);
    let out_end = vm.arg_i32(ctx, 4).max(0) as usize;
    let data = vm.arg_ref(ctx, 5);
    let charset = converter_name(vm, id);

    let chars = char_array(vm, input).unwrap_or_default();
    let (mut ci, mut bo) = read_offsets(vm, data);
    let limit_chars = in_end.min(chars.len());
    let limit_bytes = out_end;

    let mut out: Vec<u8> = Vec::new();
    let mut overflow = false;
    while ci < limit_chars {
        let ch = chars[ci];
        let piece = encode_char(ch, &chars, &mut ci, &charset);
        match piece {
            Some(bytes) => {
                if bo + out.len() + bytes.len() > limit_bytes {
                    overflow = true;
                    break;
                }
                out.extend_from_slice(&bytes);
            }
            None => {
                let mut result = U_INVALID_CHAR_FOUND;
                if (charset == "UTF-8" || charset == "UTF-16LE" || charset == "UTF-16BE")
                    && (0xD800..=0xDBFF).contains(&ch)
                    && ci + 1 >= limit_chars
                {
                    result = U_ILLEGAL_CHAR_FOUND;
                }
                write_offsets(vm, data, ci, bo);
                vm.ret_i32(ctx, result);
                return Ok(());
            }
        }
    }
    // Copy produced bytes into the output array.
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Byte(v),
        ..
    })) = vm.heap.get_mut(output)
    {
        for (k, b) in out.iter().enumerate() {
            if bo + k < v.len() {
                v[bo + k] = *b as i8;
            }
        }
    }
    // `CharsetEncoderICU.setPosition` uses absolute byte offsets.
    bo += out.len();
    write_offsets(vm, data, ci, bo);
    vm.ret_i32(ctx, if overflow { U_BUFFER_OVERFLOW } else { 0 });
    Ok(())
}

/// Encode one Unicode scalar starting at `*ci`, advancing `*ci`.
fn encode_char(ch: u16, chars: &[u16], ci: &mut usize, charset: &str) -> Option<Vec<u8>> {
    match charset {
        "UTF-8" => {
            // Combine a surrogate pair into a scalar.
            if (0xD800..=0xDBFF).contains(&ch) {
                if *ci + 1 < chars.len() && (0xDC00..=0xDFFF).contains(&chars[*ci + 1]) {
                    let scalar =
                        0x10000 + (((ch as u32 - 0xD800) << 10) | (chars[*ci + 1] as u32 - 0xDC00));
                    *ci += 2;
                    let c = char::from_u32(scalar).unwrap_or('\u{FFFD}');
                    let mut buf = [0u8; 4];
                    return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
                }
                return None;
            }
            if (0xDC00..=0xDFFF).contains(&ch) {
                return None;
            }
            *ci += 1;
            Some(
                char::from_u32(ch as u32)
                    .unwrap_or('\u{FFFD}')
                    .to_string()
                    .into_bytes(),
            )
        }
        "UTF-16LE" => {
            *ci += 1;
            Some(ch.to_le_bytes().to_vec())
        }
        "UTF-16BE" => {
            *ci += 1;
            Some(ch.to_be_bytes().to_vec())
        }
        "US-ASCII" => {
            if ch > 0x7F {
                return None;
            }
            *ci += 1;
            Some(vec![ch as u8])
        }
        _ => {
            // ISO-8859-1 and single-byte charsets.
            *ci += 1;
            Some(vec![ch as u8])
        }
    }
}

fn decode(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_u64(ctx, 0);
    let input = vm.arg_ref(ctx, 1);
    let in_end = vm.arg_i32(ctx, 2).max(0) as usize;
    let output = vm.arg_ref(ctx, 3);
    let out_end = vm.arg_i32(ctx, 4).max(0) as usize;
    let data = vm.arg_ref(ctx, 5);
    let charset = converter_name(vm, id);

    let bytes = byte_array(vm, input).unwrap_or_default();
    let (start_bi, start_co) = read_offsets(vm, data);
    let mut bi = start_bi.min(bytes.len());
    let co = start_co;
    let limit_bytes = in_end.min(bytes.len());
    let mut out: Vec<u16> = Vec::new();
    let mut overflow = false;
    while bi < limit_bytes {
        if co + out.len() >= out_end {
            overflow = true;
            break;
        }
        let ch = bytes[bi] as u8;
        bi += 1;
        match charset.as_str() {
            "UTF-16LE" => {
                if bi < limit_bytes {
                    out.push(u16::from_le_bytes([ch, bytes[bi] as u8]));
                    bi += 1;
                }
            }
            "UTF-16BE" => {
                if bi < limit_bytes {
                    out.push(u16::from_be_bytes([ch, bytes[bi] as u8]));
                    bi += 1;
                }
            }
            _ => out.push(ch as u16),
        }
    }
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Char(v),
        ..
    })) = vm.heap.get_mut(output)
    {
        for (k, c) in out.iter().enumerate() {
            if co + k < v.len() {
                v[co + k] = *c;
            }
        }
    }
    // `CharsetDecoderICU.setPosition` adds `data[i]` to the buffer position, so
    // these are the *counts* consumed/produced in this call, not absolute
    // array offsets (AOSP's JNI binding subtracts the starting offsets).
    write_offsets(vm, data, bi - start_bi, out.len());
    vm.ret_i32(ctx, if overflow { U_BUFFER_OVERFLOW } else { 0 });
    Ok(())
}

fn read_offsets(vm: &Vm, data: crate::heap::Handle) -> (usize, usize) {
    match vm.heap.get(data) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Int(v),
            ..
        })) => (
            v.first().copied().unwrap_or(0).max(0) as usize,
            v.get(1).copied().unwrap_or(0).max(0) as usize,
        ),
        _ => (0, 0),
    }
}

fn write_offsets(vm: &mut Vm, data: crate::heap::Handle, a: usize, b: usize) {
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Int(v),
        ..
    })) = vm.heap.get_mut(data)
    {
        if !v.is_empty() {
            v[0] = a as i32;
        }
        if v.len() > 1 {
            v[1] = b as i32;
        }
    }
}

fn char_array(vm: &Vm, h: crate::heap::Handle) -> Option<Vec<u16>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Char(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

fn byte_array(vm: &Vm, h: crate::heap::Handle) -> Option<Vec<i8>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Byte(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

// Keep `NULL` referenced for future object converters.
const _: crate::heap::Handle = NULL;
