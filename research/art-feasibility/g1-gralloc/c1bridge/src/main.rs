#![allow(dead_code, unused_imports, clippy::all)]
//! G1 item (2) consumer half: the frame the stock BufferQueue producer queued in
//! the ART sandbox is applied through **C1's real `surface_client`** path.
//!
//! Wire: one `SOCK_SEQPACKET` frame per cycle, the `acos_binder_peer` frame
//! shape (48-byte header + data + `SCM_RIGHTS` fds). The probe sends the
//! shim's 88-byte `GraphicBuffer` flatten form, the frame's memfd, and
//! `(w,h,stride,format,producer_checksum)`. This harness builds the **legacy
//! `android.ui.ISurfaceComposer` SET_TRANSACTION_STATE parcel** (code 8) that
//! BLAST sends, transacts it into the installed `SurfaceFlinger` node, and C1
//! decodes + applies it: the layer's latest buffer is replaced and
//! `onReleaseBuffer` is queued for the buffer it replaced. The pixel checksum
//! the probe compares against is computed **here**, by reading the fd C1 stored
//! — i.e. on the consumer side of the buffer hand-off.

include!("helpers.rs");

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn pread_all(fd: i32, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0usize;
    while off < buf.len() {
        let n = unsafe {
            libc::pread(
                fd,
                buf[off..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - off,
                off as libc::off_t,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            break;
        }
        off += n as usize;
    }
    Ok(())
}

#[derive(Default, Clone, Copy)]
struct Reply {
    status: i32,
    released_frame: u32,
    released_ok: u32,
    release_calls: u64,
    latch_checksum: u64,
    stored_bytes: u64,
}

fn reply_bytes(r: &Reply) -> Vec<u8> {
    let mut v = Vec::with_capacity(40);
    v.extend_from_slice(&r.status.to_le_bytes());
    v.extend_from_slice(&r.released_frame.to_le_bytes());
    v.extend_from_slice(&r.released_ok.to_le_bytes());
    v.extend_from_slice(&[0u8; 4]); // padding: C struct alignment (40 bytes)
    v.extend_from_slice(&r.release_calls.to_le_bytes());
    v.extend_from_slice(&r.latch_checksum.to_le_bytes());
    v.extend_from_slice(&r.stored_bytes.to_le_bytes());
    v
}

fn main() {
    let path = std::env::args().nth(1).expect("socket path");
    let _ = std::fs::remove_file(&path);
    let lfd = acos_binder_peer::protocol::listen(&path).expect("listen");
    eprintln!("[c1bridge] listening on {path}");
    let cfd = acos_binder_peer::protocol::accept(lfd).expect("accept");
    eprintln!("[c1bridge] producer connected");

    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = host
        .check_service("SurfaceFlinger")
        .expect("lookup")
        .expect("SurfaceFlinger");
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "blast");
    let listener = host.register_aux(ITRANSACTION_LISTENER);

    let mut release_calls: u64 = 0;
    let mut last_released: u64 = 0;
    let mut frames: u64 = 0;

    loop {
        let f = match acos_binder_peer::protocol::read_frame(cfd) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[c1bridge] read_frame: {e}");
                break;
            }
        };
        if f.fds.is_empty() || f.data.len() < 88 + 24 {
            eprintln!("[c1bridge] short frame: {} bytes {} fds", f.data.len(), f.fds.len());
            break;
        }
        let (flat, meta) = f.data.split_at(88);
        let u32at = |o: usize| u32::from_le_bytes(meta[o..o + 4].try_into().expect("4"));
        let checksum = u64::from_le_bytes(meta[16..24].try_into().expect("8"));
        let frame_number = f.seq as u64 + 1;
        let buffer_id = 1000 + f.seq as u64;
        let fd = unsafe { OwnedFd::from_raw_fd(f.fds[0]) };
        let rb = RealBuf {
            flat: flat.to_vec(),
            fd,
            w: u32at(0),
            h: u32at(4),
            stride: u32at(8),
            format: u32at(12) as i32,
            checksum,
        };
        *REAL_BUF.lock().unwrap() = Some(rb);

        let callback = CallbackId {
            id: f.seq as i64,
            kind: 0,
        };
        let mut parcel = transaction_parcel(
            &[(
                handle,
                layer_id,
                Some((listener, frame_number, buffer_id)),
                vec![(listener, vec![callback])],
            )],
            &[0, 0, 0, 0, 0, f.seq as i32],
            &[(listener, vec![callback])],
            frame_number,
        );
        let mut reply = Parcel::new();
        let trc = host.transact(sf, SET_TRANSACTION_STATE, &mut parcel, &mut reply, 0);

        // C1's reverse deliveries: code 2 == onReleaseBuffer(ReleaseCallbackId, Fence, uint32)
        let mut released_ok = 0u32;
        let mut released_frame = 0u32;
        for d in host.take_deliveries() {
            if d.code == 2 {
                release_calls += 1;
                let mut data = d.data.clone();
                if data.enforce_interface(ITRANSACTION_LISTENER).is_ok()
                    && data.read_int32() == Ok(1)
                {
                    let _buffer = data.read_uint64();
                    let frame = data.read_uint64().unwrap_or(0);
                    last_released = frame;
                    released_frame = frame as u32;
                    released_ok = 1;
                }
            }
        }

        // The checksum is taken from *C1's store*: the fd the transaction gave
        // the compositor client, not the producer's copy.
        {
            let s = state.lock().unwrap();
            eprintln!(
                "[c1bridge] frame {} transact_ok {} transactions_decoded {} layer={:?} latest={:?}",
                frame_number,
                trc.is_ok(),
                s.transactions_decoded,
                s.layer(handle).map(|l| l.layer_id),
                s.latest_buffer(handle).map(|b| (b.frame_number, b.buffer_id, b.width, b.stride))
            );
        }
        let mut stored_bytes = 0u64;
        let mut latch_checksum = 0u64;
        {
            let (s_fd, w, h, stride, stored_frame) = {
                let s = state.lock().unwrap();
                match s.latest_buffer(handle) {
                    Some(lb) => (
                        lb.fd.as_raw_fd(),
                        lb.width,
                        lb.height,
                        lb.stride,
                        lb.frame_number,
                    ),
                    None => (-1, 0, 0, 0, 0),
                }
            };
            if s_fd >= 0 && stored_frame == frame_number {
                let len = (stride as usize) * (h as usize) * 4;
                let mut buf = vec![0u8; len];
                if pread_all(s_fd, &mut buf).is_ok() {
                    stored_bytes = len as u64;
                    latch_checksum = fnv(&buf);
                }
            }
        }

        let rep = Reply {
            status: if trc.is_ok() { 0 } else { -1 },
            released_frame,
            released_ok,
            release_calls,
            latch_checksum,
            stored_bytes,
        };
        let out = acos_binder_peer::protocol::Frame::plain(
            acos_binder_peer::protocol::F_RSP,
            f.seq,
            0,
            0,
            0,
            reply_bytes(&rep),
        );
        if acos_binder_peer::protocol::write_frame(cfd, &out).is_err() {
            break;
        }
        frames += 1;
        eprintln!(
            "[c1bridge] frame {} buffer_id {} stored {} B c1_checksum {:016x} producer {:016x} releases {} last_released {}",
            frame_number, buffer_id, stored_bytes, latch_checksum, checksum, release_calls, last_released
        );
    }
    eprintln!(
        "[c1bridge] done: {frames} transactions, {release_calls} onReleaseBuffer callbacks, last released frame {last_released}"
    );
}
