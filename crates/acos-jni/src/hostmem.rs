//! Host-mapping queries used to validate caller-supplied addresses.
//!
//! JNI is the one boundary where native guest code hands ACOS a *host* pointer
//! (`NewDirectByteBuffer(address, capacity)`). The VM never trusts such a
//! pointer for its own memory model, but the copy still dereferences it, so the
//! range is validated against the process's own mapping table first. Anything
//! unparseable fails closed.

/// Whether `[start, end)` is covered by a single readable `/proc/self/maps`
/// mapping.
///
/// The requirement "single mapping" is deliberate: a range that straddles two
/// mappings or extends past the end of one is exactly the fault a hostile
/// `capacity` produces, and it must be rejected rather than read.
///
/// The read is `/proc/self/maps` (no `unsafe`, no libc dependency). On any
/// error — an unreadable `/proc`, an unparsable line — this returns `false`.
pub fn range_is_readable(start: usize, end: usize) -> bool {
    if end <= start {
        return false;
    }
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return false;
    };
    maps.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else {
            return false;
        };
        let Some(perms) = fields.next() else {
            return false;
        };
        let Some((lo, hi)) = range.split_once('-') else {
            return false;
        };
        let (Ok(lo), Ok(hi)) = (
            usize::from_str_radix(lo, 16),
            usize::from_str_radix(hi, 16),
        ) else {
            return false;
        };
        perms.starts_with('r') && start >= lo && end <= hi
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_stack_buffer_is_readable() {
        let buf = [0u8; 32];
        let start = buf.as_ptr() as usize;
        assert!(
            range_is_readable(start, start + buf.len()),
            "the test's own stack buffer must validate"
        );
    }

    #[test]
    fn an_empty_or_inverted_range_is_rejected() {
        let buf = [0u8; 32];
        let start = buf.as_ptr() as usize;
        assert!(!range_is_readable(start, start));
        assert!(!range_is_readable(start + 1, start));
    }

    #[test]
    fn an_unmapped_address_is_rejected() {
        // x86_64 user addresses end well below this; nothing is mapped here.
        let start = 0x7fff_ffff_0000usize;
        assert!(!range_is_readable(start, start + 16));
    }

    #[test]
    fn a_range_past_the_mapping_end_is_rejected() {
        let buf = [0u8; 16];
        let start = buf.as_ptr() as usize;
        // `usize::MAX - 8` is certainly past the mapping holding `buf`.
        assert!(!range_is_readable(start, usize::MAX - 8));
    }
}
