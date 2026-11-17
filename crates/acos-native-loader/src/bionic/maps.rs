//! Host mapping queries for guest-pointer validation.
//!
//! Guest native code hands the bionic shims raw pointers (a `sem_t*`, a stack
//! range for `pthread_attr_*`). ACOS maps guest objects into its own process, so
//! such a pointer is a host pointer, but it is still *guest-supplied*: a hostile
//! or buggy native can pass an unmapped address and the shim would turn a
//! clean `EINVAL` into a SIGSEGV. Before dereferencing, shims therefore ask
//! whether the range is a live mapping of this process.
//!
//! The source is `/proc/self/maps` (no `unsafe`, no libc). A parse failure
//! fails closed: an unreadable map cannot authorise a dereference.

/// One `/proc/self/maps` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub start: usize,
    pub end: usize,
    pub readable: bool,
    pub writable: bool,
}

/// The mapping that contains `addr`, if any.
pub(crate) fn mapping_containing(addr: usize) -> Option<Mapping> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let range = fields.next()?;
        let perms = fields.next()?;
        let (lo, hi) = range.split_once('-')?;
        let start = usize::from_str_radix(lo, 16).ok()?;
        let end = usize::from_str_radix(hi, 16).ok()?;
        if addr < start || addr >= end {
            return None;
        }
        Some(Mapping {
            start,
            end,
            readable: perms.starts_with('r'),
            writable: perms.as_bytes().get(1) == Some(&b'w'),
        })
    })
}

/// The host page size used for alignment checks (x86_64 Linux: 4096).
pub(crate) fn page_size() -> usize {
    4096
}

/// Whether `[addr, addr+len)` lies inside one mapping with the required access.
///
/// A range that straddles two mappings or runs past the end of one is rejected:
/// that is exactly the fault an oversized `len` produces.
pub(crate) fn range_is_mapped(addr: usize, len: usize, need_write: bool) -> bool {
    if addr == 0 || len == 0 {
        return false;
    }
    let Some(end) = addr.checked_add(len) else {
        return false;
    };
    match mapping_containing(addr) {
        Some(m) => m.readable && (!need_write || m.writable) && m.start <= addr && end <= m.end,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_buffer_is_mapped() {
        let mut buf = [0u8; 32];
        let addr = buf.as_mut_ptr() as usize;
        assert!(range_is_mapped(addr, buf.len(), true));
        assert!(range_is_mapped(addr, 1, true));
    }

    #[test]
    fn null_and_empty_ranges_are_not_mapped() {
        assert!(!range_is_mapped(0, 4, true));
        let buf = [0u8; 4];
        assert!(!range_is_mapped(buf.as_ptr() as usize, 0, true));
    }

    #[test]
    fn an_unmapped_address_is_not_mapped() {
        assert!(!range_is_mapped(0x7fff_ffff_0000, 4, false));
    }

    #[test]
    fn a_range_past_the_mapping_end_is_not_mapped() {
        let buf = [0u8; 8];
        let addr = buf.as_ptr() as usize;
        assert!(!range_is_mapped(addr, usize::MAX - addr - 1, false));
        assert!(
            !range_is_mapped(usize::MAX - 1, 8, false),
            "overflow is rejected"
        );
    }

    #[test]
    fn read_only_memory_does_not_satisfy_a_write_requirement() {
        // The crate's own read-only text/rodata is mapped `r--`/`r-x`.
        let text = mapping_containing(static_text_addr());
        if let Some(m) = text {
            if !m.writable {
                assert!(!range_is_mapped(m.start, 4, true));
            }
        }
    }

    /// Address of a read-only static in this crate's image.
    fn static_text_addr() -> usize {
        static RO: u8 = 1;
        std::ptr::addr_of!(RO) as usize
    }
}
