# AOSP source reference for agents (read-only)

Local sparse checkouts of Google's open-source Android at tag android-14.0.0_r75 (approx. 73 MB), in `/home/Qui/Projects/acos/.cache/aosp/` (gitignored; recreate with `tools/fetch-aosp-reference.sh`).
Use them to understand behaviour and wire formats BEFORE guessing or disassembling. Do not copy source into the repo; the compiled binaries from the API-34 system image stay the thing we run. The tag is close to, not identical to, the image build: verify against the image when it matters (ELF build ID, disassembly).

| Dir | Contents | Use for |
|---|---|---|
| `native/` | libs/binder (IPCThreadState, ProcessState, Parcel, BpBinder, ndk), include/binder, libs/ui, libs/gui | Binder driver protocol (BC_*/BR_*), node refcounting, flat_binder_object, stability |
| `base/` | core/jni (AndroidRuntime.cpp, android_os_*, registration order), libs/hwui (+ jni/), libs/androidfw, core/java/android/os, com/android/internal/os | libandroid_runtime registration, graphics JNI, AssetManager/overlays, zygote/RuntimeInit |
| `art/` | runtime, libartbase, libnativeloader, libnativebridge | boot image/classpath logic (e.g. mainline CHECK), linker namespaces, runtime flags |
| `bionic/` | linker, libc/bionic, libc/include | linker namespaces, property area, sysconf |
| `libbase/`, `core/` | libbase, libutils, libcutils, liblog, property_service | RefBase, logging, properties |
| `connectivity/`, `wifi/`, `bluetooth/`, `permission/` | `framework/src/android/net`, `framework/java/android/net/wifi`, `framework/java/android/bluetooth`, `framework-s/java/android/app/role` | the mainline service-client classes behind the ADR-0029 tombstone table (feature gates, lazy proxies) |

`base/` also carries the client paths the tombstone table cites:
`core/java/android/{app,content,net,os,view,webkit}`, `media/java/android/media`
(+ `session/`), `nfc/java/android/nfc`, `telephony/java/android/telephony`,
`hardware/java/android/hardware`.

Sparse checkouts: add more paths with `git -C <dir> sparse-checkout add <path>` (blobs are fetched on demand, partial clone). Cone mode only accepts directories; a single file path needs `--skip-checks` (which is what `tools/fetch-aosp-reference.sh` uses for `MediaRouter.java`).
