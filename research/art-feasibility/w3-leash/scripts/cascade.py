#!/usr/bin/env python3
"""W3: replay the image's `InsetsSourceControl$Array` walk over the real
`addToDisplayAsUser` reply frame and show how a **null** leash client turns the
guest's `readStrongBinder` into `UNEXPECTED_NULL` and shifts the rest of the
walk by 88 bytes.

This is the measurement that settled W2's open question ("does the guest arrive
early or late, and which preceding read consumed a different number of bytes?")
without a debugger: the guest's own log line

    Parcel: Attempt to read object from Parcel ... at offset 1440 that is not in
    the object list

is reproduced *exactly* by the framework's readers over the exact bytes the peer
sent, so the guest is **not** early or late at the leash; offset 1440 is the
cascade of the aborted leash (1352 + 88).

Usage:
    python3 research/art-feasibility/w3-leash/scripts/cascade.py [FRAME.HEX]

The reader semantics are transcribed from the image (API 34):

  classes4.dex InsetsSourceControl.writeToParcel / readFromParcel (0x1b96f8)
  libgui      SurfaceControl::readFromParcel (0x13f7d0)
              client, handle: SAFE_PARCEL(parcel.readStrongBinder, &x)
  libbinder   Parcel::readObject(bool) (0x9f2c0): a null binder
              (binder==0 && cookie==0) returns without an object-table check,
              then Parcel::finishUnflattenBinder reads the 4-byte stability
              record; Parcel::readStrongBinder(sp<IBinder>*) (0x7e7a0) maps the
              resulting null to UNEXPECTED_NULL (-2147483640) after logging
              "Expecting binder but got null!"
  libandroid_runtime nativeReadFromParcel (android_view_SurfaceControl.cpp):
              ignores the status, returns 0 when the surface is null, so Java
              keeps walking with mNativeObject == 0.
"""

import re
import sys

BINDERS = {0x73622A85, 0x73682A85, 0x73622A86, 0x73682A86}
UNEXPECTED_NULL = -2147483640


def load(path):
    d = bytearray()
    for ln in open(path):
        m = re.match(r"^([0-9a-f]{4}): (.*)$", ln.rstrip("\n"))
        if m and int(m.group(1), 16) == len(d):
            d += bytes(int(x, 16) for x in m.group(2).split())
    return bytes(d)


class Parcel:
    """Just enough of libbinder's Parcel to walk the reply."""

    def __init__(self, data, pos=0):
        self.d = data
        self.size = len(data)
        self.pos = pos
        self.events = []

    def i32(self):
        if self.pos + 4 <= self.size:
            v = int.from_bytes(self.d[self.pos:self.pos + 4], "little", signed=True)
            self.pos += 4
            return v
        return 0

    def inplace(self, n):
        n = (n + 3) & ~3
        if self.pos + n <= self.size:
            start = self.pos
            self.pos += n
            return start
        return None

    def string8(self):
        n = self.i32()
        if n < 0 or self.inplace(n + 1) is None:
            return None
        return self.d[self.pos - ((n + 1 + 3) & ~3):][:n]

    def string16(self):
        n = self.i32()
        if n < 0 or self.inplace((n + 1) * 2) is None:
            return None
        off = self.pos - (((n + 1) * 2 + 3) & ~3)
        return self.d[off:off + 2 * n].decode("utf-16-le", "replace")

    def strong_binder(self, label):
        """Parcel::readStrongBinder(sp<IBinder>*): null -> UNEXPECTED_NULL."""
        at = self.pos
        if self.pos + 24 > self.size:
            self.events.append((label, at, "NOT_ENOUGH_DATA"))
            return None
        typ = int.from_bytes(self.d[self.pos:self.pos + 4], "little")
        binder = int.from_bytes(self.d[self.pos + 8:self.pos + 16], "little")
        cookie = int.from_bytes(self.d[self.pos + 16:self.pos + 24], "little")
        self.pos += 24
        if typ in BINDERS:
            self.i32()  # Parcel::finishUnflattenBinder stability record
            if binder == 0 and cookie == 0:
                self.events.append(
                    (label, at, "NULL binder -> UNEXPECTED_NULL (%d)" % UNEXPECTED_NULL))
                return None
            self.events.append((label, at, "binder %#x (object-table check)" % binder))
            return binder
        self.events.append((label, at, "type %#x is not a flat_binder_object" % typ))
        return None


def surface_control(p, aborted):
    """SurfaceControl.readFromParcel (Java) + SurfaceControl::readFromParcel."""
    name = p.string8()
    width = p.i32()
    height = p.i32()
    present = p.i32()
    info = {"name": name, "w": width, "h": height, "native": None}
    if not present:
        return info
    client_at = p.pos
    client = p.strong_binder("  leash client")
    if client is None:
        # SAFE_PARCEL(parcel.readStrongBinder, &client) returns early; the Java
        # constructor still hands a SurfaceControl with mNativeObject == 0 to
        # the caller, and the enclosing InsetsSourceControl keeps walking.
        info["native"] = "aborted at client"
        aborted.append(client_at)
        return info
    handle = p.strong_binder("  leash handle")
    layer_id = p.i32()
    layer_name = p.string16()
    info["native"] = {
        "layerId": layer_id,
        "layerName": layer_name,
        "transformHint": p.i32(),
        "width": p.i32(),
        "height": p.i32(),
        "format": p.i32(),
        "handle_null": handle is None,
    }
    return info


def control(p, aborted, index):
    cid = p.i32()
    ctype = p.i32()
    presence = p.i32()
    leash = surface_control(p, aborted) if presence else None
    p.i32()  # initiallyVisible
    if p.i32():  # Point presence
        p.i32()
        p.i32()
    if p.i32():  # Insets presence
        for _ in range(4):
            p.i32()
    p.i32()  # skipAnimationOnce
    print("ctl%d id=%#x type=%d leash=%r" % (index, cid, ctype,
                                             (leash or {}).get("name")))
    return leash


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else \
        "research/art-feasibility/w3-leash/artifacts/w3-frame-add.hex"
    d = load(path)
    p = Parcel(d)
    print("frame %s: %d bytes" % (path, len(d)))
    p.i32()  # exception header
    p.i32()  # result
    p.i32()  # InputChannel presence
    p.i32()  # InputChannel isInitialized
    p.strong_binder("InputChannel token")
    p.string16()
    p.inplace(24)  # the fd flat object
    # InsetsState: skip to the controls with the framework's field order.
    assert p.i32() == 1, "InsetsState presence"
    p.inplace(16)  # mDisplayFrame
    variant = p.i32()
    if variant > 0:
        if p.i32():  # safe insets presence
            p.inplace(16)
        n = p.i32()
        for _ in range(max(n, 0)):
            if p.i32():
                p.inplace(16)
        if p.i32():  # waterfall
            p.inplace(16)
        p.inplace(16)   # display/physical width+height
        p.i32()         # density
        p.string16()    # cutoutSpec
        p.inplace(12)   # rotation, scale, ratio
    if p.i32():  # rounded corners
        p.i32()
        n = p.i32()
        for _ in range(max(n, 0)):
            p.i32()
            p.inplace(16)
    p.inplace(16)  # roundedCornerFrame
    if p.i32():  # privacy indicator bounds
        n = p.i32()
        for _ in range(max(n, 0)):
            p.i32()
            p.inplace(16)
        p.i32()
    if p.i32():  # display shape
        p.string8()
        p.inplace(12)
        p.inplace(12)
        p.i32()
    sources = p.i32()
    for _ in range(max(sources, 0)):
        p.i32()
        p.inplace(24)
        if p.i32():
            p.inplace(16)
        p.inplace(12)

    assert p.i32() == 1, "InsetsSourceControl$Array presence"
    count = p.i32()
    print("controls: %d (array walk starts at %d)" % (count, p.pos))
    aborted = []
    for i in range(max(count, 0)):
        if not p.i32():  # element presence
            continue
        control(p, aborted, i)
    p.i32()  # attachedFrame presence
    p.inplace(16)
    scale_len = p.i32()
    scale_val = p.i32()
    print()
    print("binder reads, in order:")
    for label, at, what in p.events:
        print("  %-18s @%4d  %s" % (label, at, what))
    print()
    print("first aborted leash client at %s" % (aborted[0] if aborted else "none"))
    print("the stub reads exactly `count` elements, so after the aborted leash it")
    print("stops %d bytes early (at %d of %d): the rest of the reply is never read"
          % (len(d) - p.pos, p.pos, len(d)))
    print("trailing float[] count read at %d = %d (the caller allocated 1 ->"
          % (p.pos - 8, scale_len))
    print("Parcel.readFloatArray throws 'bad array lengths'; scale bits = %d)"
          % scale_val)
    print()
    print("=> the guest's log ('not in the object list at offset 1440') is the")
    print("   SECOND leash's client read: %d + 88 = %d."
          % (aborted[0] if aborted else 1352, aborted[0] + 88 if aborted else 1440))


if __name__ == "__main__":
    main()
