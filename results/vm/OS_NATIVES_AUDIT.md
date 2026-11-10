# `libcore.io.Linux` native audit + `OsConstants`

Task: W25-B. HEAD `40f075c`. Scope: `android.system.OsConstants.initConstants` and the
`libcore.io.Linux` native surface (the file/process/socket shim behind
`android.system.Os` and `java.io`).

## 1. `android.system.OsConstants.initConstants`

* **Status: implemented** in `crates/acos-vm/src/natives/os_constants.rs`, registered
  after `io`/`io_linux` (which carried a partial, partly-wrong 322-entry table).
* **Coverage: 538/538** `public static final int` fields declared by
  `Landroid/system/OsConstants;` in the real API-34 `core-libart` DEX; none is left at
  the `placeholder()` zero.
* **Tier: ART-oracle-captured (not header-derived).** The API-34 `x86_64` emulator
  (`tools/oracle/emulator.sh`) booted in 14 s; every field was reflected from
  `OsConstants.class` with `DumpOs` (`getDeclaredFields()` / `getInt(null)`) and compared
  field-by-field with the NDK-27 bionic headers for `x86_64` and `aarch64`. The 538
  captures matched the `x86_64` headers exactly; `SIGRTMIN`/`SIGRTMAX` are the only
  non-macro values and come from the oracle (`41`/`64`).
* **Architecture:** the host and the ART oracle are `x86_64`. The full set is
  ABI-identical to `aarch64` except two `open(2)` flags:
  `O_DIRECT` (`x86_64 0x4000` vs `arm64 0x10000`) and `O_NOFOLLOW`
  (`x86_64 0x20000` vs `arm64 0x8000`). The `x86_64` values are used so guest `open`
  flags stay coherent with the host syscalls the VFS performs; `arm64` would be the
  correct choice for an arm64 host, and the divergence is called out in the module doc.
* **Bugs fixed by the full table:** the old table used glibc-style resolver constants
  (`AI_ADDRCONFIG 32→1024`, `AI_ALL 16→256`, `AI_NUMERICSERV 1024→8`, `AI_V4MAPPED 8→2048`,
  `NI_*`, all `EAI_*` sign/value conventions) and `SIGRTMIN 34→41`.
* **Verification:** `crates/acos-vm/tests/os_constants.rs` + `tests/java/OsConstantsMain.java`
  run on the real core library and compare a full-table FNV-1a-64 checksum
  (`7342789864980853825`), the field count, key constants, `errnoName`/`gaiName`, and
  file/stat/pipe/scatter-gather round trips against the ART capture. The two
  deliberately sandbox-specific outputs (`umask`, `environ`) are asserted separately.

## 2. `libcore.io.Linux` native audit

Declared natives extracted from the real API-34
`core-libart` DEX (`dexdump` + `tools/oracle/dex_native_methods.py`): **134** declared.

| status | count |
|---|---|
| implemented | 64 |
| pending | 9 |
| unsupported-by-design | 61 |

Unregistered natives become the structured `UNSUPPORTED_JAVA_API` naming the exact
`Lclass;->name(sig)`; that is the intended "returns a structured error" behaviour for
host-affecting operations (there is no silent fake and no host path escape).

| native | signature | status | notes |
|---|---|---|---|
| `accept` | `(Ljava/io/FileDescriptor;Ljava/net/SocketAddress;)Ljava/io/FileDescriptor;` | unsupported-by-design |  |
| `access` | `(Ljava/lang/String;I)Z` | implemented | VFS access, fails closed |
| `android_fdsan_exchange_owner_tag` | `(Ljava/io/FileDescriptor;JJ)V` | implemented | no-op |
| `android_fdsan_get_owner_tag` | `(Ljava/io/FileDescriptor;)J` | pending |  |
| `android_fdsan_get_tag_type` | `(J)Ljava/lang/String;` | pending |  |
| `android_fdsan_get_tag_value` | `(J)J` | pending |  |
| `android_getaddrinfo` | `(Ljava/lang/String;Landroid/system/StructAddrinfo;I)[Ljava/net/InetAddress;` | pending |  |
| `bind` | `(Ljava/io/FileDescriptor;Ljava/net/InetAddress;I)V` | unsupported-by-design |  |
| `bind` | `(Ljava/io/FileDescriptor;Ljava/net/SocketAddress;)V` | unsupported-by-design |  |
| `capget` | `(Landroid/system/StructCapUserHeader;)[Landroid/system/StructCapUserData;` | unsupported-by-design |  |
| `capset` | `(Landroid/system/StructCapUserHeader;[Landroid/system/StructCapUserData;)V` | unsupported-by-design |  |
| `chmod` | `(Ljava/lang/String;I)V` | implemented | sandbox-internal chmod |
| `chown` | `(Ljava/lang/String;II)V` | implemented | recorded no-op |
| `close` | `(Ljava/io/FileDescriptor;)V` | implemented | VFS close; std fds preserved |
| `connect` | `(Ljava/io/FileDescriptor;Ljava/net/InetAddress;I)V` | unsupported-by-design |  |
| `connect` | `(Ljava/io/FileDescriptor;Ljava/net/SocketAddress;)V` | unsupported-by-design |  |
| `dup` | `(Ljava/io/FileDescriptor;)Ljava/io/FileDescriptor;` | implemented | VFS dup |
| `dup2` | `(Ljava/io/FileDescriptor;I)Ljava/io/FileDescriptor;` | implemented | VFS dup2 |
| `environ` | `()[Ljava/lang/String;` | implemented | empty array: host env is never exported |
| `execv` | `(Ljava/lang/String;[Ljava/lang/String;)V` | unsupported-by-design |  |
| `execve` | `(Ljava/lang/String;[Ljava/lang/String;[Ljava/lang/String;)V` | unsupported-by-design |  |
| `fchmod` | `(Ljava/io/FileDescriptor;I)V` | implemented | sandbox-internal chmod |
| `fchown` | `(Ljava/io/FileDescriptor;II)V` | implemented | recorded no-op (no root in sandbox) |
| `fcntlInt` | `(Ljava/io/FileDescriptor;II)I` | implemented | VFS fcntl (F_GETFL/F_SETFL/…) |
| `fcntlVoid` | `(Ljava/io/FileDescriptor;I)I` | implemented | VFS fcntl |
| `fdatasync` | `(Ljava/io/FileDescriptor;)V` | implemented | VFS fdatasync |
| `fstat` | `(Ljava/io/FileDescriptor;)Landroid/system/StructStat;` | implemented | VFS fstat → StructStat |
| `fstatvfs` | `(Ljava/io/FileDescriptor;)Landroid/system/StructStatVfs;` | implemented | synthetic statvfs |
| `fsync` | `(Ljava/io/FileDescriptor;)V` | implemented | VFS fsync |
| `ftruncate` | `(Ljava/io/FileDescriptor;J)V` | implemented | VFS ftruncate |
| `gai_strerror` | `(I)Ljava/lang/String;` | implemented | bionic text for the Android EAI_* values |
| `getegid` | `()I` | implemented | app gid |
| `getenv` | `(Ljava/lang/String;)Ljava/lang/String;` | implemented | host environment read |
| `geteuid` | `()I` | implemented | app uid |
| `getgid` | `()I` | implemented | app gid |
| `getifaddrs` | `()[Landroid/system/StructIfaddrs;` | unsupported-by-design |  |
| `getnameinfo` | `(Ljava/net/InetAddress;I)Ljava/lang/String;` | pending |  |
| `getpeername` | `(Ljava/io/FileDescriptor;)Ljava/net/SocketAddress;` | unsupported-by-design |  |
| `getpgid` | `(I)I` | implemented | single process group → own pid |
| `getpid` | `()I` | implemented | process id |
| `getppid` | `()I` | implemented | process id |
| `getpwnam` | `(Ljava/lang/String;)Landroid/system/StructPasswd;` | pending |  |
| `getpwuid` | `(I)Landroid/system/StructPasswd;` | implemented | synthetic passwd row |
| `getrlimit` | `(I)Landroid/system/StructRlimit;` | implemented | RLIMIT_NOFILE = VFS ceiling (1024/1048576); else RLIM_INFINITY |
| `getsockname` | `(Ljava/io/FileDescriptor;)Ljava/net/SocketAddress;` | unsupported-by-design |  |
| `getsockoptByte` | `(Ljava/io/FileDescriptor;II)I` | pending |  |
| `getsockoptInAddr` | `(Ljava/io/FileDescriptor;II)Ljava/net/InetAddress;` | unsupported-by-design |  |
| `getsockoptInt` | `(Ljava/io/FileDescriptor;II)I` | implemented | SOL_SOCKET/SO_DOMAIN → AF_UNIX; else ENOPROTOOPT |
| `getsockoptLinger` | `(Ljava/io/FileDescriptor;II)Landroid/system/StructLinger;` | implemented | (0,0): ACOS never enables SO_LINGER; needed by FileDescriptor.close() |
| `getsockoptTimeval` | `(Ljava/io/FileDescriptor;II)Landroid/system/StructTimeval;` | unsupported-by-design |  |
| `getsockoptUcred` | `(Ljava/io/FileDescriptor;II)Landroid/system/StructUcred;` | unsupported-by-design |  |
| `gettid` | `()I` | implemented | thread id |
| `getuid` | `()I` | implemented | app uid 10192 (overridden by os_misc) |
| `getxattr` | `(Ljava/lang/String;Ljava/lang/String;)[B` | unsupported-by-design |  |
| `if_indextoname` | `(I)Ljava/lang/String;` | unsupported-by-design |  |
| `if_nametoindex` | `(Ljava/lang/String;)I` | unsupported-by-design |  |
| `inet_pton` | `(ILjava/lang/String;)Ljava/net/InetAddress;` | pending |  |
| `ioctlFlags` | `(Ljava/io/FileDescriptor;Ljava/lang/String;)I` | unsupported-by-design |  |
| `ioctlInetAddress` | `(Ljava/io/FileDescriptor;ILjava/lang/String;)Ljava/net/InetAddress;` | unsupported-by-design |  |
| `ioctlInt` | `(Ljava/io/FileDescriptor;I)I` | unsupported-by-design |  |
| `ioctlMTU` | `(Ljava/io/FileDescriptor;Ljava/lang/String;)I` | unsupported-by-design |  |
| `isatty` | `(Ljava/io/FileDescriptor;)Z` | implemented | std fds only |
| `kill` | `(II)V` | unsupported-by-design |  |
| `lchown` | `(Ljava/lang/String;II)V` | implemented | recorded no-op |
| `link` | `(Ljava/lang/String;Ljava/lang/String;)V` | implemented | VFS link |
| `listen` | `(Ljava/io/FileDescriptor;I)V` | unsupported-by-design |  |
| `listxattr` | `(Ljava/lang/String;)[Ljava/lang/String;` | unsupported-by-design |  |
| `lseek` | `(Ljava/io/FileDescriptor;JI)J` | implemented |  |
| `lstat` | `(Ljava/lang/String;)Landroid/system/StructStat;` | implemented | VFS non-following stat → StructStat |
| `memfd_create` | `(Ljava/lang/String;I)Ljava/io/FileDescriptor;` | implemented | real anonymous host fd, VFS-tracked |
| `mincore` | `(JJ[B)V` | unsupported-by-design |  |
| `mkdir` | `(Ljava/lang/String;I)V` | implemented | VFS mkdir |
| `mkfifo` | `(Ljava/lang/String;I)V` | pending |  |
| `mlock` | `(JJ)V` | unsupported-by-design |  |
| `mmap` | `(JJIILjava/io/FileDescriptor;J)J` | unsupported-by-design |  |
| `msync` | `(JJI)V` | unsupported-by-design |  |
| `munlock` | `(JJ)V` | unsupported-by-design |  |
| `munmap` | `(JJ)V` | unsupported-by-design |  |
| `open` | `(Ljava/lang/String;II)Ljava/io/FileDescriptor;` | implemented | VFS open (sandbox-resolved) |
| `pipe2` | `(I)[Ljava/io/FileDescriptor;` | implemented | rustix pipe, VFS-tracked |
| `poll` | `([Landroid/system/StructPollfd;I)I` | implemented | VFS/rustix poll over tracked fds |
| `posix_fallocate` | `(Ljava/io/FileDescriptor;JJ)V` | implemented | grows via VFS ftruncate to offset+len |
| `prctl` | `(IJJJJ)I` | unsupported-by-design |  |
| `preadBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;IIJ)I` | implemented | VFS positional read |
| `pwriteBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;IIJ)I` | implemented | VFS positional write |
| `readBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I` | implemented | VFS read / stdin |
| `readlink` | `(Ljava/lang/String;)Ljava/lang/String;` | implemented | VFS readlink (non-following) |
| `readv` | `(Ljava/io/FileDescriptor;[Ljava/lang/Object;[I[I)I` | implemented | `Os.readv`; byte[] segments only (ByteBuffer → EINVAL) |
| `realpath` | `(Ljava/lang/String;)Ljava/lang/String;` | implemented | VFS canonical_guest |
| `recvfromBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;IIILjava/net/InetSocketAddress;)I` | unsupported-by-design |  |
| `recvmsg` | `(Ljava/io/FileDescriptor;Landroid/system/StructMsghdr;I)I` | unsupported-by-design |  |
| `remove` | `(Ljava/lang/String;)V` | implemented | VFS remove |
| `removexattr` | `(Ljava/lang/String;Ljava/lang/String;)V` | unsupported-by-design |  |
| `rename` | `(Ljava/lang/String;Ljava/lang/String;)V` | implemented | VFS rename |
| `sendfile` | `(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;Landroid/system/Int64Ref;J)J` | unsupported-by-design |  |
| `sendmsg` | `(Ljava/io/FileDescriptor;Landroid/system/StructMsghdr;I)I` | unsupported-by-design |  |
| `sendtoBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;IIILjava/net/InetAddress;I)I` | unsupported-by-design |  |
| `sendtoBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;IIILjava/net/SocketAddress;)I` | unsupported-by-design |  |
| `setegid` | `(I)V` | unsupported-by-design |  |
| `setenv` | `(Ljava/lang/String;Ljava/lang/String;Z)V` | implemented | no-op |
| `seteuid` | `(I)V` | unsupported-by-design |  |
| `setgid` | `(I)V` | unsupported-by-design |  |
| `setpgid` | `(II)V` | unsupported-by-design |  |
| `setregid` | `(II)V` | unsupported-by-design |  |
| `setreuid` | `(II)V` | unsupported-by-design |  |
| `setsid` | `()I` | unsupported-by-design |  |
| `setsockoptByte` | `(Ljava/io/FileDescriptor;III)V` | unsupported-by-design |  |
| `setsockoptGroupReq` | `(Ljava/io/FileDescriptor;IILandroid/system/StructGroupReq;)V` | unsupported-by-design |  |
| `setsockoptIfreq` | `(Ljava/io/FileDescriptor;IILjava/lang/String;)V` | unsupported-by-design |  |
| `setsockoptInt` | `(Ljava/io/FileDescriptor;III)V` | unsupported-by-design |  |
| `setsockoptIpMreqn` | `(Ljava/io/FileDescriptor;III)V` | unsupported-by-design |  |
| `setsockoptLinger` | `(Ljava/io/FileDescriptor;IILandroid/system/StructLinger;)V` | unsupported-by-design |  |
| `setsockoptTimeval` | `(Ljava/io/FileDescriptor;IILandroid/system/StructTimeval;)V` | unsupported-by-design |  |
| `setuid` | `(I)V` | unsupported-by-design |  |
| `setxattr` | `(Ljava/lang/String;Ljava/lang/String;[BI)V` | unsupported-by-design |  |
| `shutdown` | `(Ljava/io/FileDescriptor;I)V` | unsupported-by-design |  |
| `socket` | `(III)Ljava/io/FileDescriptor;` | unsupported-by-design |  |
| `socketpair` | `(IIILjava/io/FileDescriptor;Ljava/io/FileDescriptor;)V` | implemented | AF_UNIX only, via rustix; VFS-tracked, ENOPROTOOPT otherwise |
| `splice` | `(Ljava/io/FileDescriptor;Landroid/system/Int64Ref;Ljava/io/FileDescriptor;Landroid/system/Int64Ref;JI)J` | unsupported-by-design |  |
| `stat` | `(Ljava/lang/String;)Landroid/system/StructStat;` | implemented | VFS stat → StructStat |
| `statvfs` | `(Ljava/lang/String;)Landroid/system/StructStatVfs;` | implemented | synthetic statvfs |
| `strerror` | `(I)Ljava/lang/String;` | implemented | bionic strerror text |
| `strsignal` | `(I)Ljava/lang/String;` | implemented | bionic text for fatal signals |
| `symlink` | `(Ljava/lang/String;Ljava/lang/String;)V` | implemented | VFS symlink |
| `sysconf` | `(I)J` | implemented | real _SC_* values (PAGESIZE 4096, CLK_TCK 100, ...); overrides the io.rs 4096 stub |
| `tcdrain` | `(Ljava/io/FileDescriptor;)V` | unsupported-by-design |  |
| `tcsendbreak` | `(Ljava/io/FileDescriptor;I)V` | unsupported-by-design |  |
| `umaskImpl` | `(I)I` | implemented | returns fixed 0022 (sandbox root is per-session) |
| `uname` | `()Landroid/system/StructUtsname;` | implemented | fixed Linux/x86_64 StructUtsname |
| `unlink` | `(Ljava/lang/String;)V` | implemented | alias of the sandboxed `remove` |
| `unsetenv` | `(Ljava/lang/String;)V` | implemented | no-op |
| `waitpid` | `(ILandroid/system/Int32Ref;I)I` | unsupported-by-design |  |
| `writeBytes` | `(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I` | implemented | VFS write / captured std streams |
| `writev` | `(Ljava/io/FileDescriptor;[Ljava/lang/Object;[I[I)I` | implemented | `Os.writev`; byte[] segments only |

### Notes on `unsupported-by-design`

* **Process control** (`kill`, `execv`, `execve`, `waitpid`, `set*id`, `setpgid`,
  `setsid`, `prctl`, `capget`, `capset`): a guest may not signal, execute, wait on or
  re-privilege host processes.
* **Network** (`socket`/`bind`/`connect`/`accept`/`listen`/`shutdown`, `send`/`recv`
  family, `ioctl*`, `getifaddrs`, `if_*`, raw `setsockopt*`): only local `AF_UNIX`
  `socketpair` is exposed; the host network is never reachable.
* **Host memory mapping** (`mmap`/`munmap`/`mlock`/`munlock`/`msync`/`mincore`): a
  guest may not map arbitrary host memory or host files.
* **Extended attributes** (`getxattr`/`setxattr`/`listxattr`/`removexattr`) and
  `chmod`/`chown` outside the sandbox: refused rather than interpreted as host paths.
* **`sendfile`/`splice`/`tcdrain`/`tcsendbreak`**: host-fd/terminal operations.

### Still pending (safe, not required this wave)

`inet_pton`, `getnameinfo`, `android_getaddrinfo`, `getsockoptByte`, `mkfifo`,
`getpwnam`, and the `android_fdsan_get_*` getters. Each is sandbox-compatible and
would be a small follow-up; none is on the boot/file/pipe path.

## 3. Tests and evidence

* `cargo test -p acos-vm --test os_constants` — passes (ART-pinned, 69 outputs).
* `cargo test -p acos-vm --test file_io` — passes (no regression in `ErrnoException`).
* `cargo test -p acos-vm --test os_natives` — passes (`Process`/uid/pid).
* `cargo test -p acos-vm --test dexprogs` — 21/21 bootstrap programs pass.
* `tools/diff-dexprogs.sh --corelib real` — **25 pass / 0 fail / 0 unsupported of 25**
  (release build from this wave; scratch-worktree report).
* `cargo clippy -p acos-vm --all-targets -- -D warnings` — clean.

The build/tests ran in a scratch worktree (`.cache/wt-w25b`) because the shared HEAD
was mid-rebase by a concurrent agent: `fwk_surface.rs` referenced an untracked,
not-yet-compiling `fwk_vsync.rs` (`mod.rs` had no declaration, and its WIP used a
`JavaValue::Array` variant that does not exist at HEAD). A one-symbol `fwk_vsync`
stub was used **only inside the scratch worktree** so the crate could build; the
main tree was not touched for it. No file in the deliverable depends on the stub.

Artifacts: the ART capture used for the checksum/sample is reproducible with the
`DumpOs` / `OsConstantsMain` app_process commands recorded in the test headers.

