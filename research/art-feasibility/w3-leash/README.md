# w3-leash — how W3 settled the `InsetsSourceControl` leash wall

Short version: the guest was never "88 bytes early"; it was refusing a **null binder** in a
`readStrongBinder` slot and the 88 bytes were the cascade. The full report is
`results/art-runtime/W3.md`; this directory keeps the measurement.

```text
scripts/cascade.py        replay the image's readers over the real reply frame; prints the
                          guest's own offsets (client @1352 -> UNEXPECTED_NULL, next @1440)
scripts/repro.sh          cascade | launch | render | attach
scripts/attach.gdb        optional host gdb: SurfaceControl::readFromParcel + Parcel readers,
                          printing dataPosition/dataSize/mObjectsSize/mNextObjectHint
scripts/ptracer.c         freestanding PR_SET_PTRACER(-1) ctor appended to the scratch data
                          root's libartshim.so so host gdb may attach under yama=1
scripts/find-guest.py     the launch guest's host pid (acos-art -> bwrap -> dalvikvm64)
artifacts/w3-frame-add.hex    the 2352-byte reply frame the guest driver handed to libbinder
artifacts/w3-cascade.txt      its replay (the two log offsets, reproduced)
artifacts/w3-launch-before.log  the wall: Parcel/SurfaceControl logd entries + the stack
artifacts/w3-launch-after.log   after the fix: no Parcel/SurfaceControl entries, next wall
```

Why gdb was prepared and then not needed: the reply frame is deterministic and the image's
reader order was already verified by W2, so replaying the readers over the exact frame bytes
reproduces every offset in the guest's log. `scripts/attach.gdb` remains useful for the next
wall, but note that it needs the `ptracer.c` shim: the guest is exec'd by the user manager
(`systemd-run --scope`), not by our shell, so yama `ptrace_scope=1` denies a plain `gdb -p`.
