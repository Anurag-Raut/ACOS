//! Per-process virtual filesystem for the guest.
//!
//! The guest (real Android bytecode) must never see an arbitrary host path.
//! Every `libcore.io.Linux` / `java.io.UnixFileSystem` file operation is
//! resolved through this module, which maps Android paths onto a small set of
//! sandboxed host locations:
//!
//! | guest path | host location | access |
//! |---|---|---|
//! | `/system/**` (fonts, `etc`, framework) | `<boot>/…` (`.cache/boot/android-34`) | read-only |
//! | `/apex/com.android.tzdata/**` | `<boot>/apex/com.android.tzdata/**` | read-only |
//! | `/apex/com.android.i18n/**` | `<boot>/apex/com.android.i18n/**` | read-only |
//! | `/data/user/<n>/<pkg>/**` | `<root>/data/user/<n>/<pkg>/**` | read/write |
//! | `/data/data/<pkg>/**` | `<root>/data/user/0/<pkg>/**` (the device symlink) | read/write |
//! | `/proc/self/{status,stat,maps}` | synthesised | read-only |
//! | `/dev/null`, `/dev/{u,}random`, `/dev/zero` | host device node | per flags |
//! | anything else | — | `ENOENT` (logged) |
//!
//! `<root>` is the session scratch directory (`ACOS_SESSION_ROOT`, default
//! `.cache/sessions/<id>/data`); it is created on construction and removed on
//! drop.
//!
//! ## Security model
//!
//! Paths are first normalised **lexically** (`.` / `..` / repeated slashes);
//! a `..` that would climb above `/` is rejected (`EACCES`). The resulting
//! guest path is then mapped to a host path under one of the two roots. Before
//! the host path is used, its parent directory (and, when the final component
//! is followed, the full path) is canonicalised and must still live under the
//! canonical root — so a symlink planted inside the sandbox that points at
//! `/etc/passwd` fails closed with `EACCES`. `O_NOFOLLOW` and the
//! non-following operations (`lstat`, `readlink`, `remove`) never resolve the
//! final component in this module and let the kernel reject a symlink.
//!
//! All operations return raw errno values (Linux generic numbering, which is
//! what `android.system.OsConstants` and the host agree on for x86_64); the
//! native layer turns them into `android.system.ErrnoException`.
//!
//! Open files are tracked in an fd table with a per-session open-fd ceiling
//! (`ACOS_MAX_OPEN_FDS`, default 1024) and a written-bytes ceiling
//! (`ACOS_MAX_BYTES_WRITTEN`, default 64 MiB). The table preserves file offsets
//! across `read`/`write`/`lseek`; dropping the `Vfs` closes every descriptor
//! and removes the scratch tree.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `ENOENT`: no such file or directory.
pub const ENOENT: i32 = 2;
/// `EIO`: input/output error.
pub const EIO: i32 = 5;
/// `EBADF`: bad file descriptor.
pub const EBADF: i32 = 9;
/// `EACCES`: permission denied.
pub const EACCES: i32 = 13;
/// `EFAULT`: bad address.
pub const EFAULT: i32 = 14;
/// `EEXIST`: file exists.
pub const EEXIST: i32 = 17;
/// `ENOTDIR`: not a directory.
pub const ENOTDIR: i32 = 20;
/// `EISDIR`: is a directory.
pub const EISDIR: i32 = 21;
/// `EINVAL`: invalid argument.
pub const EINVAL: i32 = 22;
/// `EMFILE`: too many open files.
pub const EMFILE: i32 = 24;
/// `ENOSPC`: no space left on device.
pub const ENOSPC: i32 = 28;
/// `EROFS`: read-only file system.
pub const EROFS: i32 = 30;
/// `ENAMETOOLONG`: file name too long.
pub const ENAMETOOLONG: i32 = 36;
/// `ENOSYS`: function not implemented.
pub const ENOSYS: i32 = 38;
/// `ENOTEMPTY`: directory not empty.
pub const ENOTEMPTY: i32 = 39;
/// `ELOOP`: too many levels of symbolic links.
pub const ELOOP: i32 = 40;
/// `ENODATA`: no data available (a requested xattr is not set).
pub const ENODATA: i32 = 61;
/// `EOPNOTSUPP`: operation not supported (e.g. an xattr namespace the
/// sandbox filesystem does not carry).
pub const EOPNOTSUPP: i32 = 95;
/// `EDQUOT`: disk quota exceeded.
pub const EDQUOT: i32 = 122;

// `O_*` flags as the guest (Linux generic) supplies them.
const O_ACCMODE: i32 = 0o3;
const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 0o1;
const O_RDWR: i32 = 0o2;
const O_CREAT: i32 = 0o100;
const O_EXCL: i32 = 0o200;
const O_TRUNC: i32 = 0o1000;
const O_APPEND: i32 = 0o2000;
const O_NONBLOCK: i32 = 0o4000;
const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;
const O_CLOEXEC: i32 = 0o2000000;

// `SEEK_*` / `F_*` / `access(2)` / `stat` mode bits.
const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const F_OK: i32 = 0;
const R_OK: i32 = 4;
const W_OK: i32 = 2;
const X_OK: i32 = 1;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFSOCK: u32 = 0o140000;

/// Session directory and limits for a [`Vfs`].
#[derive(Clone, Debug)]
pub struct VfsConfig {
    /// Writable scratch root (the sandbox's `/data`).
    pub root: PathBuf,
    /// Read-only boot cache root (the sandbox's `/system`).
    pub boot_root: PathBuf,
    /// Process id reported by the synthesised `/proc/self/*` files.
    pub pid: u32,
    /// App uid reported by the synthesised files and `stat` results.
    pub uid: u32,
    /// Maximum simultaneously open descriptors (`ACOS_MAX_OPEN_FDS`).
    pub max_open_fds: usize,
    /// Maximum bytes the session may write (`ACOS_MAX_BYTES_WRITTEN`; 0 = off).
    pub max_bytes_written: u64,
    /// Remove `root` when the `Vfs` is dropped.
    pub cleanup: bool,
}

impl Default for VfsConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl VfsConfig {
    /// Build the configuration from the environment.
    ///
    /// `ACOS_SESSION_ROOT` pins the scratch root (and disables cleanup);
    /// otherwise a unique `.cache/sessions/<id>/data` is used.
    pub fn from_env() -> VfsConfig {
        let boot_root = crate::vm::default_boot_dir(None)
            .unwrap_or_else(|| PathBuf::from(".cache/boot/android-34"));
        let explicit_root = std::env::var_os("ACOS_SESSION_ROOT").map(PathBuf::from);
        let root = explicit_root
            .clone()
            .unwrap_or_else(|| default_sessions_dir().join(next_session_id()).join("data"));
        VfsConfig {
            root,
            boot_root,
            pid: std::process::id(),
            uid: 10192,
            max_open_fds: env_usize("ACOS_MAX_OPEN_FDS", 1024),
            max_bytes_written: env_u64("ACOS_MAX_BYTES_WRITTEN", 64 * 1024 * 1024),
            cleanup: explicit_root.is_none() && std::env::var_os("ACOS_VFS_KEEP").is_none(),
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn default_sessions_dir() -> PathBuf {
    if let Some(boot) = crate::vm::default_boot_dir(None) {
        // <repo>/.cache/boot/android-34 -> <repo>/.cache/sessions
        if let Some(cache) = boot.parent().and_then(Path::parent) {
            return cache.join("sessions");
        }
    }
    PathBuf::from(".cache/sessions")
}

fn next_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{:x}-{n:x}", std::process::id(), nanos)
}

/// A `stat(2)` result in the fields `android.system.StructStat` needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u64,
    pub blocks: u64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// A `statvfs(2)` result for `android.system.StructStatVfs`.
#[derive(Clone, Copy, Debug)]
pub struct StatVfs {
    pub bsize: u64,
    pub frsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub favail: u64,
    pub fsid: u64,
    pub flag: u64,
    pub namemax: u64,
}

/// The backing store of an open descriptor.
#[derive(Debug)]
enum Source {
    /// A real host file owned by the descriptor table.
    File(File),
    /// Synthesised read-only content (`/proc`, denied-write devices).
    Synthetic { data: Vec<u8>, pos: u64, mode: u32 },
}

/// One entry in the descriptor table.
#[derive(Debug)]
struct OpenFile {
    source: Source,
    /// The guest path this descriptor was opened as (diagnostics).
    #[allow(dead_code)]
    guest_path: String,
    flags: i32,
    readable: bool,
    writable: bool,
    append: bool,
}

/// The per-process virtual filesystem.
#[derive(Debug)]
pub struct Vfs {
    root: PathBuf,
    root_canon: PathBuf,
    boot_root: PathBuf,
    boot_canon: PathBuf,
    pid: u32,
    uid: u32,
    max_open_fds: usize,
    max_bytes_written: u64,
    bytes_written: u64,
    next_fd: i32,
    fd_table: BTreeMap<i32, OpenFile>,
    cleanup: bool,
    /// A construction failure (missing/unwritable root): every operation fails
    /// closed with this errno.
    broken: Option<i32>,
    /// Structured denial log: `guest_path\tERRNO_NAME` for unmapped paths.
    denials: Vec<String>,
    /// Guest-set extended attributes, keyed by `(guest path, name)`.
    ///
    /// The sandbox models a filesystem that supports the `user.*` namespace,
    /// exactly like the device data partition: `ContextImpl.getCacheDir` writes
    /// `user.inode_cache` on the app data dir. Values are session-scoped and
    /// dropped when the node is removed or renamed. ACOS has no
    /// security/capability xattr layer, so other namespaces report
    /// `EOPNOTSUPP` rather than inventing a label.
    xattrs: BTreeMap<(String, String), Vec<u8>>,
}

impl Vfs {
    /// Create the sandbox, making the scratch root.
    pub fn new(config: VfsConfig) -> Vfs {
        let mut broken = None;
        if let Err(e) = std::fs::create_dir_all(config.root.join("data/user/0")) {
            broken = Some(errno_of(e));
        }
        // The device-protected storage tree `/data/user_de/0` exists on every
        // device; a device-protected `Context` resolves its data dir here.
        if let Err(e) = std::fs::create_dir_all(config.root.join("data/user_de/0")) {
            broken.get_or_insert(errno_of(e));
        }
        let root_canon =
            std::fs::canonicalize(&config.root).unwrap_or_else(|_| config.root.clone());
        let boot_canon =
            std::fs::canonicalize(&config.boot_root).unwrap_or_else(|_| config.boot_root.clone());
        Vfs {
            root: config.root,
            root_canon,
            boot_root: config.boot_root,
            boot_canon,
            pid: config.pid,
            uid: config.uid,
            max_open_fds: config.max_open_fds.max(1),
            max_bytes_written: config.max_bytes_written,
            bytes_written: 0,
            next_fd: 3,
            fd_table: BTreeMap::new(),
            cleanup: config.cleanup,
            broken,
            denials: Vec::new(),
            xattrs: BTreeMap::new(),
        }
    }

    /// Point the read-only mapping at a resolved boot directory.
    pub fn set_boot_root(&mut self, dir: &Path) {
        self.boot_root = dir.to_path_buf();
        self.boot_canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    }

    /// Override the writable scratch root. Existing open files are unaffected.
    pub fn set_root(&mut self, dir: &Path) {
        if std::fs::create_dir_all(dir.join("data/user/0")).is_ok()
            && std::fs::create_dir_all(dir.join("data/user_de/0")).is_ok()
        {
            self.root = dir.to_path_buf();
            self.root_canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            self.broken = None;
        } else {
            self.broken = Some(EACCES);
        }
    }

    /// Enable/disable cleanup of the scratch root on drop.
    pub fn set_cleanup(&mut self, cleanup: bool) {
        self.cleanup = cleanup;
    }

    /// The writable scratch root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The read-only boot cache root.
    #[must_use]
    pub fn boot_root(&self) -> &Path {
        &self.boot_root
    }

    /// Bytes written through this session so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// How many descriptors are currently open.
    #[must_use]
    pub fn open_fd_count(&self) -> usize {
        self.fd_table.len()
    }

    /// Structured `guest\tERRNO` denials, newest last.
    #[must_use]
    pub fn denials(&self) -> &[String] {
        &self.denials
    }

    /// Force a closed sandbox (used by tests and by a failed construction).
    pub fn poison(&mut self, errno: i32) {
        self.broken = Some(errno);
    }

    fn check_broken(&self) -> Result<(), i32> {
        match self.broken {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn deny(&mut self, guest: &str, errno: i32) -> i32 {
        self.denials.push(format!("{guest}\t{}", errno_name(errno)));
        errno
    }

    // -- path mapping ------------------------------------------------------

    /// Map a normalised guest path to its sandbox target.
    fn target(&self, guest: &str) -> Target {
        if let Some(rest) = guest.strip_prefix("/system") {
            let rel = rest.trim_start_matches('/');
            let host = match rel {
                "etc/fonts.xml" | "etc/system_fonts.xml" => {
                    self.boot_root.join("fonts").join("fonts.xml")
                }
                "etc/preloaded-classes" => self.boot_root.join("preloaded-classes"),
                "framework/framework-res.apk" => self.boot_root.join("framework-res.apk"),
                other => self.boot_root.join(other),
            };
            return Target::ReadOnly(host);
        }
        if let Some(rest) = guest.strip_prefix("/apex/com.android.tzdata") {
            let rel = rest.trim_start_matches('/');
            return Target::ReadOnly(
                self.boot_root
                    .join("apex")
                    .join("com.android.tzdata")
                    .join(rel),
            );
        }
        // `AndroidDataFiles.generateIcuDataPath()` reads `ANDROID_I18N_ROOT`
        // (seeded as `/apex/com.android.i18n`, the real device path) and then
        // opens `<root>/etc/icu/icudt72l.dat`. Without this mapping the ICU
        // resource bundles (`likelySubtags`) raise `MissingResourceException`
        // from `Typeface.<clinit>`.
        if let Some(rest) = guest.strip_prefix("/apex/com.android.i18n") {
            let rel = rest.trim_start_matches('/');
            return Target::ReadOnly(
                self.boot_root
                    .join("apex")
                    .join("com.android.i18n")
                    .join(rel),
            );
        }
        if let Some(rest) = guest.strip_prefix("/data/user_de/") {
            return Target::Writable(self.root.join("data").join("user_de").join(rest));
        }
        if let Some(rest) = guest.strip_prefix("/data/user/") {
            return Target::Writable(self.root.join("data").join("user").join(rest));
        }
        match guest {
            // The intermediate directories `File.mkdirs()` walks before it
            // reaches `<pkg>/files`.
            "/data" => return Target::Writable(self.root.join("data")),
            "/data/user" => return Target::Writable(self.root.join("data").join("user")),
            "/data/user_de" => return Target::Writable(self.root.join("data").join("user_de")),
            "/data/data" => return Target::Writable(self.root.join("data").join("user").join("0")),
            _ => {}
        }
        if let Some(rest) = guest.strip_prefix("/data/data/") {
            return Target::Writable(self.root.join("data").join("user").join("0").join(rest));
        }
        match guest {
            "/dev/null" | "/dev/zero" => return Target::Host(PathBuf::from(guest)),
            "/dev/urandom" | "/dev/random" => {
                return Target::Host(PathBuf::from("/dev/urandom"));
            }
            _ => {}
        }
        if let Some(rest) = guest.strip_prefix("/proc/") {
            let self_prefix = format!("{}/", self.pid);
            let tail = rest
                .strip_prefix("self/")
                .or_else(|| rest.strip_prefix(self_prefix.as_str()));
            if let Some(tail) = tail {
                return match tail {
                    "status" => Target::Synthetic(Synthetic::Status),
                    "stat" => Target::Synthetic(Synthetic::Stat),
                    "maps" => Target::Synthetic(Synthetic::Maps),
                    _ => Target::Denied(ENOENT),
                };
            }
            return Target::Denied(ENOENT);
        }
        Target::Denied(ENOENT)
    }

    /// Resolve a host path under `canon_root`, refusing symlink escapes.
    fn safe_path(
        &self,
        host: &Path,
        canon_root: &Path,
        follow_final: bool,
    ) -> Result<PathBuf, i32> {
        let parent = host.parent().ok_or(EACCES)?;
        let name = host.file_name().ok_or(EACCES)?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(errno_of)?;
        if !canonical_parent.starts_with(canon_root) {
            return Err(EACCES);
        }
        let joined = canonical_parent.join(name);
        if follow_final {
            match std::fs::canonicalize(&joined) {
                Ok(canon) => {
                    if !canon.starts_with(canon_root) {
                        return Err(EACCES);
                    }
                    Ok(canon)
                }
                Err(_) => {
                    // May not exist yet (create); refuse a dangling symlink.
                    if let Ok(md) = std::fs::symlink_metadata(&joined) {
                        if md.file_type().is_symlink() {
                            return Err(EACCES);
                        }
                    }
                    Ok(joined)
                }
            }
        } else {
            Ok(joined)
        }
    }

    // -- descriptor table --------------------------------------------------

    fn alloc_fd(&mut self, of: OpenFile) -> Result<i32, i32> {
        use std::collections::btree_map::Entry;
        if self.fd_table.len() >= self.max_open_fds {
            return Err(EMFILE);
        }
        let mut candidate = self.next_fd.max(3);
        loop {
            match self.fd_table.entry(candidate) {
                Entry::Vacant(slot) => {
                    slot.insert(of);
                    self.next_fd = candidate.wrapping_add(1).max(3);
                    return Ok(candidate);
                }
                Entry::Occupied(_) => candidate = candidate.wrapping_add(1).max(3),
            }
        }
    }

    fn get(&self, fd: i32) -> Result<&OpenFile, i32> {
        self.fd_table.get(&fd).ok_or(EBADF)
    }

    fn get_mut(&mut self, fd: i32) -> Result<&mut OpenFile, i32> {
        self.fd_table.get_mut(&fd).ok_or(EBADF)
    }

    // -- operations --------------------------------------------------------

    /// `open(path, flags, mode)`.
    pub fn open(&mut self, guest: &str, flags: i32, mode: i32) -> Result<i32, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let accmode = flags & O_ACCMODE;
        let writable = accmode != O_RDONLY;
        match self.target(&norm) {
            Target::Denied(e) => Err(self.deny(&norm, e)),
            Target::Synthetic(kind) => {
                if writable || flags & (O_CREAT | O_TRUNC) != 0 {
                    return Err(self.deny(&norm, EACCES));
                }
                let (data, mode) = self.synthetic_content(kind);
                self.alloc_fd(OpenFile {
                    source: Source::Synthetic { data, pos: 0, mode },
                    guest_path: norm,
                    flags,
                    readable: true,
                    writable: false,
                    append: false,
                })
            }
            Target::Host(host) => {
                if flags & O_DIRECTORY != 0 {
                    return Err(self.deny(&norm, ENOTDIR));
                }
                let file = open_host(&host, flags, mode).map_err(|e| self.deny(&norm, e))?;
                self.alloc_fd(OpenFile {
                    source: Source::File(file),
                    guest_path: norm,
                    flags,
                    readable: accmode != O_WRONLY,
                    writable,
                    append: flags & O_APPEND != 0,
                })
            }
            Target::ReadOnly(host) => {
                if writable || flags & (O_CREAT | O_TRUNC) != 0 {
                    return Err(self.deny(&norm, EROFS));
                }
                let follow = flags & O_NOFOLLOW == 0;
                let host = self
                    .safe_path(&host, &self.boot_canon, follow)
                    .map_err(|e| self.deny(&norm, e))?;
                let file = open_host(&host, flags, mode).map_err(|e| self.deny(&norm, e))?;
                self.alloc_fd(OpenFile {
                    source: Source::File(file),
                    guest_path: norm,
                    flags,
                    readable: true,
                    writable: false,
                    append: false,
                })
            }
            Target::Writable(host) => {
                let follow = flags & O_NOFOLLOW == 0;
                let host = self
                    .safe_path(&host, &self.root_canon, follow)
                    .map_err(|e| self.deny(&norm, e))?;
                let file = open_host(&host, flags, mode).map_err(|e| self.deny(&norm, e))?;
                self.alloc_fd(OpenFile {
                    source: Source::File(file),
                    guest_path: norm,
                    flags,
                    readable: accmode != O_WRONLY,
                    writable,
                    append: flags & O_APPEND != 0,
                })
            }
        }
    }

    /// `close(fd)`.
    pub fn close(&mut self, fd: i32) -> Result<(), i32> {
        match self.fd_table.remove(&fd) {
            Some(_) => Ok(()),
            None => Err(EBADF),
        }
    }

    pub fn read(&mut self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
        let of = self.get_mut(fd)?;
        if !of.readable {
            return Err(EBADF);
        }
        match &mut of.source {
            Source::File(f) => f.read(buf).map_err(errno_of),
            Source::Synthetic { data, pos, .. } => {
                let start = (*pos as usize).min(data.len());
                let n = buf.len().min(data.len() - start);
                buf[..n].copy_from_slice(&data[start..start + n]);
                *pos += n as u64;
                Ok(n)
            }
        }
    }

    pub fn pread(&mut self, fd: i32, buf: &mut [u8], off: u64) -> Result<usize, i32> {
        let of = self.get(fd)?;
        if !of.readable {
            return Err(EBADF);
        }
        match &of.source {
            Source::File(f) => f.read_at(buf, off).map_err(errno_of),
            Source::Synthetic { data, .. } => {
                let start = (off as usize).min(data.len());
                let n = buf.len().min(data.len() - start);
                buf[..n].copy_from_slice(&data[start..start + n]);
                Ok(n)
            }
        }
    }

    /// Whether a descriptor's writes count against the session byte quota.
    ///
    /// Only files under the writable scratch root count; `/dev/null`, pipes and
    /// synthetic files must never trip `EDQUOT`.
    fn counts_quota(&self, fd: i32) -> bool {
        self.fd_table.get(&fd).is_some_and(|of| {
            matches!(of.source, Source::File(_)) && of.guest_path.starts_with("/data/")
        })
    }

    pub fn write(&mut self, fd: i32, buf: &[u8]) -> Result<usize, i32> {
        let counts = self.counts_quota(fd);
        if counts {
            self.check_quota(buf.len())?;
        }
        let of = self.get_mut(fd)?;
        if !of.writable {
            return Err(EBADF);
        }
        let append = of.append;
        let n = match &mut of.source {
            Source::File(f) => {
                if append {
                    let _ = f.seek(SeekFrom::End(0));
                }
                f.write(buf).map_err(errno_of)?
            }
            Source::Synthetic { .. } => return Err(EACCES),
        };
        if counts {
            self.bytes_written = self.bytes_written.saturating_add(n as u64);
        }
        Ok(n)
    }

    pub fn pwrite(&mut self, fd: i32, buf: &[u8], off: u64) -> Result<usize, i32> {
        let counts = self.counts_quota(fd);
        if counts {
            self.check_quota(buf.len())?;
        }
        let of = self.get(fd)?;
        if !of.writable {
            return Err(EBADF);
        }
        let n = match &of.source {
            Source::File(f) => f.write_at(buf, off).map_err(errno_of)?,
            Source::Synthetic { .. } => return Err(EACCES),
        };
        if counts {
            self.bytes_written = self.bytes_written.saturating_add(n as u64);
        }
        Ok(n)
    }

    fn check_quota(&self, n: usize) -> Result<(), i32> {
        if self.max_bytes_written != 0
            && self.bytes_written.saturating_add(n as u64) > self.max_bytes_written
        {
            return Err(EDQUOT);
        }
        Ok(())
    }

    pub fn lseek(&mut self, fd: i32, offset: i64, whence: i32) -> Result<i64, i32> {
        let of = self.get_mut(fd)?;
        match &mut of.source {
            Source::File(f) => {
                let pos = match whence {
                    SEEK_SET => SeekFrom::Start(offset.max(0) as u64),
                    SEEK_CUR => SeekFrom::Current(offset),
                    SEEK_END => SeekFrom::End(offset),
                    _ => return Err(EINVAL),
                };
                f.seek(pos).map(|p| p as i64).map_err(errno_of)
            }
            Source::Synthetic { data, pos, .. } => {
                let new = match whence {
                    SEEK_SET => offset,
                    SEEK_CUR => *pos as i64 + offset,
                    SEEK_END => data.len() as i64 + offset,
                    _ => return Err(EINVAL),
                };
                if new < 0 {
                    return Err(EINVAL);
                }
                *pos = new as u64;
                Ok(new)
            }
        }
    }

    pub fn fstat(&self, fd: i32) -> Result<Stat, i32> {
        let of = self.get(fd)?;
        match &of.source {
            Source::File(f) => f.metadata().map(stat_of).map_err(errno_of),
            Source::Synthetic { data, mode, .. } => Ok(Stat {
                mode: mode | S_IFREG,
                nlink: 1,
                uid: self.uid,
                gid: self.uid,
                size: data.len() as u64,
                blksize: 4096,
                blocks: data.len().div_ceil(512) as u64,
                ..Stat::default()
            }),
        }
    }

    pub fn stat(&self, guest: &str, follow: bool) -> Result<Stat, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let host = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::Synthetic(kind) => return Ok(self.synthetic_stat(kind)),
            Target::Host(h) => h,
            Target::ReadOnly(h) => self.safe_path(&h, &self.boot_canon, follow)?,
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, follow)?,
        };
        let md = if follow {
            std::fs::metadata(&host)
        } else {
            std::fs::symlink_metadata(&host)
        }
        .map_err(errno_of)?;
        Ok(stat_of(md))
    }

    /// Whether a descriptor refers to a socket (`S_IFSOCK`).
    #[must_use]
    pub fn is_socket(&self, fd: i32) -> bool {
        self.fstat(fd)
            .map(|st| st.mode & S_IFMT == S_IFSOCK)
            .unwrap_or(false)
    }

    /// `getsockopt(2)` for the options the sandbox can answer truthfully.
    /// Every ACOS socket is `AF_UNIX`, so `SOL_SOCKET/SO_DOMAIN` is `AF_UNIX`;
    /// anything else is `ENOPROTOOPT` rather than a faked host value.
    pub fn getsockopt_int(&self, fd: i32, level: i32, optname: i32) -> Result<i32, i32> {
        const SOL_SOCKET: i32 = 1;
        const SO_DOMAIN: i32 = 39;
        const ENOTSOCK: i32 = 88;
        const ENOPROTOOPT: i32 = 92;
        let of = self.get(fd)?;
        if !matches!(of.source, Source::File(_)) {
            return Err(ENOTSOCK);
        }
        if level == SOL_SOCKET && optname == SO_DOMAIN {
            return Ok(1); // AF_UNIX
        }
        Err(ENOPROTOOPT)
    }

    /// `access(2)`: `Ok(())` when allowed, `Err(errno)` otherwise.
    pub fn access(&self, guest: &str, mode: i32) -> Result<(), i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let (host, read_only) = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::Synthetic(_) | Target::Host(_) => return Ok(()),
            Target::ReadOnly(h) => (self.safe_path(&h, &self.boot_canon, true)?, true),
            Target::Writable(h) => (self.safe_path(&h, &self.root_canon, true)?, false),
        };
        let md = std::fs::metadata(&host).map_err(errno_of)?;
        if mode == F_OK {
            return Ok(());
        }
        if mode & W_OK != 0 && (read_only || md.permissions().readonly()) {
            return Err(EACCES);
        }
        if mode & R_OK != 0 && !mode_permits_read(&md) {
            return Err(EACCES);
        }
        if mode & X_OK != 0 {
            let is_dir = md.is_dir();
            let exec = md.permissions().mode() & 0o111 != 0;
            if !is_dir && !exec {
                return Err(EACCES);
            }
        }
        Ok(())
    }

    /// Bytes readable without blocking (`FIONREAD` equivalent).
    pub fn available(&self, fd: i32) -> Result<i32, i32> {
        let of = self.get(fd)?;
        match &of.source {
            Source::File(f) => {
                let md = f.metadata().map_err(errno_of)?;
                if !md.is_file() {
                    return Ok(0);
                }
                // `try_clone` dups the descriptor, sharing the file offset.
                let mut probe = f.try_clone().map_err(errno_of)?;
                let pos = probe.stream_position().map_err(errno_of)?;
                Ok(md.len().saturating_sub(pos).min(i32::MAX as u64) as i32)
            }
            Source::Synthetic { data, pos, .. } => Ok(data
                .len()
                .saturating_sub(*pos as usize)
                .min(i32::MAX as usize)
                as i32),
        }
    }

    pub fn dup(&mut self, fd: i32) -> Result<i32, i32> {
        let of = self.get(fd)?;
        let cloned = clone_open_file(of)?;
        self.alloc_fd(cloned)
    }

    pub fn dup2(&mut self, fd: i32, newfd: i32) -> Result<i32, i32> {
        if newfd < 0 {
            return Err(EBADF);
        }
        let of = self.get(fd)?;
        let cloned = clone_open_file(of)?;
        self.fd_table.remove(&newfd);
        self.fd_table.insert(newfd, cloned);
        Ok(newfd)
    }

    pub fn fcntl(&mut self, fd: i32, cmd: i32, arg: i32) -> Result<i32, i32> {
        let of = self.get_mut(fd)?;
        match cmd {
            F_GETFL => {
                let mut flags = of.flags & O_ACCMODE;
                if of.append {
                    flags |= O_APPEND;
                }
                if of.flags & O_NONBLOCK != 0 {
                    flags |= O_NONBLOCK;
                }
                Ok(flags)
            }
            F_SETFL => {
                of.append = arg & O_APPEND != 0;
                of.flags = (of.flags & !(O_APPEND | O_NONBLOCK)) | (arg & (O_APPEND | O_NONBLOCK));
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }

    pub fn ftruncate(&mut self, fd: i32, len: u64) -> Result<(), i32> {
        let of = self.get_mut(fd)?;
        if !of.writable {
            return Err(EINVAL);
        }
        match &of.source {
            Source::File(f) => f.set_len(len).map_err(errno_of),
            Source::Synthetic { .. } => Err(EACCES),
        }
    }

    pub fn fsync(&mut self, fd: i32, data_only: bool) -> Result<(), i32> {
        let of = self.get(fd)?;
        match &of.source {
            Source::File(f) => {
                if data_only {
                    f.sync_data().map_err(errno_of)
                } else {
                    f.sync_all().map_err(errno_of)
                }
            }
            Source::Synthetic { .. } => Ok(()),
        }
    }

    pub fn chmod(&mut self, guest: &str, mode: i32, fd: Option<i32>) -> Result<(), i32> {
        if let Some(fd) = fd {
            let of = self.get(fd)?;
            match &of.source {
                Source::File(f) => {
                    let perms = std::fs::Permissions::from_mode(mode as u32 & 0o7777);
                    f.set_permissions(perms).map_err(errno_of)
                }
                Source::Synthetic { .. } => Err(EACCES),
            }
        } else {
            self.check_broken()?;
            let norm = normalize_guest(guest).ok_or(EACCES)?;
            match self.target(&norm) {
                Target::Denied(e) => Err(e),
                Target::ReadOnly(_) => Err(EROFS),
                Target::Synthetic(_) => Err(EACCES),
                Target::Host(_) => Err(EACCES),
                Target::Writable(h) => {
                    let h = self.safe_path(&h, &self.root_canon, true)?;
                    std::fs::set_permissions(
                        &h,
                        std::fs::Permissions::from_mode(mode as u32 & 0o7777),
                    )
                    .map_err(errno_of)
                }
            }
        }
    }

    pub fn mkdir(&mut self, guest: &str, mode: i32) -> Result<(), i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        match self.target(&norm) {
            Target::Denied(e) => Err(e),
            Target::ReadOnly(_) | Target::Synthetic(_) | Target::Host(_) => Err(EROFS),
            Target::Writable(h) => {
                let h = self.safe_path(&h, &self.root_canon, false)?;
                if h.exists() {
                    return Err(EEXIST);
                }
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(mode as u32 & 0o7777);
                builder.create(&h).map_err(errno_of)
            }
        }
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), i32> {
        self.check_broken()?;
        let from_norm = normalize_guest(from).ok_or(EACCES)?;
        let to_norm = normalize_guest(to).ok_or(EACCES)?;
        let from_host = match self.target(&from_norm) {
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, false)?,
            Target::Denied(e) => return Err(e),
            _ => return Err(EROFS),
        };
        let to_host = match self.target(&to_norm) {
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, false)?,
            Target::Denied(e) => return Err(e),
            _ => return Err(EROFS),
        };
        std::fs::rename(&from_host, &to_host).map_err(errno_of)?;
        let from_key = Self::xattr_guest_key(&from_norm);
        let to_key = Self::xattr_guest_key(&to_norm);
        self.move_xattrs(&from_key, &to_key);
        Ok(())
    }

    pub fn remove(&mut self, guest: &str) -> Result<(), i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        match self.target(&norm) {
            Target::Denied(e) => Err(e),
            Target::ReadOnly(_) | Target::Synthetic(_) | Target::Host(_) => Err(EROFS),
            Target::Writable(h) => {
                let h = self.safe_path(&h, &self.root_canon, false)?;
                let md = std::fs::symlink_metadata(&h).map_err(errno_of)?;
                if md.is_dir() {
                    std::fs::remove_dir(&h).map_err(errno_of)?;
                } else {
                    std::fs::remove_file(&h).map_err(errno_of)?;
                }
                let key = Self::xattr_guest_key(&norm);
                self.drop_xattrs_under(&key);
                Ok(())
            }
        }
    }

    pub fn readlink(&self, guest: &str) -> Result<String, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let host = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::ReadOnly(h) => self.safe_path(&h, &self.boot_canon, false)?,
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, false)?,
            Target::Host(_) | Target::Synthetic(_) => return Err(EINVAL),
        };
        let target = std::fs::read_link(&host).map_err(errno_of)?;
        Ok(target.to_string_lossy().into_owned())
    }

    pub fn symlink(&mut self, target: &str, link: &str) -> Result<(), i32> {
        self.check_broken()?;
        let norm = normalize_guest(link).ok_or(EACCES)?;
        match self.target(&norm) {
            Target::Writable(h) => {
                let h = self.safe_path(&h, &self.root_canon, false)?;
                std::os::unix::fs::symlink(target, h).map_err(errno_of)
            }
            Target::Denied(e) => Err(e),
            _ => Err(EROFS),
        }
    }

    pub fn link(&mut self, old: &str, new: &str) -> Result<(), i32> {
        self.check_broken()?;
        let old_norm = normalize_guest(old).ok_or(EACCES)?;
        let new_norm = normalize_guest(new).ok_or(EACCES)?;
        let old_host = match self.target(&old_norm) {
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, true)?,
            Target::Denied(e) => return Err(e),
            _ => return Err(EROFS),
        };
        let new_host = match self.target(&new_norm) {
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, false)?,
            Target::Denied(e) => return Err(e),
            _ => return Err(EROFS),
        };
        std::fs::hard_link(&old_host, &new_host).map_err(errno_of)
    }

    // -- extended attributes ----------------------------------------------

    /// Map `/data/data/<pkg>` (the device symlink) onto `/data/user/0/<pkg>`
    /// so both spellings address the same inode and share its xattrs.
    fn xattr_guest_key(norm: &str) -> String {
        if let Some(rest) = norm.strip_prefix("/data/data/") {
            format!("/data/user/0/{rest}")
        } else if norm == "/data/data" {
            "/data/user/0".to_string()
        } else {
            norm.to_string()
        }
    }

    /// Resolve `guest` to its xattr store key, requiring the node to exist.
    /// `write` selects the error for a read-only node: `EROFS` when the caller
    /// would mutate, while a read is allowed to look (and find nothing).
    fn xattr_key(&self, guest: &str, write: bool) -> Result<String, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let key = Self::xattr_guest_key(&norm);
        match self.target(&norm) {
            Target::Denied(e) => Err(e),
            Target::ReadOnly(h) => {
                let h = self.safe_path(&h, &self.boot_canon, true)?;
                std::fs::metadata(&h).map_err(errno_of)?;
                if write {
                    Err(EROFS)
                } else {
                    Ok(key)
                }
            }
            Target::Writable(h) => {
                let h = self.safe_path(&h, &self.root_canon, true)?;
                std::fs::metadata(&h).map_err(errno_of)?;
                Ok(key)
            }
            Target::Synthetic(_) | Target::Host(_) => {
                if write {
                    Err(EROFS)
                } else {
                    Ok(key)
                }
            }
        }
    }

    /// Whether `name` addresses the one xattr namespace the sandbox models.
    /// A real Android data partition carries `user.*` attrs for an app; the
    /// `trusted.`/`security.`/`system.` namespaces need a capability model
    /// ACOS does not have, so they report `EOPNOTSUPP`.
    fn xattr_namespace_ok(name: &str) -> bool {
        name.len() > "user.".len() && name.starts_with("user.")
    }

    /// `setxattr(2)` for the modelled namespace.
    pub fn setxattr(
        &mut self,
        guest: &str,
        name: &str,
        value: &[u8],
        flags: i32,
    ) -> Result<(), i32> {
        const XATTR_CREATE: i32 = 1;
        const XATTR_REPLACE: i32 = 2;
        if !Self::xattr_namespace_ok(name) {
            return Err(EOPNOTSUPP);
        }
        let key = self.xattr_key(guest, true)?;
        let slot = (key, name.to_string());
        let exists = self.xattrs.contains_key(&slot);
        if flags & XATTR_CREATE != 0 && exists {
            return Err(EEXIST);
        }
        if flags & XATTR_REPLACE != 0 && !exists {
            return Err(ENODATA);
        }
        self.xattrs.insert(slot, value.to_vec());
        Ok(())
    }

    /// `getxattr(2)`: the stored value or `ENODATA`.
    pub fn getxattr(&self, guest: &str, name: &str) -> Result<Vec<u8>, i32> {
        if !Self::xattr_namespace_ok(name) {
            return Err(EOPNOTSUPP);
        }
        let key = self.xattr_key(guest, false)?;
        self.xattrs
            .get(&(key, name.to_string()))
            .cloned()
            .ok_or(ENODATA)
    }

    /// `listxattr(2)`: the `user.*` names set on the node. Sorted so the reply
    /// is deterministic (the kernel order is unspecified).
    pub fn listxattr(&self, guest: &str) -> Result<Vec<String>, i32> {
        let key = self.xattr_key(guest, false)?;
        let mut names: Vec<String> = self
            .xattrs
            .keys()
            .filter(|(path, _)| *path == key)
            .map(|(_, name)| name.clone())
            .collect();
        names.sort();
        Ok(names)
    }

    /// `removexattr(2)`: drop the stored value or `ENODATA`.
    pub fn removexattr(&mut self, guest: &str, name: &str) -> Result<(), i32> {
        if !Self::xattr_namespace_ok(name) {
            return Err(EOPNOTSUPP);
        }
        let key = self.xattr_key(guest, true)?;
        self.xattrs
            .remove(&(key, name.to_string()))
            .map(|_| ())
            .ok_or(ENODATA)
    }

    /// Drop every stored xattr on `guest` and, for a directory, its children.
    fn drop_xattrs_under(&mut self, guest: &str) {
        let prefix = format!("{guest}/");
        self.xattrs
            .retain(|(path, _), _| path != guest && !path.starts_with(&prefix));
    }

    /// Move the xattrs of a renamed node (and its subtree) to its new path.
    fn move_xattrs(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        let moved: Vec<((String, String), Vec<u8>)> = self
            .xattrs
            .iter()
            .filter(|((path, _), _)| path == from || path.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for ((path, name), value) in moved {
            self.xattrs.remove(&(path.clone(), name.clone()));
            let new_path = if path == from {
                to.to_string()
            } else {
                format!("{to}{}", &path[from.len()..])
            };
            self.xattrs.insert((new_path, name), value);
        }
    }

    /// Tolerant canonicalisation for `java.io.UnixFileSystem.canonicalize0`.
    ///
    /// Android's `canonicalize_md.c` resolves the existing prefix of a path and
    /// appends the non-existent remainder, so `File.mkdirs()` can canonicalise
    /// the directory it is about to create. The sandbox still fails closed if
    /// the existing prefix resolves outside a mapped root.
    pub fn canonical_guest_lenient(&self, guest: &str) -> Result<String, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let (host, canon_root) = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::Synthetic(_) | Target::Host(_) => return Ok(norm),
            Target::ReadOnly(h) => (h, &self.boot_canon),
            Target::Writable(h) => (h, &self.root_canon),
        };
        let mut probe = host.as_path();
        loop {
            if let Ok(canon) = std::fs::canonicalize(probe) {
                if !canon.starts_with(canon_root) {
                    return Err(EACCES);
                }
                break;
            }
            match probe.parent() {
                Some(parent) => probe = parent,
                None => break,
            }
        }
        if let Some(rest) = norm.strip_prefix("/data/data/") {
            return Ok(format!("/data/user/0/{rest}"));
        }
        if norm == "/data/data" {
            return Ok("/data/user/0".to_string());
        }
        Ok(norm)
    }

    /// A canonical guest path for an existing file (`realpath`/`canonicalize`).
    ///
    /// The guest path is normalised lexically and existence is verified through
    /// the sandbox; `/data/data/<pkg>` is rewritten to `/data/user/0/<pkg>` to
    /// match the device symlink.
    pub fn canonical_guest(&self, guest: &str) -> Result<String, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(ENOENT)?;
        let host = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::Synthetic(_) => return Ok(norm),
            Target::Host(h) => h,
            Target::ReadOnly(h) => self.safe_path(&h, &self.boot_canon, true)?,
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, true)?,
        };
        std::fs::metadata(&host).map_err(errno_of)?;
        if let Some(rest) = norm.strip_prefix("/data/data/") {
            return Ok(format!("/data/user/0/{rest}"));
        }
        Ok(norm)
    }

    pub fn list(&self, guest: &str) -> Result<Vec<String>, i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        let host = match self.target(&norm) {
            Target::Denied(e) => return Err(e),
            Target::ReadOnly(h) => self.safe_path(&h, &self.boot_canon, true)?,
            Target::Writable(h) => self.safe_path(&h, &self.root_canon, true)?,
            Target::Host(_) | Target::Synthetic(_) => return Err(ENOTDIR),
        };
        let entries = std::fs::read_dir(&host).map_err(errno_of)?;
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort_unstable();
        Ok(names)
    }

    pub fn statvfs(&self, guest: &str) -> Result<StatVfs, i32> {
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        match self.target(&norm) {
            Target::Denied(e) => Err(e),
            Target::ReadOnly(h) => {
                self.safe_path(&h, &self.boot_canon, true)?;
                Ok(synthetic_statvfs())
            }
            Target::Writable(h) => {
                self.safe_path(&h, &self.root_canon, true)?;
                Ok(synthetic_statvfs())
            }
            Target::Host(_) => Ok(synthetic_statvfs()),
            Target::Synthetic(_) => Ok(synthetic_statvfs()),
        }
    }

    pub fn fstatvfs(&self) -> StatVfs {
        synthetic_statvfs()
    }

    pub fn utimens(&mut self, guest: &str, mtime: i64) -> Result<(), i32> {
        self.check_broken()?;
        let norm = normalize_guest(guest).ok_or(EACCES)?;
        match self.target(&norm) {
            Target::Denied(e) => Err(e),
            Target::Writable(h) => {
                let h = self.safe_path(&h, &self.root_canon, true)?;
                let f = OpenOptions::new().write(true).open(&h).map_err(errno_of)?;
                let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.max(0) as u64);
                f.set_modified(t).map_err(errno_of)
            }
            _ => Err(EROFS),
        }
    }

    /// `pipe2(flags)` returning the read and write descriptor numbers.
    pub fn pipe2(&mut self, flags: i32) -> Result<(i32, i32), i32> {
        self.check_broken()?;
        if self.fd_table.len() + 2 > self.max_open_fds {
            return Err(EMFILE);
        }
        let mut pflags = rustix::pipe::PipeFlags::empty();
        if flags & O_CLOEXEC != 0 {
            pflags |= rustix::pipe::PipeFlags::CLOEXEC;
        }
        if flags & O_NONBLOCK != 0 {
            pflags |= rustix::pipe::PipeFlags::NONBLOCK;
        }
        let (r, w) = rustix::pipe::pipe_with(pflags).map_err(rustix_errno)?;
        let rfd = self.alloc_fd(OpenFile {
            source: Source::File(File::from(r)),
            guest_path: "pipe:[read]".to_string(),
            flags: O_RDONLY | flags,
            readable: true,
            writable: false,
            append: false,
        })?;
        let wfd = match self.alloc_fd(OpenFile {
            source: Source::File(File::from(w)),
            guest_path: "pipe:[write]".to_string(),
            flags: O_WRONLY | flags,
            readable: false,
            writable: true,
            append: false,
        }) {
            Ok(fd) => fd,
            Err(e) => {
                self.fd_table.remove(&rfd);
                return Err(e);
            }
        };
        Ok((rfd, wfd))
    }

    /// `socketpair(AF_UNIX, type | flags, 0)`, returning the two descriptor
    /// numbers. Only local `AF_UNIX` pairs are created: they are the
    /// sandbox-safe case (`ParcelFileDescriptor.createPipe`,
    /// `LocalSocket`); every other family/protocol is refused rather than
    /// touching the host network.
    pub fn socketpair(&mut self, domain: i32, type_: i32, protocol: i32) -> Result<(i32, i32), i32> {
        self.check_broken()?;
        const AF_UNIX: i32 = 1;
        const EAFNOSUPPORT: i32 = 97;
        const EPROTONOSUPPORT: i32 = 93;
        if domain != AF_UNIX {
            return Err(EAFNOSUPPORT);
        }
        if protocol != 0 {
            return Err(EPROTONOSUPPORT);
        }
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        // `type` may carry the two socket flags Linux allows alongside it.
        let flags = type_ & (O_CLOEXEC | O_NONBLOCK);
        let type_bits = type_ & !(O_CLOEXEC | O_NONBLOCK);
        let sock_type = match type_bits {
            1 => SocketType::STREAM,
            2 => SocketType::DGRAM,
            3 => SocketType::RAW,
            5 => SocketType::SEQPACKET,
            _ => return Err(EPROTONOSUPPORT),
        };
        let mut sflags = SocketFlags::empty();
        if flags & O_CLOEXEC != 0 {
            sflags |= SocketFlags::CLOEXEC;
        }
        if flags & O_NONBLOCK != 0 {
            sflags |= SocketFlags::NONBLOCK;
        }
        let (a, b) =
            rustix::net::socketpair(AddressFamily::UNIX, sock_type, sflags, None).map_err(rustix_errno)?;
        if self.fd_table.len() + 2 > self.max_open_fds {
            return Err(EMFILE);
        }
        let afd = self.alloc_fd(OpenFile {
            source: Source::File(File::from(a)),
            guest_path: "socketpair:[0]".to_string(),
            flags: O_RDWR | flags,
            readable: true,
            writable: true,
            append: false,
        })?;
        let bfd = match self.alloc_fd(OpenFile {
            source: Source::File(File::from(b)),
            guest_path: "socketpair:[1]".to_string(),
            flags: O_RDWR | flags,
            readable: true,
            writable: true,
            append: false,
        }) {
            Ok(fd) => fd,
            Err(e) => {
                self.fd_table.remove(&afd);
                return Err(e);
            }
        };
        Ok((afd, bfd))
    }

    /// `memfd_create(name, flags)`, a real anonymous host memory fd tracked by
    /// the descriptor table. The name only labels the fd for diagnostics.
    pub fn memfd_create(&mut self, name: &str, flags: i32) -> Result<i32, i32> {
        self.check_broken()?;
        const MFD_CLOEXEC: i32 = 1;
        const MFD_ALLOW_SEALING: i32 = 2;
        use rustix::fs::MemfdFlags;
        let mut mflags = MemfdFlags::empty();
        if flags & MFD_CLOEXEC != 0 {
            mflags |= MemfdFlags::CLOEXEC;
        }
        if flags & MFD_ALLOW_SEALING != 0 {
            mflags |= MemfdFlags::ALLOW_SEALING;
        }
        let owned = rustix::fs::memfd_create(name, mflags).map_err(rustix_errno)?;
        self.alloc_fd(OpenFile {
            source: Source::File(File::from(owned)),
            guest_path: format!("memfd:{name}"),
            flags: O_RDWR | (flags & O_CLOEXEC),
            readable: true,
            writable: true,
            append: false,
        })
    }

    /// `poll(2)`. `fds` is `(fd, events, revents)`; `revents` is written back.
    pub fn poll(&mut self, fds: &mut [(i32, i16, i16)], timeout_ms: i32) -> Result<usize, i32> {
        use rustix::event::{PollFd, PollFlags};
        for slot in fds.iter_mut() {
            slot.2 = 0;
        }
        let mut indices: Vec<usize> = Vec::with_capacity(fds.len());
        let mut pollfds: Vec<PollFd<'_>> = Vec::with_capacity(fds.len());
        for (i, &(fd, events, _)) in fds.iter().enumerate() {
            let Some(of) = self.fd_table.get(&fd) else {
                continue;
            };
            let Source::File(f) = &of.source else {
                continue;
            };
            let mut flags = PollFlags::empty();
            if events & 0x1 != 0 {
                flags |= PollFlags::IN;
            }
            if events & 0x4 != 0 {
                flags |= PollFlags::OUT;
            }
            indices.push(i);
            pollfds.push(PollFd::new(f, flags));
        }
        let timeout = if timeout_ms < 0 {
            None
        } else {
            Some(rustix::event::Timespec {
                tv_sec: (timeout_ms / 1000) as i64,
                tv_nsec: ((timeout_ms % 1000) as i64) * 1_000_000,
            })
        };
        let n = rustix::event::poll(&mut pollfds, timeout.as_ref()).map_err(rustix_errno)?;
        for (idx, pfd) in indices.iter().zip(pollfds.iter()) {
            let rev = pfd.revents();
            let mut bits = 0i16;
            if rev.intersects(PollFlags::IN) {
                bits |= 0x1;
            }
            if rev.intersects(PollFlags::OUT) {
                bits |= 0x4;
            }
            if rev.intersects(PollFlags::ERR) {
                bits |= 0x8;
            }
            if rev.intersects(PollFlags::HUP) {
                bits |= 0x10;
            }
            if rev.intersects(PollFlags::NVAL) {
                bits |= 0x20;
            }
            fds[*idx].2 = bits;
        }
        Ok(n)
    }

    fn synthetic_content(&self, kind: Synthetic) -> (Vec<u8>, u32) {
        let text = match kind {
            Synthetic::Status => format!(
                "Name:\tacos\nState:\tR (running)\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\n",
                self.pid, self.pid, self.pid, self.uid, self.uid, self.uid, self.uid,
                self.uid, self.uid, self.uid, self.uid
            ),
            Synthetic::Stat => format!(
                "{} (acos) R {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
                self.pid, self.pid, self.pid
            ),
            Synthetic::Maps => {
                "00400000-00401000 r-xp 00000000 00:00 0\n\
                 00600000-00601000 r--p 00000000 00:00 0\n\
                 00601000-00602000 rw-p 00000000 00:00 0\n\
                 7f0000000000-7f0000001000 rw-p 00000000 00:00 0\n"
                    .to_string()
            }
        };
        (text.into_bytes(), 0o444)
    }

    fn synthetic_stat(&self, kind: Synthetic) -> Stat {
        let (data, mode) = self.synthetic_content(kind);
        Stat {
            mode: mode | S_IFREG,
            nlink: 1,
            uid: self.uid,
            gid: self.uid,
            size: data.len() as u64,
            blksize: 4096,
            blocks: data.len().div_ceil(512) as u64,
            ..Stat::default()
        }
    }
}

impl Drop for Vfs {
    fn drop(&mut self) {
        // Closing the map drops every `File`, releasing the host descriptors.
        self.fd_table.clear();
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.root);
            // Remove the per-session `<id>` directory too (only succeeds when
            // it is empty, so a shared `ACOS_SESSION_ROOT` is never touched).
            if let Some(parent) = self.root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Synthetic {
    Status,
    Stat,
    Maps,
}

#[derive(Clone, Debug)]
enum Target {
    ReadOnly(PathBuf),
    Writable(PathBuf),
    Host(PathBuf),
    Synthetic(Synthetic),
    Denied(i32),
}

/// Lexically normalise an absolute guest path.
///
/// Returns `None` when a `..` climbs above `/` (an escape attempt).
#[must_use]
pub fn normalize_guest(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let absolute = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    for comp in absolute.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(format!("/{}", out.join("/")))
}

fn open_host(host: &Path, flags: i32, mode: i32) -> Result<File, i32> {
    let accmode = flags & O_ACCMODE;
    let read = accmode != O_WRONLY;
    let write = accmode == O_RDWR || accmode == O_WRONLY;
    let mut opts = OpenOptions::new();
    opts.read(read).write(write);
    if flags & O_CREAT != 0 {
        opts.create(true);
    }
    if flags & O_EXCL != 0 {
        opts.create_new(true);
    }
    if flags & O_TRUNC != 0 {
        opts.truncate(true);
    }
    if flags & O_APPEND != 0 {
        opts.append(true);
    }
    opts.mode(mode as u32 & 0o7777);
    let mut custom = 0i32;
    if flags & O_NOFOLLOW != 0 {
        custom |= libc::O_NOFOLLOW;
    }
    if flags & O_DIRECTORY != 0 {
        custom |= libc::O_DIRECTORY;
    }
    if flags & O_CLOEXEC != 0 {
        custom |= libc::O_CLOEXEC;
    }
    opts.custom_flags(custom);
    opts.open(host).map_err(errno_of)
}

fn clone_open_file(of: &OpenFile) -> Result<OpenFile, i32> {
    let source = match &of.source {
        Source::File(f) => Source::File(f.try_clone().map_err(errno_of)?),
        Source::Synthetic { data, pos, mode } => Source::Synthetic {
            data: data.clone(),
            pos: *pos,
            mode: *mode,
        },
    };
    Ok(OpenFile {
        source,
        guest_path: of.guest_path.clone(),
        flags: of.flags,
        readable: of.readable,
        writable: of.writable,
        append: of.append,
    })
}

fn mode_permits_read(md: &std::fs::Metadata) -> bool {
    let ft = md.file_type();
    ft.is_file() || ft.is_dir() || ft.is_symlink()
}

fn stat_of(md: std::fs::Metadata) -> Stat {
    Stat {
        dev: md.dev(),
        ino: md.ino(),
        mode: md.mode(),
        nlink: md.nlink(),
        uid: md.uid(),
        gid: md.gid(),
        rdev: md.rdev(),
        size: md.size(),
        blksize: md.blksize(),
        blocks: md.blocks(),
        atime: md.atime(),
        mtime: md.mtime(),
        ctime: md.ctime(),
    }
}

fn synthetic_statvfs() -> StatVfs {
    StatVfs {
        bsize: 4096,
        frsize: 4096,
        blocks: 1 << 20,
        bfree: 1 << 19,
        bavail: 1 << 19,
        files: 1 << 16,
        ffree: 1 << 15,
        favail: 1 << 15,
        fsid: 0,
        flag: 0,
        namemax: 255,
    }
}

/// Map a host `io::Error` to its raw errno (`EIO` when unknown).
#[must_use]
pub fn errno_of(e: std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

/// Map a `rustix` syscall error to its raw errno.
#[must_use]
pub fn rustix_errno(e: rustix::io::Errno) -> i32 {
    e.raw_os_error()
}

/// The libc/bionic `strerror` text for an errno (no `(os error N)` suffix).
#[must_use]
pub fn strerror(errno: i32) -> &'static str {
    match errno {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        3 => "No such process",
        4 => "Interrupted system call",
        5 => "Input/output error",
        6 => "No such device or address",
        7 => "Argument list too long",
        8 => "Exec format error",
        9 => "Bad file descriptor",
        10 => "No child processes",
        11 => "Resource temporarily unavailable",
        12 => "Cannot allocate memory",
        13 => "Permission denied",
        14 => "Bad address",
        15 => "Block device required",
        16 => "Device or resource busy",
        17 => "File exists",
        18 => "Invalid cross-device link",
        19 => "No such device",
        20 => "Not a directory",
        21 => "Is a directory",
        22 => "Invalid argument",
        23 => "Too many open files in system",
        24 => "Too many open files",
        25 => "Inappropriate ioctl for device",
        26 => "Text file busy",
        27 => "File too large",
        28 => "No space left on device",
        29 => "Illegal seek",
        30 => "Read-only file system",
        31 => "Too many links",
        32 => "Broken pipe",
        33 => "Numerical argument out of domain",
        34 => "Numerical result out of range",
        35 => "Resource deadlock avoided",
        36 => "File name too long",
        37 => "No locks available",
        38 => "Function not implemented",
        39 => "Directory not empty",
        40 => "Too many levels of symbolic links",
        42 => "No message of desired type",
        43 => "Identifier removed",
        61 => "No data available",
        62 => "Timer expired",
        71 => "Protocol error",
        75 => "Value too large for defined data type",
        95 => "Operation not supported",
        122 => "Disk quota exceeded",
        125 => "Operation canceled",
        _ => "Unknown error",
    }
}

/// The `OsConstants.errnoName` uppercase name for an errno.
#[must_use]
pub fn errno_name(errno: i32) -> &'static str {
    match errno {
        1 => "EPERM",
        2 => "ENOENT",
        3 => "ESRCH",
        4 => "EINTR",
        5 => "EIO",
        6 => "ENXIO",
        7 => "E2BIG",
        8 => "ENOEXEC",
        9 => "EBADF",
        10 => "ECHILD",
        11 => "EAGAIN",
        12 => "ENOMEM",
        13 => "EACCES",
        14 => "EFAULT",
        16 => "EBUSY",
        17 => "EEXIST",
        18 => "EXDEV",
        19 => "ENODEV",
        20 => "ENOTDIR",
        21 => "EISDIR",
        22 => "EINVAL",
        23 => "ENFILE",
        24 => "EMFILE",
        25 => "ENOTTY",
        26 => "ETXTBSY",
        27 => "EFBIG",
        28 => "ENOSPC",
        29 => "ESPIPE",
        30 => "EROFS",
        31 => "EMLINK",
        32 => "EPIPE",
        33 => "EDOM",
        34 => "ERANGE",
        35 => "EDEADLK",
        36 => "ENAMETOOLONG",
        37 => "ENOLCK",
        38 => "ENOSYS",
        39 => "ENOTEMPTY",
        40 => "ELOOP",
        61 => "ENODATA",
        95 => "EOPNOTSUPP",
        122 => "EDQUOT",
        125 => "ECANCELED",
        _ => "UNKNOWN",
    }
}

// `S_IFMT`/`S_IFLNK`/`S_IFSOCK` are part of the documented mapping; keep them
// referenced so the constants stay meaningful for future `stat` consumers.
const _: u32 = S_IFMT | S_IFDIR | S_IFREG | S_IFLNK | S_IFSOCK;

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        base: PathBuf,
        vfs: Vfs,
    }

    impl Sandbox {
        fn new(name: &str, max_fds: usize, max_bytes: u64) -> Sandbox {
            let base = std::env::temp_dir().join(format!(
                "acos-vfs-{name}-{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let boot = base.join("boot");
            let root = base.join("scratch");
            std::fs::create_dir_all(boot.join("fonts")).unwrap();
            std::fs::create_dir_all(root.join("data/user/0/com.example.app/files")).unwrap();
            std::fs::write(
                boot.join("fonts/fonts.xml"),
                b"<families>\n  <family name=\"sans-serif\"/>\n</families>\n",
            )
            .unwrap();
            std::fs::write(boot.join("fonts/Roboto-Regular.ttf"), b"TTF").unwrap();
            let vfs = Vfs::new(VfsConfig {
                root,
                boot_root: boot.clone(),
                pid: 4242,
                uid: 10192,
                max_open_fds: max_fds,
                max_bytes_written: max_bytes,
                cleanup: true,
            });
            Sandbox { base, vfs }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn write_file(path: &Path, data: &[u8]) {
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn normalize_rejects_escape_above_root() {
        assert_eq!(normalize_guest("/a/b/../c").as_deref(), Some("/a/c"));
        assert_eq!(normalize_guest("a//b/./c").as_deref(), Some("/a/b/c"));
        assert_eq!(normalize_guest("/../../etc/passwd"), None);
        assert_eq!(normalize_guest("/a/../../b"), None);
    }

    #[test]
    fn reads_system_fonts_xml_via_file_stream_contract() {
        let mut sb = Sandbox::new("fonts", 64, 1 << 20);
        let fd = sb
            .vfs
            .open("/system/etc/fonts.xml", O_RDONLY, 0)
            .expect("open fonts.xml");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8];
        loop {
            let n = sb.vfs.read(fd, &mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        assert!(String::from_utf8_lossy(&buf).starts_with("<families>"));
        sb.vfs.close(fd).unwrap();

        // `/system/fonts/*` maps onto the extracted font tree.
        let ttf = sb.vfs.open("/system/fonts/Roboto-Regular.ttf", O_RDONLY, 0);
        assert!(ttf.is_ok(), "{ttf:?}");
        sb.vfs.close(ttf.unwrap()).unwrap();
    }

    /// Regression: `AndroidDataFiles.generateIcuDataPath()` builds
    /// `/apex/com.android.i18n/etc/icu/` from `ANDROID_I18N_ROOT`. That prefix
    /// must resolve to the extracted i18n apex in the boot cache, or
    /// `Typeface.<clinit>` throws `MissingResourceException` for
    /// `likelySubtags` (W40-A).
    #[test]
    fn reads_i18n_apex_icu_data_under_the_real_device_path() {
        let mut sb = Sandbox::new("i18n", 64, 1 << 20);
        let icu = sb.vfs.boot_root().join("apex/com.android.i18n/etc/icu");
        std::fs::create_dir_all(&icu).unwrap();
        write_file(&icu.join("icudt72l.dat"), b"ICU-DATA");

        let fd = sb
            .vfs
            .open("/apex/com.android.i18n/etc/icu/icudt72l.dat", O_RDONLY, 0)
            .expect("open icudt72l.dat under /apex/com.android.i18n");
        let mut buf = [0u8; 8];
        assert_eq!(sb.vfs.read(fd, &mut buf).unwrap(), 8);
        assert_eq!(&buf, b"ICU-DATA");
        sb.vfs.close(fd).unwrap();

        // The synthetic `/system/apex/...` spelling keeps working too.
        let fd = sb
            .vfs
            .open("/system/apex/com.android.i18n/etc/icu/icudt72l.dat", O_RDONLY, 0)
            .expect("open icudt72l.dat under /system/apex");
        sb.vfs.close(fd).unwrap();
    }

    #[test]
    fn write_read_back_in_app_files_dir() {
        let mut sb = Sandbox::new("rw", 64, 1 << 20);
        let flags = O_RDWR | O_CREAT | O_TRUNC;
        let fd = sb
            .vfs
            .open("/data/user/0/com.example.app/files/probe.bin", flags, 0o600)
            .expect("create");
        assert_eq!(sb.vfs.write(fd, b"hello acos").unwrap(), 10);
        assert_eq!(sb.vfs.lseek(fd, 0, SEEK_SET).unwrap(), 0);
        let mut buf = [0u8; 10];
        assert_eq!(sb.vfs.read(fd, &mut buf).unwrap(), 10);
        assert_eq!(&buf, b"hello acos");
        sb.vfs.close(fd).unwrap();

        // `/data/data/<pkg>` is the device symlink onto `/data/user/0/<pkg>`.
        let fd = sb
            .vfs
            .open("/data/data/com.example.app/files/probe.bin", O_RDONLY, 0)
            .expect("read via /data/data");
        let mut buf = [0u8; 10];
        assert_eq!(sb.vfs.read(fd, &mut buf).unwrap(), 10);
        assert_eq!(&buf, b"hello acos");
    }

    #[test]
    fn system_mount_is_read_only() {
        let mut sb = Sandbox::new("ro", 64, 1 << 20);
        assert_eq!(
            sb.vfs.open("/system/etc/fonts.xml", O_WRONLY, 0),
            Err(EROFS)
        );
        assert_eq!(
            sb.vfs.open("/system/new_file", O_RDWR | O_CREAT, 0o600),
            Err(EROFS)
        );
    }

    #[test]
    fn lexical_traversal_is_rejected() {
        let mut sb = Sandbox::new("traversal", 64, 1 << 20);
        // A `..` that exactly reaches `/` leaves an unmapped path: ENOENT.
        assert_eq!(
            sb.vfs.open(
                "/data/user/0/com.example.app/../../../../etc/passwd",
                O_RDONLY,
                0
            ),
            Err(ENOENT)
        );
        // One more `..` climbs above `/`: EACCES.
        assert_eq!(
            sb.vfs.open(
                "/data/user/0/com.example.app/../../../../../etc/passwd",
                O_RDONLY,
                0
            ),
            Err(EACCES)
        );
        assert_eq!(
            sb.vfs.open("/system/../../etc/passwd", O_RDONLY, 0),
            Err(EACCES)
        );
        // `/etc/passwd` is not inside any mapped root.
        assert_eq!(sb.vfs.open("/etc/passwd", O_RDONLY, 0), Err(ENOENT));
    }

    #[test]
    fn unknown_roots_are_enoent() {
        let mut sb = Sandbox::new("unknown", 64, 1 << 20);
        assert_eq!(sb.vfs.open("/etc/passwd", O_RDONLY, 0), Err(ENOENT));
        assert_eq!(sb.vfs.open("/data/local/tmp/x", O_RDONLY, 0), Err(ENOENT));
        assert!(!sb.vfs.denials().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_fails_closed() {
        let mut sb = Sandbox::new("symlink", 64, 1 << 20);
        let link = sb.vfs.root().join("data/user/0/com.example.app/files/evil");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        assert_eq!(
            sb.vfs
                .open("/data/user/0/com.example.app/files/evil", O_RDONLY, 0),
            Err(EACCES)
        );
        // A symlink to a legitimate file inside the same sandbox is allowed.
        let ok = sb.vfs.root().join("data/user/0/com.example.app/files/ok");
        write_file(&ok, b"inside");
        let link2 = sb
            .vfs
            .root()
            .join("data/user/0/com.example.app/files/link2");
        std::os::unix::fs::symlink("ok", &link2).unwrap();
        let fd = sb
            .vfs
            .open("/data/user/0/com.example.app/files/link2", O_RDONLY, 0)
            .expect("internal symlink");
        let mut buf = [0u8; 6];
        assert_eq!(sb.vfs.read(fd, &mut buf).unwrap(), 6);
        assert_eq!(&buf, b"inside");
    }

    #[test]
    fn open_fd_quota_is_enforced() {
        let mut sb = Sandbox::new("fdquota", 2, 1 << 20);
        let a = sb.vfs.open("/system/etc/fonts.xml", O_RDONLY, 0).unwrap();
        let b = sb
            .vfs
            .open("/system/fonts/Roboto-Regular.ttf", O_RDONLY, 0)
            .unwrap();
        assert_eq!(sb.vfs.open("/dev/null", O_RDONLY, 0), Err(EMFILE));
        sb.vfs.close(a).unwrap();
        sb.vfs.close(b).unwrap();
        assert!(sb.vfs.open("/dev/null", O_RDONLY, 0).is_ok());
    }

    #[test]
    fn dev_null_writes_are_not_quota_limited() {
        let mut sb = Sandbox::new("devnull", 64, 2);
        let fd = sb.vfs.open("/dev/null", O_WRONLY, 0).unwrap();
        for _ in 0..10 {
            sb.vfs.write(fd, b"xx").unwrap();
        }
        assert_eq!(sb.vfs.bytes_written(), 0);
        sb.vfs.close(fd).unwrap();
    }

    #[test]
    fn write_byte_quota_is_enforced() {
        let mut sb = Sandbox::new("bytequota", 64, 4);
        let fd = sb
            .vfs
            .open(
                "/data/user/0/com.example.app/files/q",
                O_RDWR | O_CREAT,
                0o600,
            )
            .unwrap();
        assert_eq!(sb.vfs.write(fd, b"abc").unwrap(), 3);
        assert_eq!(sb.vfs.write(fd, b"def"), Err(EDQUOT));
    }

    #[test]
    fn dup_shares_the_file_offset() {
        let mut sb = Sandbox::new("dup", 64, 1 << 20);
        let fd = sb
            .vfs
            .open(
                "/data/user/0/com.example.app/files/d",
                O_RDWR | O_CREAT | O_TRUNC,
                0o600,
            )
            .unwrap();
        sb.vfs.write(fd, b"0123456789").unwrap();
        sb.vfs.lseek(fd, 0, SEEK_SET).unwrap();
        let dup = sb.vfs.dup(fd).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(sb.vfs.read(fd, &mut buf).unwrap(), 4);
        assert_eq!(&buf, b"0123");
        // The duplicate sees the advanced offset.
        let mut buf2 = [0u8; 4];
        assert_eq!(sb.vfs.read(dup, &mut buf2).unwrap(), 4);
        assert_eq!(&buf2, b"4567");
    }

    #[test]
    fn proc_self_status_is_synthesised() {
        let mut sb = Sandbox::new("proc", 64, 1 << 20);
        let fd = sb.vfs.open("/proc/self/status", O_RDONLY, 0).unwrap();
        let mut buf = [0u8; 256];
        let n = sb.vfs.read(fd, &mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf[..n]);
        assert!(text.contains("Name:\tacos"));
        assert!(text.contains("Pid:\t4242"));
        assert_eq!(sb.vfs.open("/proc/self/cmdline", O_RDONLY, 0), Err(ENOENT));
    }

    #[test]
    fn realpath_returns_the_guest_path() {
        let sb = Sandbox::new("realpath", 64, 1 << 20);
        assert_eq!(
            sb.vfs.canonical_guest("/system/etc/fonts.xml").unwrap(),
            "/system/etc/fonts.xml"
        );
        assert_eq!(
            sb.vfs
                .canonical_guest("/data/data/com.example.app/files")
                .unwrap(),
            "/data/user/0/com.example.app/files"
        );
        assert_eq!(sb.vfs.canonical_guest("/system/nope"), Err(ENOENT));
    }

    #[test]
    fn lenient_canonicalize_accepts_a_missing_leaf() {
        let sb = Sandbox::new("lenient", 64, 1 << 20);
        assert_eq!(
            sb.vfs
                .canonical_guest_lenient("/data/user/0/com.example.app/files/new.txt")
                .unwrap(),
            "/data/user/0/com.example.app/files/new.txt"
        );
        assert_eq!(
            sb.vfs
                .canonical_guest_lenient("/data/data/com.example.app/files/new.txt")
                .unwrap(),
            "/data/user/0/com.example.app/files/new.txt"
        );
    }

    #[test]
    fn pipe_and_poll_round_trip() {
        let mut sb = Sandbox::new("pipe", 64, 1 << 20);
        let (r, w) = sb.vfs.pipe2(0).unwrap();
        sb.vfs.write(w, b"ping").unwrap();
        let mut slots = [(r, 0x1i16, 0i16)];
        let ready = sb.vfs.poll(&mut slots, 0).unwrap();
        assert_eq!(ready, 1);
        assert_ne!(slots[0].2 & 0x1, 0, "read end must report POLLIN");
        let mut buf = [0u8; 4];
        assert_eq!(sb.vfs.read(r, &mut buf).unwrap(), 4);
        assert_eq!(&buf, b"ping");
        sb.vfs.close(r).unwrap();
        sb.vfs.close(w).unwrap();
    }

    #[test]
    fn dropping_the_vfs_releases_fds_and_temp_files() {
        let root;
        {
            let mut sb = Sandbox::new("drop", 64, 1 << 20);
            root = sb.vfs.root().to_path_buf();
            let fd = sb
                .vfs
                .open(
                    "/data/user/0/com.example.app/files/x",
                    O_RDWR | O_CREAT,
                    0o600,
                )
                .unwrap();
            sb.vfs.write(fd, b"temp").unwrap();
            assert_eq!(sb.vfs.open_fd_count(), 1);
            assert!(root.exists());
            // `sb` drops here, running `Vfs::drop`.
        }
        assert!(!root.exists(), "session scratch tree must be removed");
    }
}
