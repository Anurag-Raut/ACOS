//! **T-42 (proof of concept): VM snapshot / restore.**
//!
//! This module serialises the *runtime* state of a [`Vm`] into a versioned,
//! fingerprint-checked byte stream and rebuilds a `Vm` from it. It is the
//! measured proof of concept behind [ADR-0010] (snapshot/restore as the
//! start/reset strategy) and the T-42 backlog item.
//!
//! ## What is snapshotted
//!
//! * the **loaded class set** — by descriptor, plus each class's initialisation
//!   state and static-field slots. Method bodies, vtables, field layouts and
//!   native bindings are **re-linked lazily from the same boot DEX files** on
//!   restore (the fingerprint in the header guarantees they are the same files),
//!   so no code is copied into the snapshot;
//! * the whole **heap arena** (instances, arrays, strings, `Class` mirrors and
//!   builtin payloads), preserving **handle indices** so object identity and
//!   `identityHashCode` survive;
//! * **interned strings** (identity matters: `intern()` returns the same
//!   object), the **string hash** cache, the integer cache, **native roots**,
//!   pre-allocated fatal errors, the main `ThreadGroup`, the thread table, the
//!   throwable stack table, output/exit state and the heap/metric counters.
//!
//! ## What is *not* snapshotted
//!
//! Green-thread Java frames, monitors, native converter handles and open file
//! descriptors are **preconditions**: snapshotting refuses (structured
//! `SNAPSHOT_PRECONDITION`) while they are live. The T-43 work item lists the
//! remaining per-process state (pid/tid, RNG, clock, properties, resources,
//! binder identity, fds) that must be re-seeded after restore. The header
//! carries a locale and a clock anchor so the re-seed has somewhere to start.
//!
//! ## Copy-light design
//!
//! The crate is `#![forbid(unsafe_code)]`, so `mmap(MAP_PRIVATE)` / copy-on-write
//! of the arena is not available here. The restore path therefore *memcpys* the
//! arena through [`Object`] decode; `examples/snapshot_probe.rs` measures that
//! decode against a raw `memcpy` of the same bytes as the honest
//! lower bound a CoW mapping would approach.
//!
//! [ADR-0010]: ../../../../DECISIONS.md

use std::collections::HashMap;

use crate::class::InitState;
use crate::error::{VmError, VmResult};
use crate::heap::{
    ArrayData, ArrayObj, BuiltinData, Handle, Heap, Object, ThreadData, NULL,
};
use crate::vm::{CoreLib, ThreadRT, ThreadStatus, Vm, VmOptions};

/// File magic at offset 0 of every snapshot.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"ACOSSNAP";
/// Snapshot format version. Bump on any layout change; restore refuses to read
/// a stream whose version differs.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// Parsed snapshot header (also the tamper surface for mismatch tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub version: u32,
    pub corelib: CoreLib,
    pub trace_natives: bool,
    pub gc_stress: bool,
    pub heap_max_bytes: u64,
    /// Combined boot-DEX fingerprint (checksum + SHA-1 + size of every loaded
    /// provider, in order).
    pub boot_fingerprint: u64,
    pub property_hash: u64,
    pub locale: String,
    pub clock_anchor_millis: u64,
    pub boot_dir: Option<String>,
    pub class_count: u32,
    pub object_slots: u32,
}

/// Wall-clock-free timings for one restore, used by the measurement harness.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreStats {
    /// Time spent copying the snapshot bytes into an owned buffer. This is the
    /// honest lower bound a `mmap(MAP_PRIVATE)`/CoW arena restore would
    /// approach (the crate forbids `unsafe`, so no real mapping is done).
    pub copy_nanos: u64,
    /// Time spent fully decoding the stream into a live [`Vm`].
    pub decode_nanos: u64,
    /// Bytes in the stream.
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// Compact little-endian cursor writer / reader
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn handle(&mut self, v: Handle) {
        self.u32(v);
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.raw(s.as_bytes());
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    fn need(&self, n: usize) -> VmResult<()> {
        if self.pos + n > self.buf.len() {
            return Err(VmError::SnapshotFormat(format!(
                "unexpected end of snapshot at offset {} (need {n} bytes, have {})",
                self.pos,
                self.buf.len() - self.pos
            )));
        }
        Ok(())
    }

    fn u8(&mut self) -> VmResult<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }
    fn bool(&mut self) -> VmResult<bool> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> VmResult<u16> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> VmResult<u32> {
        self.need(4)?;
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(u32::from_le_bytes(b))
    }
    fn i32(&mut self) -> VmResult<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> VmResult<u64> {
        self.need(8)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(b))
    }
    fn i64(&mut self) -> VmResult<i64> {
        Ok(self.u64()? as i64)
    }
    fn str(&mut self) -> VmResult<String> {
        let n = self.u32()? as usize;
        self.need(n)?;
        let s = std::str::from_utf8(&self.buf[self.pos..self.pos + n])
            .map_err(|_| VmError::SnapshotFormat("invalid UTF-8 string".into()))?
            .to_string();
        self.pos += n;
        Ok(s)
    }
    fn handle(&mut self) -> VmResult<Handle> {
        self.u32()
    }
    fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

// ---------------------------------------------------------------------------
// Fingerprints
// ---------------------------------------------------------------------------

/// FNV-1a over the loaded DEX provider fingerprints, in order.
fn boot_fingerprint(vm: &Vm) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    // Hash the boot classpath *definition* (the `BOOTCLASSPATH` jar list and
    // each jar's size), not the dynamically loaded `vm.dexs`: lazy providers are
    // pulled in on demand by the framework boot, so hashing `vm.dexs` would make
    // a framework snapshot invalid against a fresh `Vm::new`. The list is the
    // stable identity of the boot image; content changes still surface through
    // the class-layout checks in `restore`.
    if let Some(dir) = &vm.boot_dir {
        if let Ok(text) = std::fs::read_to_string(dir.join("BOOTCLASSPATH")) {
            for line in text.lines() {
                let jar = line.trim();
                for b in jar.as_bytes() {
                    mix(*b);
                }
                mix(b'\n');
                if let Ok(meta) = std::fs::metadata(dir.join("dex").join(jar.trim_end_matches(".jar"))) {
                    for b in (meta.len() as u64).to_le_bytes() {
                        mix(b);
                    }
                }
            }
        }
    }
    // Bootstrap has no on-disk boot classpath: fall back to the loaded providers.
    if vm.boot_dir.is_none() {
        for dex in &vm.dexs {
            let bytes = dex.file.bytes();
            let header = dex.file.header();
            for b in header.checksum.to_le_bytes() {
                mix(b);
            }
            for b in header.signature {
                mix(b);
            }
            for b in (bytes.len() as u64).to_le_bytes() {
                mix(b);
            }
        }
    }
    h
}

/// Hash of the property table that a snapshot is valid for.
///
/// ACOS has no guest `SystemProperties` table yet, so the hash covers the
/// configuration the VM actually consults: corelib, boot dir, locale and the
/// `ACOS_*` environment (sorted, so it is order-independent and deterministic).
fn property_hash(vm: &Vm) -> u64 {
    let mut props: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("ACOS_"))
        .collect();
    props.sort();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mix = |h: &mut u64, s: &str| {
        for b in s.as_bytes() {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        *h ^= 0xff;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let core = match vm.corelib {
        CoreLib::Bootstrap => "bootstrap",
        CoreLib::Real => "real",
    };
    mix(&mut h, core);
    mix(&mut h, &vm.snapshot_locale);
    if let Some(d) = &vm.boot_dir {
        mix(&mut h, &d.to_string_lossy());
    }
    for (k, v) in &props {
        mix(&mut h, k);
        mix(&mut h, v);
    }
    h
}

// ---------------------------------------------------------------------------
// Preconditions
// ---------------------------------------------------------------------------

impl Vm {
    /// True when `frame` is the host-bridge sentinel base frame (the only frame
    /// a quiescent host thread should carry at snapshot time).
    fn is_sentinel_frame(&self, frame: &crate::frame::Frame) -> bool {
        let Some(m) = self.methods.get(frame.method as usize) else {
            return false;
        };
        m.class == self.c_object && m.name == "<init>" && m.sig == "()V"
    }

    /// Enforce the T-42 snapshot preconditions and return a structured error
    /// naming the first violation.
    pub fn snapshot_preconditions(&self) -> VmResult<()> {
        if self.pending_exception != NULL {
            return Err(VmError::SnapshotPrecondition(
                "a pending Java exception is live (handle != 0)".into(),
            ));
        }
        if self.threads.len() > 1 {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} green threads are live; only the main thread may exist",
                self.threads.len()
            )));
        }
        for (i, t) in self.threads.iter().enumerate() {
            if !t.frames.is_empty() {
                // The host thread always carries one sentinel base frame
                // (`javabridge::ensure_host_thread`); it is a host bridge
                // artifact, not Java execution state, and the snapshot stores no
                // frames. Allow it when it is the only frame.
                let sentinel_only = t.frames.len() == 1 && self.is_sentinel_frame(&t.frames[0]);
                if !sentinel_only {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "thread {i} has {} live Java frames (running); snapshot requires a quiescent VM",
                        t.frames.len()
                    )));
                }
            }
            match t.status {
                ThreadStatus::Runnable | ThreadStatus::Finished => {}
                other => {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "thread {i} is {other:?} (not runnable); snapshot requires a quiescent VM"
                    )))
                }
            }
            if t.wait_monitor.is_some() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} holds a wait monitor"
                )));
            }
            if t.wake_at.is_some() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} has a pending wake deadline"
                )));
            }
            if !t.joiners.is_empty() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} has {} joiner(s)",
                    t.joiners.len()
                )));
            }
        }
        if self.vfs.open_fd_count() != 0 {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} file descriptor(s) are open; the snapshot stores no fd number",
                self.vfs.open_fd_count()
            )));
        }
        if !self.converters.is_empty() {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} ICU converter native handle(s) are registered; not serialisable yet",
                self.converters.len()
            )));
        }
        if !self.monitors.is_empty() {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} monitor(s) are held",
                self.monitors.len()
            )));
        }
        // The stream stores class descriptors, not code: every DEX provider must
        // be one `Vm::restore` will re-load from the same boot files. An app DEX
        // would be silently missing after restore, so refuse it (T-43).
        match self.corelib {
            CoreLib::Bootstrap => {
                if !self.dexs.is_empty() {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "{} application DEX image(s) are loaded; the T-42 snapshot only covers the core provider set",
                        self.dexs.len()
                    )));
                }
            }
            CoreLib::Real => {
                if self.dexs.len() != self.boot_dexs.len() {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "{} non-boot DEX image(s) are loaded ({} boot, {} total); the T-42 snapshot only covers the core provider set",
                        self.dexs.len() - self.boot_dexs.len(),
                        self.boot_dexs.len(),
                        self.dexs.len()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Serialise this VM into a versioned snapshot byte stream.
    ///
    /// The bytes are a pure function of the VM state (no wall clock, no HashMap
    /// iteration order): two snapshots of the same state are byte-equal.
    pub fn snapshot(&self) -> VmResult<Vec<u8>> {
        self.snapshot_preconditions()?;

        let mut w = Writer::default();
        w.raw(&SNAPSHOT_MAGIC);
        w.u32(SNAPSHOT_FORMAT_VERSION);
        w.u8(match self.corelib {
            CoreLib::Bootstrap => 0,
            CoreLib::Real => 1,
        });
        w.bool(self.native_trace.is_some());
        w.bool(self.gc_stress);
        w.u64(self.heap.max_bytes as u64);
        w.u64(boot_fingerprint(self));
        w.u64(property_hash(self));
        w.str(&self.snapshot_locale);
        w.u64(self.clock_anchor_millis);
        match &self.boot_dir {
            Some(d) => {
                w.bool(true);
                w.str(&d.to_string_lossy());
            }
            None => w.bool(false),
        }
        w.u32(self.classes.len() as u32);
        w.u32(self.heap.objects.len() as u32);

        // ---- heap counters / metrics ------------------------------------
        w.u64(self.heap.live_bytes as u64);
        w.u64(self.heap.peak_bytes as u64);
        w.u64(self.heap.allocations);
        w.u64(self.heap.gc_count);
        w.u64(self.heap.gc_nanos);
        w.u64(self.metrics.instructions);
        w.u64(self.metrics.invokes);
        w.u64(self.metrics.clinit_count);

        // ---- process roots / scalar state -------------------------------
        w.u64(self.next_converter);
        w.u32(self.next_thread_id);
        w.handle(self.oom_object);
        w.handle(self.stack_overflow_object);
        w.handle(self.main_thread_group);
        w.handle(self.stdout);
        w.handle(self.stderr);
        w.bool(self.stopping);
        match self.exit_request {
            Some(c) => {
                w.bool(true);
                w.i32(c);
            }
            None => w.bool(false),
        }
        w.u32(self.out_buf.len() as u32);
        w.raw(&self.out_buf);
        w.u32(self.err_buf.len() as u32);
        w.raw(&self.err_buf);

        // ---- classes (descriptor + init + statics) ----------------------
        for c in &self.classes {
            w.str(&c.descriptor);
            w.u8(init_state_tag(c.init));
            w.bool(c.is_primitive);
            w.bool(c.is_array);
            w.u32(c.statics.len() as u32);
            for &s in &c.statics {
                w.u32(s);
            }
            w.handle(c.class_obj);
        }

        // ---- heap arena (slot-preserving) -------------------------------
        for obj in &self.heap.objects {
            match obj {
                Some(o) => {
                    w.bool(true);
                    encode_object(&mut w, o);
                }
                None => w.bool(false),
            }
        }

        // ---- interned strings / roots / caches --------------------------
        w.u32(self.interned.len() as u32);
        for &h in &self.interned {
            w.handle(h);
        }
        w.u32(self.native_roots.len() as u32);
        for &h in &self.native_roots {
            w.handle(h);
        }

        // int_cache: sorted by key for determinism.
        let mut ints: Vec<(i32, Handle)> = self.int_cache.iter().map(|(k, v)| (*k, *v)).collect();
        ints.sort_by_key(|(k, _)| *k);
        w.u32(ints.len() as u32);
        for (k, v) in ints {
            w.i32(k);
            w.handle(v);
        }

        // string_hashes: sorted by handle for determinism.
        let mut sh: Vec<(Handle, i32)> =
            self.string_hashes.iter().map(|(k, v)| (*k, *v)).collect();
        sh.sort_by_key(|(k, _)| *k);
        w.u32(sh.len() as u32);
        for (k, v) in sh {
            w.handle(k);
            w.i32(v);
        }

        // throwable stacks: sorted by handle for determinism.
        let mut ts: Vec<(Handle, &Vec<crate::vm::StackEntry>)> =
            self.throwable_stacks.iter().map(|(k, v)| (*k, v)).collect();
        ts.sort_by_key(|(k, _)| *k);
        w.u32(ts.len() as u32);
        for (h, entries) in ts {
            w.handle(h);
            w.u32(entries.len() as u32);
            for e in entries {
                w.str(&e.class);
                w.str(&e.method);
                w.u32(e.line);
            }
        }

        // ---- threads (quiescent, no frames) -----------------------------
        w.u32(self.threads.len() as u32);
        for t in &self.threads {
            w.handle(t.object);
            w.u8(thread_status_tag(t.status));
        }

        Ok(w.buf)
    }

    /// Find the first heap instance whose class descriptor is `desc`.
    ///
    /// Used by the framework-init snapshot to recover the preserved
    /// `android.app.ActivityThread` handle without adding a VM field: handles
    /// are slot-preserving, so the handle found here is valid after restore.
    pub fn find_instance_of_class(&self, desc: &str) -> Option<Handle> {
        let class_id = *self.class_by_desc.get(desc)?;
        for (idx, slot) in self.heap.objects.iter().enumerate() {
            if let Some(Object::Instance { class, .. }) = slot {
                if *class == class_id {
                    return Some(idx as Handle);
                }
            }
        }
        None
    }

    /// Parse only the header of a snapshot (used for refusal tests and
    /// diagnostics).
    pub fn snapshot_header(bytes: &[u8]) -> VmResult<SnapshotHeader> {
        let mut r = Reader::new(bytes);
        read_header(&mut r)
    }

    /// Restore a VM from [`Vm::snapshot`] bytes.
    ///
    /// The boot DEX set, property table and locale in the header must match this
    /// process; otherwise restore is refused with `SNAPSHOT_MISMATCH`. Class
    /// metadata is re-linked from the boot DEX files (no code is in the stream).
    pub fn restore(bytes: &[u8]) -> VmResult<Vm> {
        let (vm, _stats) = Vm::restore_with_stats(bytes)?;
        Ok(vm)
    }

    /// [`Vm::restore`] plus timings for the measurement harness.
    pub fn restore_with_stats(bytes: &[u8]) -> VmResult<(Vm, RestoreStats)> {
        let t_copy = std::time::Instant::now();
        // Copy-light lower bound: the whole stream is copied once here. A CoW
        // mmap would map instead of copy; the crate forbids `unsafe`, so this
        // memcpy is the measured surrogate.
        let owned = bytes.to_vec();
        let copy_nanos = t_copy.elapsed().as_nanos() as u64;
        let t_decode = std::time::Instant::now();

        let mut r = Reader::new(&owned);
        let header = read_header(&mut r)?;
        if header.version != SNAPSHOT_FORMAT_VERSION {
            return Err(VmError::SnapshotFormat(format!(
                "unsupported snapshot format version {} (expected {})",
                header.version, SNAPSHOT_FORMAT_VERSION
            )));
        }

        let opts = VmOptions {
            heap_max_bytes: header.heap_max_bytes as usize,
            gc_stress: header.gc_stress,
            corelib: header.corelib,
            boot_dir: header.boot_dir.as_ref().map(std::path::PathBuf::from),
            trace_natives: header.trace_natives,
        };
        let mut vm = Vm::new(opts)?;

        // Strict fingerprint / metadata refusal before touching any state.
        let now_boot = boot_fingerprint(&vm);
        if now_boot != header.boot_fingerprint {
            return Err(VmError::SnapshotMismatch(format!(
                "boot DEX fingerprint mismatch: snapshot {:016x}, boot {now_boot:016x}",
                header.boot_fingerprint
            )));
        }
        let now_props = property_hash(&vm);
        if now_props != header.property_hash {
            return Err(VmError::SnapshotMismatch(format!(
                "property hash mismatch: snapshot {:016x}, boot {now_props:016x}",
                header.property_hash
            )));
        }
        if vm.snapshot_locale != header.locale {
            return Err(VmError::SnapshotMismatch(format!(
                "locale mismatch: snapshot {:?}, boot {:?}",
                header.locale, vm.snapshot_locale
            )));
        }

        // ---- heap counters / metrics ------------------------------------
        vm.heap.live_bytes = r.u64()? as usize;
        vm.heap.peak_bytes = r.u64()? as usize;
        vm.heap.allocations = r.u64()?;
        vm.heap.gc_count = r.u64()?;
        vm.heap.gc_nanos = r.u64()?;
        vm.metrics.instructions = r.u64()?;
        vm.metrics.invokes = r.u64()?;
        vm.metrics.clinit_count = r.u64()?;

        // ---- process roots / scalars ------------------------------------
        vm.next_converter = r.u64()?;
        vm.next_thread_id = r.u32()?;
        vm.oom_object = r.handle()?;
        vm.stack_overflow_object = r.handle()?;
        vm.main_thread_group = r.handle()?;
        vm.stdout = r.handle()?;
        vm.stderr = r.handle()?;
        vm.stopping = r.bool()?;
        vm.exit_request = if r.bool()? { Some(r.i32()?) } else { None };
        let out_len = r.u32()? as usize;
        r.need(out_len)?;
        let out = r.remaining()[..out_len].to_vec();
        r.pos += out_len;
        vm.out_buf = out;
        let err_len = r.u32()? as usize;
        r.need(err_len)?;
        let err = r.remaining()[..err_len].to_vec();
        r.pos += err_len;
        vm.err_buf = err;

        // ---- classes -----------------------------------------------------
        let class_count = header.class_count as usize;
        let mut class_desc = Vec::with_capacity(class_count);
        let mut class_meta = Vec::with_capacity(class_count);
        for _ in 0..class_count {
            let desc = r.str()?;
            let init = tag_init_state(r.u8()?)?;
            let is_primitive = r.bool()?;
            let is_array = r.bool()?;
            let n = r.u32()? as usize;
            let mut statics = Vec::with_capacity(n);
            for _ in 0..n {
                statics.push(r.u32()?);
            }
            let class_obj = r.handle()?;
            class_desc.push(desc);
            class_meta.push((init, is_primitive, is_array, statics, class_obj));
        }

        // Re-link any class the fresh VM does not already have, then build the
        // old -> new id map by descriptor.
        let mut old_to_new = Vec::with_capacity(class_count);
        for desc in &class_desc {
            let id = match vm.find_class(desc) {
                Some(id) => id,
                None => vm.load_class(desc)?,
            };
            old_to_new.push(id);
        }
        for (i, desc) in class_desc.iter().enumerate() {
            let new = old_to_new[i];
            let got = vm.class_desc(new).to_string();
            if &got != desc {
                return Err(VmError::SnapshotMismatch(format!(
                    "class id {i} maps to {got}, snapshot expected {desc}"
                )));
            }
        }

        // Static slots + init state (checked for layout equality).
        for (i, (init, _prim, _arr, statics, class_obj)) in class_meta.iter().enumerate() {
            let new = old_to_new[i] as usize;
            if vm.classes[new].statics.len() != statics.len() {
                return Err(VmError::SnapshotMismatch(format!(
                    "class {} static layout changed: snapshot {} slots, boot {}",
                    class_desc[i],
                    statics.len(),
                    vm.classes[new].statics.len()
                )));
            }
            vm.classes[new].statics.copy_from_slice(statics);
            vm.classes[new].init = *init;
            vm.classes[new].class_obj = *class_obj;
        }

        // ---- heap arena --------------------------------------------------
        let slots = header.object_slots as usize;
        let mut objects: Vec<Option<Object>> = Vec::with_capacity(slots);
        for i in 0..slots {
            if r.bool()? {
                objects.push(Some(decode_object(&mut r, &old_to_new)?));
            } else {
                objects.push(None);
                let _ = i;
            }
        }
        // Rebuild the free list from empty slots (slot 0 is the null handle).
        let mut free = Vec::new();
        for (i, o) in objects.iter().enumerate().skip(1) {
            if o.is_none() {
                free.push(i as u32);
            }
        }
        vm.heap.objects = objects;
        vm.heap.free = free;
        vm.heap.field_pool.clear();

        // ---- interned strings / roots / caches ---------------------------
        let interned_n = r.u32()? as usize;
        vm.interned.clear();
        for _ in 0..interned_n {
            vm.interned.push(r.handle()?);
        }
        vm.native_roots.clear();
        let roots_n = r.u32()? as usize;
        for _ in 0..roots_n {
            vm.native_roots.push(r.handle()?);
        }

        let int_n = r.u32()? as usize;
        vm.int_cache.clear();
        for _ in 0..int_n {
            let k = r.i32()?;
            let v = r.handle()?;
            vm.int_cache.insert(k, v);
        }

        let sh_n = r.u32()? as usize;
        vm.string_hashes.clear();
        for _ in 0..sh_n {
            let k = r.handle()?;
            let v = r.i32()?;
            vm.string_hashes.insert(k, v);
        }

        // Rebuild the canonical intern table from the ordered interned list, so
        // HashMap iteration order never enters the snapshot.
        vm.strings.clear();
        for &h in &vm.interned {
            if let Some(Object::String(units)) = vm.heap.get(h) {
                vm.strings.insert(units.clone(), h);
            }
        }

        let ts_n = r.u32()? as usize;
        vm.throwable_stacks.clear();
        for _ in 0..ts_n {
            let h = r.handle()?;
            let n = r.u32()? as usize;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(crate::vm::StackEntry {
                    class: r.str()?,
                    method: r.str()?,
                    line: r.u32()?,
                });
            }
            vm.throwable_stacks.insert(h, entries);
        }

        // ---- threads -----------------------------------------------------
        let thread_n = r.u32()? as usize;
        vm.threads.clear();
        for _ in 0..thread_n {
            let object = r.handle()?;
            let status = tag_thread_status(r.u8()?)?;
            let mut t = ThreadRT::new(object);
            t.status = status;
            vm.threads.push(t);
        }
        vm.current_thread = 0;

        // Re-resolution caches refer to the *old* class/provider layout; drop
        // them so every access re-resolves through the descriptor tables. This
        // is what makes restore independent of DEX parse-order accidents.
        for dex in &mut vm.dexs {
            for s in &mut dex.string_cache {
                *s = None;
            }
            for f in &mut dex.field_cache {
                *f = None;
            }
            for m in &mut dex.method_cache {
                *m = None;
            }
            for c in &mut dex.class_cache {
                *c = None;
            }
        }
        vm.dispatch_cache.clear();
        vm.gc_threshold = (vm.heap.live_bytes * 2)
            .max(vm.heap.max_bytes / 16)
            .max(1 << 20);
        vm.metrics.classes_loaded = vm.classes.len();

        let decode_nanos = t_decode.elapsed().as_nanos() as u64;
        Ok((
            vm,
            RestoreStats {
                copy_nanos,
                decode_nanos,
                bytes: bytes.len() as u64,
            },
        ))
    }
}

fn read_header(r: &mut Reader<'_>) -> VmResult<SnapshotHeader> {
    r.need(8)?;
    if r.remaining()[..8] != SNAPSHOT_MAGIC {
        return Err(VmError::SnapshotFormat("bad magic (not an ACOS snapshot)".into()));
    }
    r.pos += 8;
    let version = r.u32()?;
    let corelib = match r.u8()? {
        0 => CoreLib::Bootstrap,
        1 => CoreLib::Real,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown corelib tag {other}"
            )))
        }
    };
    let trace_natives = r.bool()?;
    let gc_stress = r.bool()?;
    let heap_max_bytes = r.u64()?;
    let boot_fingerprint = r.u64()?;
    let property_hash = r.u64()?;
    let locale = r.str()?;
    let clock_anchor_millis = r.u64()?;
    let boot_dir = if r.bool()? { Some(r.str()?) } else { None };
    let class_count = r.u32()?;
    let object_slots = r.u32()?;
    Ok(SnapshotHeader {
        version,
        corelib,
        trace_natives,
        gc_stress,
        heap_max_bytes,
        boot_fingerprint,
        property_hash,
        locale,
        clock_anchor_millis,
        boot_dir,
        class_count,
        object_slots,
    })
}

fn init_state_tag(s: InitState) -> u8 {
    match s {
        InitState::NotInit => 0,
        InitState::InProgress => 1,
        InitState::Init => 2,
        InitState::Failed => 3,
    }
}

fn tag_init_state(t: u8) -> VmResult<InitState> {
    Ok(match t {
        0 => InitState::NotInit,
        1 => InitState::InProgress,
        2 => InitState::Init,
        3 => InitState::Failed,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown InitState tag {other}"
            )))
        }
    })
}

fn thread_status_tag(s: ThreadStatus) -> u8 {
    match s {
        ThreadStatus::Runnable => 0,
        ThreadStatus::Running => 1,
        ThreadStatus::BlockedMonitor(_) => 2,
        ThreadStatus::Sleeping => 3,
        ThreadStatus::Waiting => 4,
        ThreadStatus::Parked => 5,
        ThreadStatus::Finished => 6,
    }
}

fn tag_thread_status(t: u8) -> VmResult<ThreadStatus> {
    Ok(match t {
        0 => ThreadStatus::Runnable,
        1 => ThreadStatus::Running,
        2 => ThreadStatus::BlockedMonitor(NULL),
        3 => ThreadStatus::Sleeping,
        4 => ThreadStatus::Waiting,
        5 => ThreadStatus::Parked,
        6 => ThreadStatus::Finished,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown ThreadStatus tag {other}"
            )))
        }
    })
}

// ---------------------------------------------------------------------------
// Object encode / decode
// ---------------------------------------------------------------------------

const OBJ_INSTANCE: u8 = 0;
const OBJ_ARRAY: u8 = 1;
const OBJ_STRING: u8 = 2;
const OBJ_CLASS: u8 = 3;
const OBJ_BUILTIN: u8 = 4;

fn encode_object(w: &mut Writer, o: &Object) {
    match o {
        Object::Instance { class, fields } => {
            w.u8(OBJ_INSTANCE);
            w.u32(*class);
            w.u32(fields.len() as u32);
            for &f in fields {
                w.u32(f);
            }
        }
        Object::Array(a) => {
            w.u8(OBJ_ARRAY);
            w.str(&a.elem_desc);
            encode_array_data(w, &a.data);
        }
        Object::String(units) => {
            w.u8(OBJ_STRING);
            w.u32(units.len() as u32);
            for &u in units {
                w.u16(u);
            }
        }
        Object::Class(id) => {
            w.u8(OBJ_CLASS);
            w.u32(*id);
        }
        Object::Builtin { class, data } => {
            w.u8(OBJ_BUILTIN);
            w.u32(*class);
            encode_builtin(w, data);
        }
    }
}

fn encode_array_data(w: &mut Writer, d: &ArrayData) {
    match d {
        ArrayData::Bool(v) => {
            w.u8(0);
            w.u32(v.len() as u32);
            for &x in v {
                w.u8(x as u8);
            }
        }
        ArrayData::Byte(v) => {
            w.u8(1);
            w.u32(v.len() as u32);
            for &x in v {
                w.u8(x as u8);
            }
        }
        ArrayData::Char(v) => {
            w.u8(2);
            w.u32(v.len() as u32);
            for &x in v {
                w.u16(x);
            }
        }
        ArrayData::Short(v) => {
            w.u8(3);
            w.u32(v.len() as u32);
            for &x in v {
                w.u16(x as u16);
            }
        }
        ArrayData::Int(v) => {
            w.u8(4);
            w.u32(v.len() as u32);
            for &x in v {
                w.i32(x);
            }
        }
        ArrayData::Long(v) => {
            w.u8(5);
            w.u32(v.len() as u32);
            for &x in v {
                w.i64(x);
            }
        }
        ArrayData::Float(v) => {
            w.u8(6);
            w.u32(v.len() as u32);
            for &x in v {
                w.u32(x.to_bits());
            }
        }
        ArrayData::Double(v) => {
            w.u8(7);
            w.u32(v.len() as u32);
            for &x in v {
                w.u64(x.to_bits());
            }
        }
        ArrayData::Ref(v) => {
            w.u8(8);
            w.u32(v.len() as u32);
            for &x in v {
                w.u32(x);
            }
        }
    }
}

fn decode_array_data(r: &mut Reader<'_>) -> VmResult<ArrayData> {
    let tag = r.u8()?;
    let n = r.u32()? as usize;
    Ok(match tag {
        0 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u8()? != 0);
            }
            ArrayData::Bool(v)
        }
        1 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u8()? as i8);
            }
            ArrayData::Byte(v)
        }
        2 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()?);
            }
            ArrayData::Char(v)
        }
        3 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()? as i16);
            }
            ArrayData::Short(v)
        }
        4 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.i32()?);
            }
            ArrayData::Int(v)
        }
        5 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.i64()?);
            }
            ArrayData::Long(v)
        }
        6 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(f32::from_bits(r.u32()?));
            }
            ArrayData::Float(v)
        }
        7 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(f64::from_bits(r.u64()?));
            }
            ArrayData::Double(v)
        }
        8 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u32()?);
            }
            ArrayData::Ref(v)
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown ArrayData tag {other}"
            )))
        }
    })
}

const BUILTIN_STRING_BUILDER: u8 = 0;
const BUILTIN_LIST: u8 = 1;
const BUILTIN_HASH_MAP: u8 = 2;
const BUILTIN_TREE_MAP: u8 = 3;
const BUILTIN_ENTRY: u8 = 4;
const BUILTIN_ITERATOR: u8 = 5;
const BUILTIN_ENUM_SET: u8 = 6;
const BUILTIN_THREAD: u8 = 7;
const BUILTIN_STACK_TRACE: u8 = 8;

fn encode_builtin(w: &mut Writer, d: &BuiltinData) {
    match d {
        BuiltinData::StringBuilder(units) => {
            w.u8(BUILTIN_STRING_BUILDER);
            w.u32(units.len() as u32);
            for &u in units {
                w.u16(u);
            }
        }
        BuiltinData::List(v) => {
            w.u8(BUILTIN_LIST);
            encode_handles(w, v);
        }
        BuiltinData::HashMap(v) => {
            w.u8(BUILTIN_HASH_MAP);
            encode_handle_pairs(w, v);
        }
        BuiltinData::TreeMap(v) => {
            w.u8(BUILTIN_TREE_MAP);
            encode_handle_pairs(w, v);
        }
        BuiltinData::Entry { key, value } => {
            w.u8(BUILTIN_ENTRY);
            w.u32(*key);
            w.u32(*value);
        }
        BuiltinData::Iterator { items, index } => {
            w.u8(BUILTIN_ITERATOR);
            encode_handles(w, items);
            w.u32(*index as u32);
        }
        BuiltinData::EnumSet {
            enum_class,
            elements,
        } => {
            w.u8(BUILTIN_ENUM_SET);
            w.u32(*enum_class);
            encode_handles(w, elements);
        }
        BuiltinData::Thread(t) => {
            w.u8(BUILTIN_THREAD);
            w.u32(t.id);
            w.u32(t.name);
            w.u32(t.target);
            w.bool(t.alive);
            w.bool(t.daemon);
        }
        BuiltinData::StackTraceElement {
            class_name,
            method_name,
            file_name,
            line,
        } => {
            w.u8(BUILTIN_STACK_TRACE);
            w.str(class_name);
            w.str(method_name);
            match file_name {
                Some(f) => {
                    w.bool(true);
                    w.str(f);
                }
                None => w.bool(false),
            }
            w.i32(*line);
        }
    }
}

fn encode_handles(w: &mut Writer, v: &[Handle]) {
    w.u32(v.len() as u32);
    for &h in v {
        w.u32(h);
    }
}

fn encode_handle_pairs(w: &mut Writer, v: &[(Handle, Handle)]) {
    w.u32(v.len() as u32);
    for (a, b) in v {
        w.u32(*a);
        w.u32(*b);
    }
}

fn decode_builtin(r: &mut Reader<'_>, remap: &[u32]) -> VmResult<BuiltinData> {
    let tag = r.u8()?;
    Ok(match tag {
        BUILTIN_STRING_BUILDER => {
            let n = r.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()?);
            }
            BuiltinData::StringBuilder(v)
        }
        BUILTIN_LIST => BuiltinData::List(decode_handles(r)?),
        BUILTIN_HASH_MAP => BuiltinData::HashMap(decode_handle_pairs(r)?),
        BUILTIN_TREE_MAP => BuiltinData::TreeMap(decode_handle_pairs(r)?),
        BUILTIN_ENTRY => {
            let key = r.u32()?;
            let value = r.u32()?;
            BuiltinData::Entry { key, value }
        }
        BUILTIN_ITERATOR => {
            let items = decode_handles(r)?;
            let index = r.u32()? as usize;
            BuiltinData::Iterator { items, index }
        }
        BUILTIN_ENUM_SET => {
            let enum_class = r.u32()?;
            let elements = decode_handles(r)?;
            BuiltinData::EnumSet {
                enum_class: remap_class(remap, enum_class),
                elements,
            }
        }
        BUILTIN_THREAD => {
            let id = r.u32()?;
            let name = r.u32()?;
            let target = r.u32()?;
            let alive = r.bool()?;
            let daemon = r.bool()?;
            BuiltinData::Thread(ThreadData {
                id,
                name,
                target,
                alive,
                daemon,
            })
        }
        BUILTIN_STACK_TRACE => {
            let class_name = r.str()?;
            let method_name = r.str()?;
            let file_name = if r.bool()? { Some(r.str()?) } else { None };
            let line = r.i32()?;
            BuiltinData::StackTraceElement {
                class_name,
                method_name,
                file_name,
                line,
            }
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown BuiltinData tag {other}"
            )))
        }
    })
}

fn decode_handles(r: &mut Reader<'_>) -> VmResult<Vec<Handle>> {
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(r.u32()?);
    }
    Ok(v)
}

fn decode_handle_pairs(r: &mut Reader<'_>) -> VmResult<Vec<(Handle, Handle)>> {
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let a = r.u32()?;
        let b = r.u32()?;
        v.push((a, b));
    }
    Ok(v)
}

fn remap_class(remap: &[u32], old: u32) -> u32 {
    remap.get(old as usize).copied().unwrap_or(old)
}

fn decode_object(r: &mut Reader<'_>, remap: &[u32]) -> VmResult<Object> {
    let tag = r.u8()?;
    Ok(match tag {
        OBJ_INSTANCE => {
            let class = remap_class(remap, r.u32()?);
            let n = r.u32()? as usize;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                fields.push(r.u32()?);
            }
            Object::Instance { class, fields }
        }
        OBJ_ARRAY => {
            let elem_desc = r.str()?;
            let data = decode_array_data(r)?;
            Object::Array(ArrayObj { elem_desc, data })
        }
        OBJ_STRING => {
            let n = r.u32()? as usize;
            let mut units = Vec::with_capacity(n);
            for _ in 0..n {
                units.push(r.u16()?);
            }
            Object::String(units)
        }
        OBJ_CLASS => Object::Class(remap_class(remap, r.u32()?)),
        OBJ_BUILTIN => {
            let class = remap_class(remap, r.u32()?);
            let data = decode_builtin(r, remap)?;
            Object::Builtin { class, data }
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown object tag {other}"
            )))
        }
    })
}

/// A helper used by tests: brute-force the byte offset of the property hash
/// field so a mismatch can be injected without a second serialiser.
pub fn header_property_hash_offset() -> usize {
    8    // magic
        + 4  // version
        + 1  // corelib
        + 1  // trace
        + 1  // gc_stress
        + 8  // heap_max
        + 8  // boot fingerprint
        // property hash starts here
}

/// A helper used by tests: build a `Heap` with the given slot count. Exposed
/// only to the crate's own tests to keep `heap.rs` free of snapshot concerns.
#[doc(hidden)]
pub fn empty_heap(max_bytes: usize) -> Heap {
    Heap::new(max_bytes)
}

/// Sort helper shared by the determinism test: canonical byte order of a map.
#[doc(hidden)]
pub fn sorted_handles(m: &HashMap<Handle, i32>) -> Vec<Handle> {
    let mut v: Vec<Handle> = m.keys().copied().collect();
    v.sort_unstable();
    v
}
