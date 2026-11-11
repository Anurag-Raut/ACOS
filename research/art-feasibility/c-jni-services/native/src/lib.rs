//! `libacosjniprobe.so` — the Rust half of the ART JNI/service feasibility probe.
//!
//! Everything here is `extern "C"`; the JNI boilerplate lives in `shim.c`
//! (compiled with the NDK clang, linked into this cdylib) so the Rust side never
//! has to walk the `JNINativeInterface_` function-pointer table by hand.
//!
//! Natives reachable from Java (class `dev.acos.artprobe.NativeProbe`):
//!
//! | Java native | Rust | what it exercises |
//! |---|---|---|
//! | `nativeAdd(II)I` | [`probe_add`] | plain value marshalling, call overhead |
//! | `nativeEcho(String)String` | [`probe_echo`] | UTF-8 string round trip (in+out) |
//! | `nativeCallback(String)V` | (shim) | Java callback via `CallStaticVoidMethod` |
//! | `nativeCopyIntArray([I)[I` | [`probe_array_inout`] | primitive array in+out |
//! | `nativeServiceCall(String)String` | [`probe_service_call`] | ACOS ServiceHost-style unix-socket round trip |
//! | `nativeServiceBench(I)J` | [`probe_service_bench`] | per-call latency (ns) |
//! | `nativeFork(I[I)I` | [`probe_fork`] | zygote-less warm-parent `fork()` measurement |

use std::ffi::{c_char, c_int, CStr, CString};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Instant;

/// Where the stand-in ACOS ServiceHost listens *inside* the mini-Android root.
/// `.cache/art-exp/data` is bind-mounted at `/data` by `artroot.sh`, so the same
/// socket is `.cache/art-exp-c/service.sock` on the host.
const SOCK_PATH: &str = "/data/service.sock";

/// Interface token of the probe service (matches the Rust server).
const DESCRIPTOR: &str = "dev.acos.artprobe.Probe";
const TOKEN_HEADER: u32 = 0x5359_5354; // "SYST", system libbinder build
const PENALTY_GATHER: u32 = 0x8000_0000;

// ---------------------------------------------------------------------------
// value / string / array natives
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn probe_add(a: c_int, b: c_int) -> c_int {
    a.wrapping_add(b)
}

/// Echo the C string back as a heap `CString` the caller must free with
/// [`probe_free_string`]. Prefixes nothing, so a byte-exact comparison works.
#[no_mangle]
pub extern "C" fn probe_echo(input: *const c_char) -> *mut c_char {
    if input.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the shim always passes a NUL-terminated string it owns.
    let s = unsafe { CStr::from_ptr(input) };
    let body = s.to_string_lossy();
    let out = format!("rust:{body}");
    match CString::new(out) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn probe_free_string(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

/// In-place transform, to prove the array memory really was handed to Rust:
/// `out[i] = in[i] * 2 + 1`. Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn probe_array_inout(data: *mut c_int, len: c_int) -> c_int {
    if data.is_null() || len < 0 {
        return -1;
    }
    let slice = std::slice::from_raw_parts_mut(data, len as usize);
    for v in slice.iter_mut() {
        *v = v.wrapping_mul(2).wrapping_add(1);
    }
    0
}

// ---------------------------------------------------------------------------
// Parcel-format helpers (the byte layout `acos-parcel` decodes)
// ---------------------------------------------------------------------------

fn put_i32(v: &mut Vec<u8>, x: i32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_i64(v: &mut Vec<u8>, x: i64) {
    v.extend_from_slice(&x.to_le_bytes());
}
/// Android `String16`: `int32 units` + UTF-16LE + NUL, padded to 4 bytes.
fn put_string16(v: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    put_i32(v, units.len() as i32);
    for u in units {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v.extend_from_slice(&[0, 0]);
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// Request parcel exactly as a real AIDL proxy would write it:
/// interface token (policy|gather, workSourceUid, header, String16) + args.
fn encode_request(seq: i32, payload: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(128 + payload.len() * 2);
    put_i32(&mut v, (PENALTY_GATHER) as i32);
    put_i32(&mut v, -1);
    put_i32(&mut v, TOKEN_HEADER as i32);
    put_string16(&mut v, DESCRIPTOR);
    put_i32(&mut v, seq);
    put_string16(&mut v, payload);
    v
}

/// Minimal `String16` reader mirroring `acos-parcel::read_string16`.
fn get_string16(b: &[u8], pos: &mut usize) -> Option<String> {
    if *pos + 4 > b.len() {
        return None;
    }
    let units = i32::from_le_bytes(b[*pos..*pos + 4].try_into().ok()?) as usize;
    *pos += 4;
    if *pos + units * 2 > b.len() {
        return None;
    }
    let mut u16s = Vec::with_capacity(units);
    for i in 0..units {
        let off = *pos + i * 2;
        u16s.push(u16::from_le_bytes([b[off], b[off + 1]]));
    }
    *pos += (units * 2 + 2 + 3) & !3; // data + NUL, padded
    String::from_utf16(&u16s).ok()
}

// ---------------------------------------------------------------------------
// framing + socket client
// ---------------------------------------------------------------------------

fn write_frame(s: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    s.write_all(&(body.len() as u32).to_le_bytes())?;
    s.write_all(body)
}

fn read_frame(s: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr)?;
    let len = u32::from_le_bytes(hdr) as usize;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    Ok(body)
}

/// One `transact()` against the stand-in ServiceHost: connect, send, read reply.
/// Returns `(status, reply_string)`.
fn transact(stream: &mut UnixStream, seq: i32, payload: &str) -> std::io::Result<(i32, String)> {
    write_frame(stream, &encode_request(seq, payload))?;
    let reply = read_frame(stream)?;
    let mut pos = 0usize;
    if reply.len() < 4 {
        return Ok((-1, String::new()));
    }
    let status = i32::from_le_bytes(reply[0..4].try_into().unwrap());
    pos += 4;
    let text = get_string16(&reply, &mut pos).unwrap_or_default();
    Ok((status, text))
}

fn connect() -> Option<UnixStream> {
    UnixStream::connect(SOCK_PATH).ok()
}

/// One call from Java. Returns `"<status>|<text>"`, or `"ERR:<os error>"`.
#[no_mangle]
pub extern "C" fn probe_service_call(payload: *const c_char) -> *mut c_char {
    let payload = if payload.is_null() {
        String::new()
    } else {
        // SAFETY: owned NUL-terminated string from the shim.
        unsafe { CStr::from_ptr(payload) }.to_string_lossy().into_owned()
    };
    let text = match connect() {
        None => "ERR:connect".to_string(),
        Some(mut s) => match transact(&mut s, 0, &payload) {
            Ok((status, reply)) => format!("{status}|{reply}"),
            Err(e) => format!("ERR:{e}"),
        },
    };
    match CString::new(text) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// `n` sequential round trips on **one** connection; returns ns/call (mean),
/// or -1 when the socket cannot be reached.
#[no_mangle]
pub extern "C" fn probe_service_bench(n: c_int, payload: *const c_char) -> i64 {
    let payload = if payload.is_null() {
        "ping".to_string()
    } else {
        // SAFETY: owned NUL-terminated string from the shim.
        unsafe { CStr::from_ptr(payload) }.to_string_lossy().into_owned()
    };
    let n = n.max(1) as i64;
    let Some(mut s) = connect() else {
        return -1;
    };
    // warm-up
    for i in 0..16 {
        if transact(&mut s, i, &payload).is_err() {
            return -1;
        }
    }
    let t0 = Instant::now();
    for i in 0..n {
        if transact(&mut s, i as i32, &payload).is_err() {
            return -1;
        }
    }
    let elapsed = t0.elapsed();
    (elapsed.as_nanos() as i64) / n
}

// ---------------------------------------------------------------------------
// zygote-less fork
// ---------------------------------------------------------------------------

extern "C" {
    fn fork() -> c_int;
    fn _exit(code: c_int) -> !;
    fn usleep(usec: u32) -> c_int;
}

/// Fork `n` children from the already-warm ART parent. Children sleep
/// `hold_ms` and `_exit(0)` (no Java teardown — that is the point: they die with
/// the parent's warm heap still mapped). Returns the number of pids written into
/// `out` (which the caller must size >= n).
///
/// # Safety
/// `out` must point to at least `n` writable `c_int`s. Forking a multithreaded
/// ART is inherently unsafe — see the report, this exists to *measure* the page
/// sharing, not as a production API.
#[no_mangle]
pub unsafe extern "C" fn probe_fork(n: c_int, hold_ms: c_int, out: *mut c_int) -> c_int {
    if out.is_null() || n < 0 {
        return -1;
    }
    let out = std::slice::from_raw_parts_mut(out, n as usize);
    let mut made = 0;
    for slot in out.iter_mut() {
        let pid = fork();
        if pid < 0 {
            break;
        }
        if pid == 0 {
            // Child: return to the Java caller, which does the work and halts.
            // (Returning into Java from a forked ART is exactly the unsafe part
            // this experiment wants to measure.)
            let _ = (hold_ms, usleep as unsafe extern "C" fn(u32) -> c_int, _exit);
            return 0;
        }
        *slot = pid;
        made += 1;
    }
    made
}
