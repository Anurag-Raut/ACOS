//! `libcore.io.Linux` process/posix natives (M1 subset) plus the ICU version
//! queries.
//!
//! The file-I/O natives live in [`super::io_linux`] and are registered right
//! after this module so they replace the temporary std-stream stubs. This module
//! keeps the identity/environment calls (`getpid`, `getenv`, `uname`, …) and the
//! `strerror` text `android.system.ErrnoException.getMessage()` needs.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::{Handle, NULL};
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Llibcore/io/Linux;";
    vm.register_native(c, "isatty", "(Ljava/io/FileDescriptor;)Z", isatty);
    vm.register_native(c, "getpid", "()I", getpid);
    vm.register_native(c, "gettid", "()I", gettid);
    vm.register_native(c, "getuid", "()I", getuid);
    vm.register_native(c, "geteuid", "()I", getuid);
    vm.register_native(c, "getgid", "()I", getuid);
    vm.register_native(c, "getegid", "()I", getuid);
    vm.register_native(c, "getppid", "()I", getpid);
    vm.register_native(
        c,
        "getenv",
        "(Ljava/lang/String;)Ljava/lang/String;",
        getenv,
    );
    vm.register_native(
        c,
        "setenv",
        "(Ljava/lang/String;Ljava/lang/String;Z)V",
        setenv,
    );
    vm.register_native(c, "unsetenv", "(Ljava/lang/String;)V", unsetenv);
    vm.register_native(c, "uname", "()Landroid/system/StructUtsname;", uname);
    vm.register_native(c, "getpwuid", "(I)Landroid/system/StructPasswd;", getpwuid);
    vm.register_native(c, "strerror", "(I)Ljava/lang/String;", strerror);
    vm.register_native(c, "sysconf", "(I)J", sysconf);
    vm.register_native(
        c,
        "android_fdsan_exchange_owner_tag",
        "(Ljava/io/FileDescriptor;JJ)V",
        noop_fd,
    );
    vm.register_native(
        "Landroid/system/OsConstants;",
        "initConstants",
        "()V",
        init_constants,
    );
    // `java.io.UnixFileSystem.initIDs` only caches JNI field/method IDs; it has
    // no observable effect (ART's own implementation is a no-op in the fast
    // path). Called from the `<clinit>` that `LoadedApk`/`Context` trigger.
    vm.register_native("Ljava/io/UnixFileSystem;", "initIDs", "()V", noop_fd);
    icu_versions(vm);
    Ok(())
}

/// `libcore.icu.ICU` version queries (no ICU data required).
fn icu_versions(vm: &mut Vm) {
    let c = "Llibcore/icu/ICU;";
    vm.register_native(c, "getIcuVersion", "()Ljava/lang/String;", icu_version);
    vm.register_native(c, "getUnicodeVersion", "()Ljava/lang/String;", icu_version);
    vm.register_native(c, "getCldrVersion", "()Ljava/lang/String;", icu_version);
}

fn icu_version(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("74.2")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn getenv(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    match std::env::var(&name) {
        Ok(v) => {
            let h = vm.new_string(&v)?;
            vm.ret_ref(ctx, h);
        }
        Err(_) => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn setenv(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn unsetenv(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn strerror(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // bionic/libc text without Rust's "(os error N)" suffix; `ErrnoException`
    // concatenates this into its exact ART message.
    let msg = crate::vfs::strerror(vm.arg_i32(ctx, 0));
    let h = vm.new_string(msg)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sysconf(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 4096);
    Ok(())
}

fn new_struct(vm: &mut Vm, desc: &str, fields: &[(&str, StructValue)]) -> VmResult<Handle> {
    let cid = vm.load_class(desc)?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        for (name, val) in fields {
            match val {
                StructValue::Str(s) => {
                    let h = vm.new_string(s)?;
                    vm.native_roots.push(h);
                    vm.set_instance_field_by_name(obj, name, h);
                    vm.native_roots.pop();
                }
                StructValue::Int(i) => vm.set_instance_field_by_name(obj, name, *i as u32),
            }
        }
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

enum StructValue {
    Str(&'static str),
    Int(i32),
}

fn uname(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = new_struct(
        vm,
        "Landroid/system/StructUtsname;",
        &[
            ("sysname", StructValue::Str("Linux")),
            ("nodename", StructValue::Str("localhost")),
            ("release", StructValue::Str("5.10.0")),
            ("version", StructValue::Str("#1 SMP")),
            ("machine", StructValue::Str("x86_64")),
        ],
    )?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn getpwuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let uid = vm.arg_i32(ctx, 0);
    let obj = new_struct(
        vm,
        "Landroid/system/StructPasswd;",
        &[
            ("pw_name", StructValue::Str("acos")),
            ("pw_passwd", StructValue::Str("x")),
            ("pw_uid", StructValue::Int(uid)),
            ("pw_gid", StructValue::Int(0)),
            ("pw_gecos", StructValue::Str("acos")),
            ("pw_dir", StructValue::Str("/")),
            ("pw_shell", StructValue::Str("/bin/sh")),
        ],
    )?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// The integer fd stored in a `java.io.FileDescriptor`.
fn fd_of(vm: &Vm, fd_obj: Handle) -> Option<i32> {
    if fd_obj == NULL {
        return None;
    }
    vm.get_instance_field_by_name(fd_obj, "descriptor")
        .or_else(|| vm.get_instance_field_by_name(fd_obj, "fd"))
        .map(|v| v as i32)
}

fn isatty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).unwrap_or(-1);
    vm.ret_bool(ctx, fd == 0 || fd == 1 || fd == 2);
    Ok(())
}

fn getpid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, std::process::id() as i32);
    Ok(())
}

fn gettid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, std::process::id() as i32);
    Ok(())
}

fn getuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn noop_fd(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// `android.system.OsConstants` values (Linux generic, x86_64).
///
/// `OsConstants.<clinit>` initialises every field from `placeholder()` (which
/// returns 0) and then calls this native. Without the real values the whole
/// `android.system` surface reads zero: `O_WRONLY` becomes `O_RDONLY`,
/// `S_ISDIR` is true for every file (so `IoBridge.open` throws), and
/// `errnoName` reports the wrong name. The table is the ABI surface the
/// framework's file/socket layer compares against; values match the x86_64
/// Linux UAPI that Android and the ACOS host share.
fn os_constant_table() -> &'static [(&'static str, i32)] {
    &[
        ("AF_INET", 2),
        ("AF_INET6", 10),
        ("AF_NETLINK", 16),
        ("AF_PACKET", 17),
        ("AF_UNIX", 1),
        ("AF_UNSPEC", 0),
        ("AF_VSOCK", 40),
        ("AI_ADDRCONFIG", 32),
        ("AI_ALL", 16),
        ("AI_CANONNAME", 2),
        ("AI_NUMERICHOST", 4),
        ("AI_NUMERICSERV", 1024),
        ("AI_PASSIVE", 1),
        ("AI_V4MAPPED", 8),
        ("E2BIG", 7),
        ("EACCES", 13),
        ("EADDRINUSE", 98),
        ("EADDRNOTAVAIL", 99),
        ("EAFNOSUPPORT", 97),
        ("EAGAIN", 11),
        ("EAI_AGAIN", -3),
        ("EAI_BADFLAGS", -1),
        ("EAI_FAIL", -4),
        ("EAI_FAMILY", -6),
        ("EAI_MEMORY", -10),
        ("EAI_NODATA", -5),
        ("EAI_NONAME", -2),
        ("EAI_OVERFLOW", -12),
        ("EAI_SERVICE", -8),
        ("EAI_SOCKTYPE", -7),
        ("EAI_SYSTEM", -11),
        ("EALREADY", 114),
        ("EBADF", 9),
        ("EBADMSG", 74),
        ("EBUSY", 16),
        ("ECANCELED", 125),
        ("ECHILD", 10),
        ("ECONNABORTED", 103),
        ("ECONNREFUSED", 111),
        ("ECONNRESET", 104),
        ("EDEADLK", 35),
        ("EDESTADDRREQ", 89),
        ("EDOM", 33),
        ("EDQUOT", 122),
        ("EEXIST", 17),
        ("EFAULT", 14),
        ("EFBIG", 27),
        ("EHOSTUNREACH", 113),
        ("EIDRM", 43),
        ("EILSEQ", 84),
        ("EINPROGRESS", 115),
        ("EINTR", 4),
        ("EINVAL", 22),
        ("EIO", 5),
        ("EISCONN", 106),
        ("EISDIR", 21),
        ("ELOOP", 40),
        ("EMFILE", 24),
        ("EMLINK", 31),
        ("EMSGSIZE", 90),
        ("EMULTIHOP", 72),
        ("ENAMETOOLONG", 36),
        ("ENETDOWN", 100),
        ("ENETRESET", 102),
        ("ENETUNREACH", 101),
        ("ENFILE", 23),
        ("ENOBUFS", 105),
        ("ENODATA", 61),
        ("ENODEV", 19),
        ("ENOENT", 2),
        ("ENOEXEC", 8),
        ("ENOLCK", 37),
        ("ENOLINK", 67),
        ("ENOMEM", 12),
        ("ENOMSG", 42),
        ("ENONET", 64),
        ("ENOPROTOOPT", 92),
        ("ENOSPC", 28),
        ("ENOSR", 63),
        ("ENOSTR", 60),
        ("ENOSYS", 38),
        ("ENOTCONN", 107),
        ("ENOTDIR", 20),
        ("ENOTEMPTY", 39),
        ("ENOTSOCK", 88),
        ("ENOTSUP", 95),
        ("ENOTTY", 25),
        ("ENXIO", 6),
        ("EOPNOTSUPP", 95),
        ("EOVERFLOW", 75),
        ("EPERM", 1),
        ("EPIPE", 32),
        ("EPROTO", 71),
        ("EPROTONOSUPPORT", 93),
        ("EPROTOTYPE", 91),
        ("ERANGE", 34),
        ("EROFS", 30),
        ("ESPIPE", 29),
        ("ESRCH", 3),
        ("ESTALE", 116),
        ("ETH_P_ALL", 3),
        ("ETIME", 62),
        ("ETIMEDOUT", 110),
        ("ETXTBSY", 26),
        ("EUSERS", 87),
        ("EXDEV", 18),
        ("FD_CLOEXEC", 1),
        ("FIONREAD", 21531),
        ("F_DUPFD", 0),
        ("F_DUPFD_CLOEXEC", 1030),
        ("F_GETFD", 1),
        ("F_GETFL", 3),
        ("F_GETLK", 5),
        ("F_GETLK64", 5),
        ("F_GETOWN", 9),
        ("F_OK", 0),
        ("F_RDLCK", 0),
        ("F_SETFD", 2),
        ("F_SETFL", 4),
        ("F_SETLK", 6),
        ("F_SETLK64", 6),
        ("F_SETLKW", 7),
        ("F_SETLKW64", 7),
        ("F_SETOWN", 8),
        ("F_UNLCK", 2),
        ("F_WRLCK", 1),
        ("IPPROTO_ICMP", 1),
        ("IPPROTO_ICMPV6", 58),
        ("IPPROTO_IP", 0),
        ("IPPROTO_IPV6", 41),
        ("IPPROTO_RAW", 255),
        ("IPPROTO_TCP", 6),
        ("IPPROTO_UDP", 17),
        ("IPV6_CHECKSUM", 7),
        ("IPV6_MULTICAST_HOPS", 18),
        ("IPV6_MULTICAST_IF", 17),
        ("IPV6_MULTICAST_LOOP", 19),
        ("IPV6_RECVDSTOPTS", 58),
        ("IPV6_RECVHOPLIMIT", 51),
        ("IPV6_RECVHOPOPTS", 53),
        ("IPV6_RECVPKTINFO", 49),
        ("IPV6_RECVRTHDR", 56),
        ("IPV6_RECVTCLASS", 66),
        ("IPV6_TCLASS", 67),
        ("IPV6_UNICAST_HOPS", 16),
        ("IPV6_V6ONLY", 26),
        ("IP_MULTICAST_IF", 32),
        ("IP_MULTICAST_LOOP", 34),
        ("IP_MULTICAST_TTL", 33),
        ("IP_RECVTOS", 13),
        ("IP_TOS", 1),
        ("IP_TTL", 2),
        ("MAP_ANONYMOUS", 32),
        ("MAP_POPULATE", 32768),
        ("MAP_PRIVATE", 2),
        ("MAP_SHARED", 1),
        ("MFD_CLOEXEC", 1),
        ("MSG_CTRUNC", 8),
        ("MSG_DONTROUTE", 4),
        ("MSG_EOR", 128),
        ("MSG_OOB", 1),
        ("MSG_PEEK", 2),
        ("MSG_TRUNC", 32),
        ("MSG_WAITALL", 256),
        ("NETLINK_ROUTE", 0),
        ("NI_DGRAM", 16),
        ("NI_NAMEREQD", 8),
        ("NI_NOFQDN", 4),
        ("NI_NUMERICHOST", 1),
        ("NI_NUMERICSERV", 2),
        ("O_ACCMODE", 3),
        ("O_APPEND", 1024),
        ("O_CLOEXEC", 524288),
        ("O_CREAT", 64),
        ("O_DIRECT", 16384),
        ("O_DSYNC", 4096),
        ("O_EXCL", 128),
        ("O_NOCTTY", 256),
        ("O_NOFOLLOW", 131072),
        ("O_NONBLOCK", 2048),
        ("O_RDONLY", 0),
        ("O_RDWR", 2),
        ("O_SYNC", 1052672),
        ("O_TRUNC", 512),
        ("O_WRONLY", 1),
        ("POLLERR", 8),
        ("POLLHUP", 16),
        ("POLLIN", 1),
        ("POLLNVAL", 32),
        ("POLLOUT", 4),
        ("POLLPRI", 2),
        ("POLLRDBAND", 128),
        ("POLLRDNORM", 64),
        ("POLLWRBAND", 512),
        ("POLLWRNORM", 256),
        ("PROT_EXEC", 4),
        ("PROT_READ", 1),
        ("PROT_WRITE", 2),
        ("RLIMIT_NOFILE", 7),
        ("R_OK", 4),
        ("SEEK_CUR", 1),
        ("SEEK_END", 2),
        ("SEEK_SET", 0),
        ("SHUT_RD", 0),
        ("SHUT_RDWR", 2),
        ("SHUT_WR", 1),
        ("SIGABRT", 6),
        ("SIGALRM", 14),
        ("SIGBUS", 7),
        ("SIGCHLD", 17),
        ("SIGCONT", 18),
        ("SIGFPE", 8),
        ("SIGHUP", 1),
        ("SIGILL", 4),
        ("SIGINT", 2),
        ("SIGIO", 29),
        ("SIGKILL", 9),
        ("SIGPIPE", 13),
        ("SIGPROF", 27),
        ("SIGPWR", 30),
        ("SIGQUIT", 3),
        ("SIGRTMAX", 64),
        ("SIGRTMIN", 34),
        ("SIGSEGV", 11),
        ("SIGSTKFLT", 16),
        ("SIGSTOP", 19),
        ("SIGSYS", 31),
        ("SIGTERM", 15),
        ("SIGTRAP", 5),
        ("SIGTSTP", 20),
        ("SIGTTIN", 21),
        ("SIGTTOU", 22),
        ("SIGURG", 23),
        ("SIGUSR1", 10),
        ("SIGUSR2", 12),
        ("SIGVTALRM", 26),
        ("SIGWINCH", 28),
        ("SIGXCPU", 24),
        ("SIGXFSZ", 25),
        ("SOCK_CLOEXEC", 524288),
        ("SOCK_DGRAM", 2),
        ("SOCK_NONBLOCK", 2048),
        ("SOCK_RAW", 3),
        ("SOCK_SEQPACKET", 5),
        ("SOCK_STREAM", 1),
        ("SOL_SOCKET", 1),
        ("SOL_UDP", 17),
        ("SO_BINDTODEVICE", 25),
        ("SO_BROADCAST", 6),
        ("SO_DEBUG", 1),
        ("SO_DOMAIN", 39),
        ("SO_DONTROUTE", 5),
        ("SO_ERROR", 4),
        ("SO_KEEPALIVE", 9),
        ("SO_LINGER", 13),
        ("SO_OOBINLINE", 10),
        ("SO_PASSCRED", 16),
        ("SO_PEERCRED", 17),
        ("SO_PROTOCOL", 38),
        ("SO_RCVBUF", 8),
        ("SO_RCVLOWAT", 18),
        ("SO_RCVTIMEO", 20),
        ("SO_REUSEADDR", 2),
        ("SO_SNDBUF", 7),
        ("SO_SNDLOWAT", 19),
        ("SO_SNDTIMEO", 21),
        ("SO_TYPE", 3),
        ("SPLICE_F_MORE", 4),
        ("SPLICE_F_MOVE", 1),
        ("SPLICE_F_NONBLOCK", 2),
        ("STDERR_FILENO", 2),
        ("STDIN_FILENO", 0),
        ("STDOUT_FILENO", 1),
        ("ST_MANDLOCK", 64),
        ("ST_NOATIME", 1024),
        ("ST_NODEV", 4),
        ("ST_NODIRATIME", 2048),
        ("ST_NOEXEC", 8),
        ("ST_NOSUID", 2),
        ("ST_RDONLY", 1),
        ("ST_RELATIME", 4096),
        ("ST_SYNCHRONOUS", 16),
        ("S_IFBLK", 24576),
        ("S_IFCHR", 8192),
        ("S_IFDIR", 16384),
        ("S_IFIFO", 4096),
        ("S_IFLNK", 40960),
        ("S_IFREG", 32768),
        ("S_IFSOCK", 49152),
        ("S_IRGRP", 32),
        ("S_IROTH", 4),
        ("S_IRUSR", 256),
        ("S_IRWXG", 56),
        ("S_IRWXO", 7),
        ("S_IRWXU", 448),
        ("S_ISGID", 1024),
        ("S_ISUID", 2048),
        ("S_ISVTX", 512),
        ("S_IWGRP", 16),
        ("S_IWOTH", 2),
        ("S_IWUSR", 128),
        ("S_IXGRP", 8),
        ("S_IXOTH", 1),
        ("S_IXUSR", 64),
        ("TCP_NODELAY", 1),
        ("TCP_USER_TIMEOUT", 18),
        ("TIOCOUTQ", 21521),
        ("VMADDR_CID_ANY", -1),
        ("VMADDR_CID_HOST", 2),
        ("VMADDR_CID_LOCAL", 1),
        ("VMADDR_PORT_ANY", -1),
        ("WCONTINUED", 8),
        ("WEXITED", 4),
        ("WNOHANG", 1),
        ("WNOWAIT", 16777216),
        ("WSTOPPED", 2),
        ("WUNTRACED", 2),
        ("W_OK", 2),
        ("XATTR_CREATE", 1),
        ("XATTR_REPLACE", 2),
        ("X_OK", 1),
        ("S_IFMT", 61440),
    ]
}

fn init_constants(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    for (name, value) in os_constant_table() {
        // The class is mid-`<clinit>`; the field slots already exist.
        let _ = vm.set_static_field(
            "android.system.OsConstants",
            name,
            crate::javabridge::JavaValue::Int(*value),
        );
    }
    Ok(())
}
