//! `ACOS_PARCEL_DUMP=<path>`: write one reply's wire bytes to disk.
//!
//! The sibling hook `ACOS_PMS_TRACE` records *which* query was answered; this
//! one records *the bytes* of the answer, so a reply that desynchronises the
//! framework's reader can be byte-diffed against the AOSP `writeToParcel` order
//! instead of guessed at (`docs/AOSP_REFERENCE.md`). It is how WV2 found that
//! the Rust `ApplicationInfo` codec was missing two writes: no amount of
//! symmetric ACOS-internal round-tripping shows that class of defect, because
//! both sides of an internal round trip share the same wrong spec.
//!
//! `ACOS_PARCEL_DUMP=<path>` makes every dump go to `<path>.<seq>-<tag>.bin`
//! (raw data payload, no binder header) and appends one line per dump to
//! `<path>.index` (`seq tag len file`). Unset (the default) the hook is inert.
//!
//! The payload is `Parcel::marshall()` truncated to the written data size: the
//! bytes a `Parcel.unmarshall()` on the reader side sees, which is exactly what
//! a byte-diff needs.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use acos_parcel::Parcel;

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The configured dump base path, when the hook is on.
fn base() -> Option<PathBuf> {
    std::env::var_os("ACOS_PARCEL_DUMP").map(PathBuf::from)
}

/// True when `ACOS_PARCEL_DUMP` is set (cheap guard for callers that would
/// otherwise build a scratch parcel per reply).
pub fn enabled() -> bool {
    std::env::var_os("ACOS_PARCEL_DUMP").is_some()
}

/// Dump the written bytes of `parcel`. Returns the file written.
pub fn dump(tag: &str, parcel: &Parcel) -> Option<PathBuf> {
    let end = parcel.data_position();
    let all = parcel.marshall();
    let len = end.min(all.len());
    dump_bytes(tag, &all[..len])
}

/// Dump `bytes` under `tag`. Returns the file written.
pub fn dump_bytes(tag: &str, bytes: &[u8]) -> Option<PathBuf> {
    let base = base()?;
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file = PathBuf::from(format!("{}.{seq:03}-{safe}.bin", base.display()));
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&file, bytes).ok()?;
    if let Ok(mut index) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}.index", base.display()))
    {
        let _ = writeln!(
            index,
            "{seq:03} {tag} len={} {}",
            bytes.len(),
            file.display()
        );
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hook writes exactly the written data bytes and indexes them; unset
    /// it writes nothing.
    #[test]
    fn dump_writes_the_written_data_bytes_and_indexes_them() {
        let dir = std::env::temp_dir().join(format!("acos-parcel-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = dir.join("reply");

        // Unset: inert.
        std::env::remove_var("ACOS_PARCEL_DUMP");
        let mut p = Parcel::new();
        p.write_int32(7);
        assert!(dump("off", &p).is_none(), "unset hook must not write");
        assert!(!dir.exists());

        std::env::set_var("ACOS_PARCEL_DUMP", &base);
        let mut p = Parcel::new();
        p.write_int32(0x1122_3344);
        p.write_string8("x").unwrap();
        let end = p.data_position();
        let file = dump("pms.getPackageInfo.dev.acos.test", &p).expect("dump");
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(bytes.len(), end, "the dump is the written payload");
        assert_eq!(&bytes[..4], &0x1122_3344i32.to_ne_bytes());
        let index = std::fs::read_to_string(format!("{}.index", base.display())).unwrap();
        assert!(
            index.contains("pms.getPackageInfo.dev.acos.test"),
            "{index}"
        );
        std::env::remove_var("ACOS_PARCEL_DUMP");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
