# ART sysroot gap scan

entry: `libart.so`  root: `.cache/art-exp/sysroot`

libraries in closure: 18
unresolved DT_NEEDED: {}

| library | imports | table-fwd | table-shim | corpus | missing |
|---|---:|---:|---:|---:|---:|
| `libbase.so` | 162 | 63 | 19 | 61 | 19 |
| `libc++.so` | 135 | 76 | 36 | 0 | 23 |
| `libdl_android.so` | 8 | 0 | 0 | 8 | 0 |
| `ld-android.so` | 0 | 0 | 0 | 0 | 0 |
| `liblzma.so` | 16 | 11 | 5 | 0 | 0 |
| `libprocinfo.so` | 29 | 13 | 4 | 8 | 4 |
| `libunwindstack.so` | 137 | 36 | 18 | 64 | 19 |
| `libart.so` | 700 | 103 | 41 | 521 | 35 |
| `libartbase.so` | 205 | 73 | 19 | 97 | 16 |
| `libartpalette.so` | 7 | 1 | 4 | 2 | 0 |
| `libdexfile.so` | 110 | 17 | 1 | 91 | 1 |
| `liblz4.so` | 8 | 8 | 0 | 0 | 0 |
| `libnativebridge.so` | 21 | 10 | 7 | 3 | 1 |
| `libnativeloader.so` | 113 | 17 | 8 | 88 | 0 |
| `libprofile.so` | 123 | 23 | 3 | 85 | 12 |
| `libsigchain.so` | 34 | 12 | 14 | 3 | 5 |
| `libstatssocket.so` | 21 | 16 | 4 | 0 | 1 |
| `heapprofd_client_api.so` | 67 | 40 | 9 | 14 | 4 |

## Missing symbols (101)

- `_Unwind_Backtrace` — needed by libunwindstack.so
- `_Unwind_DeleteException` — needed by libc++.so
- `_Unwind_GetDataRelBase` — needed by libunwindstack.so
- `_Unwind_GetIP` — needed by libc++.so, libunwindstack.so
- `_Unwind_GetIPInfo` — needed by libunwindstack.so
- `_Unwind_GetLanguageSpecificData` — needed by libc++.so, libunwindstack.so
- `_Unwind_GetRegionStart` — needed by libc++.so, libunwindstack.so
- `_Unwind_GetTextRelBase` — needed by libunwindstack.so
- `_Unwind_RaiseException` — needed by libc++.so
- `_Unwind_Resume` — needed by libc++.so, libunwindstack.so
- `_Unwind_SetGR` — needed by libc++.so, libunwindstack.so
- `_Unwind_SetIP` — needed by libc++.so, libunwindstack.so
- `__android_log_call_aborter` — needed by libbase.so
- `__android_log_error_write` — needed by libart.so, libartbase.so
- `__android_log_get_minimum_priority` — needed by libbase.so
- `__android_log_logd_logger` — needed by libbase.so
- `__android_log_set_aborter` — needed by libbase.so
- `__android_log_set_default_tag` — needed by libbase.so
- `__android_log_set_logger` — needed by libbase.so
- `__android_log_set_minimum_priority` — needed by libbase.so
- `__android_log_write_log_message` — needed by libbase.so
- `__assert` — needed by heapprofd_client_api.so, libsigchain.so, libunwindstack.so
- `__hwasan_tag_pointer` — needed by libart.so
- `__libc_current_sigrtmax` — needed by libart.so
- `__libc_current_sigrtmin` — needed by libart.so
- `__pread64_chk` — needed by libartbase.so
- `__pwrite64_chk` — needed by libartbase.so
- `__pwrite_chk` — needed by libartbase.so, libbase.so
- `__strlcpy_chk` — needed by heapprofd_client_api.so, libbase.so, libsigchain.so, libunwindstack.so
- `__strncpy_chk2` — needed by libart.so
- `__system_property_area_serial` — needed by libbase.so
- `__system_property_set` — needed by libbase.so
- `__system_property_wait` — needed by libbase.so
- `adler32` — needed by libart.so, libdexfile.so
- `adler32_combine` — needed by libart.so
- `android_fdsan_close_with_tag` — needed by libart.so, libartbase.so, libbase.so, libprocinfo.so, libprofile.so, libunwindstack.so
- `android_fdsan_create_owner_tag` — needed by libart.so, libartbase.so, libbase.so, libprocinfo.so, libprofile.so, libunwindstack.so
- `android_fdsan_exchange_owner_tag` — needed by libart.so, libartbase.so, libbase.so, libprocinfo.so, libprofile.so, libunwindstack.so
- `android_get_device_api_level` — needed by libart.so, libartbase.so, libprofile.so
- `android_handle_signal` — needed by libsigchain.so
- `android_log_destroy` — needed by libart.so
- `android_log_write_int32` — needed by libart.so
- `android_log_write_list` — needed by libart.so
- `android_log_write_string8` — needed by libart.so
- `android_log_write_string8_len` — needed by libart.so
- `basename` — needed by libbase.so
- `create_android_logger` — needed by libart.so
- `deflate` — needed by libprofile.so
- `deflateBound` — needed by libprofile.so
- `deflateEnd` — needed by libprofile.so
- `deflateInit_` — needed by libprofile.so
- `execv` — needed by libart.so
- `execve` — needed by libart.so
- `fallocate` — needed by libart.so, libartbase.so
- `fgetc` — needed by libartbase.so
- `fstatfs` — needed by libartbase.so
- `getline` — needed by libprocinfo.so
- `getpriority` — needed by libart.so
- `getprogname` — needed by heapprofd_client_api.so, libbase.so
- `getpwnam` — needed by libartbase.so
- `inflate` — needed by libartbase.so, libprofile.so
- `inflateEnd` — needed by libartbase.so, libprofile.so
- `inflateInit2_` — needed by libartbase.so
- `inflateInit_` — needed by libprofile.so
- `iswalpha` — needed by libc++.so
- `iswblank` — needed by libc++.so
- `iswcntrl` — needed by libc++.so
- `iswdigit` — needed by libc++.so
- `iswprint` — needed by libc++.so
- `iswpunct` — needed by libc++.so
- `iswspace` — needed by libc++.so
- `iswupper` — needed by libc++.so
- `iswxdigit` — needed by libc++.so
- `mallinfo` — needed by libart.so
- `mincore` — needed by libart.so
- `mkdtemp` — needed by libbase.so
- `mkostemps` — needed by libprofile.so
- `mount` — needed by libnativebridge.so
- `pidfd_open` — needed by libart.so
- `process_vm_readv` — needed by libartbase.so, libunwindstack.so
- `pthread_getcpuclockid` — needed by libart.so
- `pthread_gettid_np` — needed by libart.so
- `pthread_sigmask64` — needed by libart.so
- `ptrace` — needed by libunwindstack.so
- `sched_getparam` — needed by libart.so
- `sched_getscheduler` — needed by libart.so
- `setpgid` — needed by libart.so
- `sigaddset64` — needed by libart.so
- `sigemptyset64` — needed by libart.so
- `sigfillset64` — needed by libsigchain.so
- `sigwait64` — needed by libart.so
- `strtod_l` — needed by libc++.so
- `strtof_l` — needed by libc++.so
- `tgkill` — needed by libunwindstack.so
- `towlower` — needed by libc++.so
- `towupper` — needed by libc++.so
- `unshare` — needed by libart.so
- `waitid` — needed by libart.so
- `wcscoll` — needed by libc++.so
- `wcsxfrm` — needed by libc++.so
- `writev` — needed by heapprofd_client_api.so, libsigchain.so, libstatssocket.so, libunwindstack.so
