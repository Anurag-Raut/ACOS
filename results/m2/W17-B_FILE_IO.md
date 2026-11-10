# W17-B — guest file-I/O substrate (VFS + `libcore.io.Linux`)

**Status: landed.** A per-process VFS maps Android paths onto sandboxed host
locations; the real `FileInputStream` / `RandomAccessFile` / `File` /
`FileOutputStream` / `Os` paths run on the real Android 14 core library with no
host path visible to the guest.

## Files

| file | change |
|---|---|
| `crates/acos-vm/src/vfs.rs` (**new**) | the sandbox: path mapping, lexical `..` rejection, symlink-escape guards, fd table with preserved offsets, per-session fd/byte quotas, drop cleanup, bionic `strerror`/`errno_name`. |
| `crates/acos-vm/src/natives/io_linux.rs` (**new**) | `libcore.io.Linux` file natives, `FileInputStream.available0/skip0`, `java.io.FileDescriptor.isSocket/sync`, `libcore.io.AsynchronousCloseMonitor.signalBlockedThreads`, and the `java.io.UnixFileSystem` backend. |
| `crates/acos-vm/src/natives/io.rs` | old host-path stubs removed; `strerror` now returns bionic text; **`OsConstants.initConstants` implemented** (see below). |
| `crates/acos-vm/src/natives/mod.rs` | `mod io_linux;` + registration immediately after `io`. |
| `crates/acos-vm/src/vfs.rs` / `vm.rs` / `lib.rs` | `Vm` owns a `pub vfs: Vfs`; `Real` boot dir wired into the read-only root; `Vfs`/`VfsConfig` exported. |
| `crates/acos-vm/Cargo.toml` | `rustix` `pipe` feature, `libc` (const values only; the crate stays `#![forbid(unsafe_code)]`). |
| `crates/acos-vm/tests/file_io.rs` (**new**), `tests/java/FileIoMain.java` (**new**) | Java-level acceptance; 16 VFS unit/security tests (incl. traversal, symlink escape, quotas, drop cleanup, pipe/poll). |

## Path map

| guest | host | access |
|---|---|---|
| `/system/**` (`etc/fonts.xml` → `fonts/fonts.xml`, `fonts/*`, `framework/framework-res.apk`, `etc/preloaded-classes`) | `.cache/boot/android-34/**` | read-only (`EROFS` on write) |
| `/apex/com.android.tzdata/**` | `.cache/boot/android-34/apex/...` | read-only |
| `/data/user/<n>/<pkg>/**` | `<root>/data/user/<n>/<pkg>/**` | r/w |
| `/data/data/<pkg>/**` | `<root>/data/user/0/<pkg>/**` (device symlink) | r/w |
| `/data`, `/data/user`, `/data/data` | the corresponding sandbox dirs | r/w |
| `/proc/self/{status,stat,maps}` | synthesised | read-only |
| `/dev/null`, `/dev/zero`, `/dev/{u,}random` | host device | per flags |
| anything else | — | `ENOENT` (structured denial log) |

`<root>` = `ACOS_SESSION_ROOT` or `.cache/sessions/<id>/data` (created on
construction, removed on drop). Limits: `ACOS_MAX_OPEN_FDS` (1024),
`ACOS_MAX_BYTES_WRITTEN` (64 MiB → `EDQUOT`).

## Security

* Lexical normalisation rejects a `..` that climbs above `/` (`EACCES`).
* Every mapped host path canonicalises its parent (and the final component
  unless `O_NOFOLLOW`/`lstat`/`readlink`/`remove`) and must stay under the
  canonical root; a symlink pointing at `/etc/passwd` fails closed with
  `EACCES`, while `readlink` still reports the link target.
* Quotas tested (`EMFILE`, `EDQUOT`); dropping the `Vfs` closes the fd table and
  removes the scratch tree.

## `OsConstants.initConstants` (unplanned but required)

`android.system.OsConstants.<clinit>` initialises every field from
`placeholder()` (0) and then calls the native `initConstants()`. With the old
no-op every constant was 0: `O_WRONLY == O_RDONLY`, `S_ISDIR` was true for every
file (`IoBridge.open` threw), and `errnoName` reported the first zero field.
`io.rs` now sets **322** Linux-generic x86_64 values (open flags, `S_IF*`,
`E*`, `F_*`, `SEEK_*`, access, poll, signal, socket, mmap). 217 uncaptured
names (mostly `_SC_*` and rtnetlink) stay 0; none is on the file path.

## Tests

```
cargo test -p acos-vm                                             73 lib + all integration suites pass
cargo test -p acos-vm --test file_io                              PASS
cargo clippy -p acos-vm --all-targets -- -D warnings              clean
tools/diff-dexprogs.sh --corelib real                             23 pass / 0 fail / 0 unsupported
```

`file_io.rs` (real core library) asserts: `fonts_head` starts
`<?xml version="1.0"`; `RandomAccessFile` seek(6)/length(67396)/getFilePointer;
write + read-back in `/data/user/0/com.example.app/files` and the
`/data/data` alias; and the exact `ErrnoException` text

```
stat failed: ENOENT (No such file or directory)
open failed: EROFS (Read-only file system)
stat failed: EACCES (Permission denied)
```

Reference label: **`jvm/linux-reference`** — no emulator was attached; the
messages are the real `ErrnoException.getMessage()` contract with bionic/glibc
`strerror` and the real `OsConstants.errnoName` values (the model layer is real
Android bytecode, only `strerror` is our table).

## M2 effect

`lifecycle_probe` classes 592 → **657**; the single VFS denial is the optional
`/product/etc/fonts_customization.xml`. The M2 blocker named in
`M2_LOG.md` (file I/O + `/system` mapping) is cleared; the next stop is a Java
exception deeper in the framework, past `Typeface`/`FontListParser`
(`result`/asset layer), not a file-I/O `UNSUPPORTED_JAVA_API`.

## Risks / gaps

* `strerror` is a host-generated table; the 3 pinned cases match bionic, but
  uncommon errnos fall back to `"Unknown error"`.
* `OsConstants` table is host-UAPI-generated; `SIGRTMIN`/`_SC_*` are
  deliberately incomplete.
* `Linux.readBytes` handles `byte[]`; direct/heap `ByteBuffer` reads
  (`FileChannel`) are not implemented — `EFAULT`.
* `chown`/`fchown`/`lchown` are validated no-ops (no root to change owner).
* `/proc/self/maps` is a small synthetic map, not the host's.
