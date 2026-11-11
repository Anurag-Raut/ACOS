//! Per-symbol `LazyTrap` binding.
//!
//! In [`crate::MissingPolicy::LazyTrap`] mode, an import that the bionic table
//! cannot satisfy is bound to a tiny generated x86_64 stub. The stub reports
//! the *exact* symbol and library when (and only when) it is actually called,
//! then aborts with a structured message — it never pretends to work.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Metadata baked into each trap stub.
#[repr(C)]
pub struct TrapRecord {
    pub symbol: String,
    pub library: String,
}

type StubKey = (String, String);

fn stubs() -> &'static Mutex<HashMap<StubKey, usize>> {
    static S: OnceLock<Mutex<HashMap<StubKey, usize>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

extern "C" fn trap_handler(record: *const TrapRecord) -> ! {
    let (symbol, library) = if record.is_null() {
        ("<unknown>".to_string(), "<unknown>".to_string())
    } else {
        // SAFETY: the record is a leaked `Box<TrapRecord>` referenced by the stub.
        let r = unsafe { &*record };
        (r.symbol.clone(), r.library.clone())
    };
    eprintln!(
        "ACOS UNSUPPORTED_NATIVE_SYMBOL: {library} called unimplemented symbol {symbol:?} \
         (loader LazyTrap policy)"
    );
    std::process::abort();
}

/// Code layout of the generated stub:
///
/// ```text
/// 48 BF <imm64>   mov rdi, <record>     ; first argument = TrapRecord*
/// 49 BB <imm64>   mov r11, <handler>
/// 41 FF E3        jmp r11
/// ```
const STUB_LEN: usize = 23;
const _: () = assert!(STUB_LEN == 23);

/// Return (allocating if needed) the address of a trap stub for `symbol` as
/// required by `library`.
pub fn stub_for(symbol: &str, library: &str) -> usize {
    let key = (symbol.to_string(), library.to_string());
    let mut g = stubs().lock().expect("trap registry poisoned");
    if let Some(&addr) = g.get(&key) {
        return addr;
    }
    let page = 4096usize;
    // SAFETY: anonymous RW mapping; we write the stub then flip to RX (W^X).
    let mem = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if mem == libc::MAP_FAILED {
        eprintln!("ACOS: failed to allocate LazyTrap stub for {library}::{symbol}");
        std::process::abort();
    }
    let record = Box::into_raw(Box::new(TrapRecord {
        symbol: symbol.to_string(),
        library: library.to_string(),
    }));
    let code = mem as *mut u8;
    // SAFETY: `page` bytes are writable.
    unsafe {
        *code.add(0) = 0x48;
        *code.add(1) = 0xBF;
        write_u64(code.add(2), record as u64);
        *code.add(10) = 0x49;
        *code.add(11) = 0xBB;
        write_u64(code.add(12), trap_handler as usize as u64);
        *code.add(20) = 0x41;
        *code.add(21) = 0xFF;
        *code.add(22) = 0xE3;
        libc::mprotect(mem, page, libc::PROT_READ | libc::PROT_EXEC);
    }
    let addr = mem as usize;
    g.insert(key, addr);
    addr
}

unsafe fn write_u64(p: *mut u8, v: u64) {
    let bytes = v.to_le_bytes();
    for (i, b) in bytes.iter().enumerate() {
        *p.add(i) = *b;
    }
}

/// Number of distinct trap stubs allocated (for tests/reports).
pub fn stub_count() -> usize {
    stubs().lock().map(|g| g.len()).unwrap_or(0)
}

