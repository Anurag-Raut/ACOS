#!/usr/bin/env python3
"""Extract the Binder IPC work-list from an ART method trace.

The method trace records every executed Java method, including the AIDL
generated `*$Stub$Proxy.<method>` wrappers (outgoing calls, one per binder
transaction) and the `*$Stub.onTransact` + handler methods (incoming calls).
This script reconstructs per-thread call stacks to attribute incoming handler
methods and emits a TSV of the binder work-list.

It also reports non-binder channel activity (InputChannel, Choreographer/vsync,
Surface/BLAST, SystemProperties reads, ashmem/memfd, sockets/pipes) by matching
method names in the trace.

CLI:
    binder_from_trace.py <trace> [--out TSV] [--channels-out TSV]
"""
import argparse
import sys
from collections import Counter

sys.path.insert(0, __import__("os").path.dirname(__file__))
from art_trace import parse_header, parse_records  # noqa: E402

# Local/transport methods that are not actual binder transactions.
NON_TX = {
    "<init>", "asBinder", "asInterface", "getMaxTransactionId", "onTransact",
    "accept", "apply", "test", "getInterfaceDescriptor", "getTransactionName",
    "onTransactInner", "dump", "dumpAsync", "handleMessage",
}


def is_transport(cls, meth):
    return meth in ("onTransact",) or meth.startswith("execTransact")


def is_stub_transport(cls, meth):
    return cls.endswith("$Stub") and meth == "onTransact"


def is_binder_transport(cls, meth):
    return cls == "android.os.Binder" and meth.startswith("execTransact")


def is_local(meth):
    return meth in NON_TX or meth.startswith("lambda$") or meth.startswith("access$")


def is_handler_candidate(cls, meth):
    # Utilities the AIDL stub calls before dispatching the real method.
    if cls.startswith(("android.os.Parcel", "android.os.Trace", "android.os.Binder",
                       "android.os.StrictMode", "android.os.SystemProperties",
                       "android.os.ServiceManager", "android.os.Build")):
        return False
    if cls.endswith("$Stub") or cls == "android.os.Binder":
        return False
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace")
    ap.add_argument("--out")
    ap.add_argument("--channels-out")
    ap.add_argument("--label", default=None)
    args = ap.parse_args()

    data = open(args.trace, "rb").read()
    threads, methods, start = parse_header(data)
    recs, _ = parse_records(data, start, methods, threads)

    outgoing = Counter()
    outgoing_first = {}
    incoming = Counter()
    incoming_first = {}
    incoming_handlers = Counter()
    stacks = {}
    order = 0

    for r in recs:
        mid = r["mid"]
        tid = r["tid"]
        if mid not in methods:
            continue
        cls, meth, sig = methods[mid]
        stack = stacks.setdefault(tid, [])
        if r["exit"]:
            if stack and stack[-1][0] == mid:
                stack.pop()
            continue
        order += 1
        # outgoing proxy transaction
        if cls.endswith("$Stub$Proxy") and not is_local(meth):
            iface = cls[: -len("$Stub$Proxy")]
            key = (iface, meth, sig)
            outgoing[key] += 1
            outgoing_first.setdefault(key, order)
        # incoming: AIDL stub transport (defines an interface)
        if is_stub_transport(cls, meth):
            iface = cls[: -len("$Stub")]
            key = (iface, meth)
            incoming[key] += 1
            incoming_first.setdefault(key, order)
            stack.append((mid, "stub", False))
            continue
        # binder transport entry (no interface name)
        if is_binder_transport(cls, meth):
            stack.append((mid, "binder", False))
            continue
        # handler = first real method called directly under a $Stub.onTransact
        if stack and stack[-1][1] == "stub" and not stack[-1][2] and is_handler_candidate(cls, meth):
            iface = methods[stack[-1][0]][0]
            iface = iface[: -len("$Stub")]
            handler = (iface, cls, meth)
            incoming_handlers[handler] += 1
            incoming_first.setdefault(handler, order)
            stack[-1] = (stack[-1][0], "stub", True)
        stack.append((mid, "normal", False))

    rows = []
    for (iface, meth, sig), cnt in outgoing.items():
        rows.append((outgoing_first[(iface, meth, sig)], "app->system", iface, meth, sig, cnt, "proxy"))
    for (iface, meth), cnt in incoming.items():
        rows.append((incoming_first[(iface, meth)], "system->app", iface, meth, "", cnt, "onTransact"))
    for (iface, cls, meth), cnt in incoming_handlers.items():
        rows.append((incoming_first[(iface, cls, meth)], "system->app", iface, cls + "." + meth, "", cnt, "handler"))
    rows.sort()

    if args.out:
        with open(args.out, "w") as f:
            f.write("order\tdirection\tinterface\tmethod\tsignature\tcount\tkind\n")
            for r in rows:
                f.write("%d\t%s\t%s\t%s\t%s\t%d\t%s\n" % r)
    print("== binder %s ==" % (args.label or args.trace))
    print("outgoing distinct transactions: %d" % len(outgoing))
    print("incoming distinct transports:  %d" % len(incoming))
    print("incoming handler methods:      %d" % len(incoming_handlers))
    print("outgoing interfaces: %s" % ", ".join(sorted({k[0] for k in outgoing})))

    # non-binder channels
    channel_patterns = {
        "InputChannel": ("InputChannel", "InputEventReceiver", "InputQueue", "InputEvent"),
        "vsync/Choreographer": ("Choreographer", "DisplayEventReceiver"),
        "Surface/BLAST/graphics": ("Surface", "BLASTBufferQueue", "SurfaceControl", "SurfaceSession",
                                   "HardwareRenderer", "RenderNode"),
        "SystemProperties": ("SystemProperties",),
        "ashmem/memfd": ("MemoryFile", "SharedMemory", "Ashmem", "Memfd"),
        "socket/pipe": ("LocalSocket", "ParcelFileDescriptor", "UnixDomainSocket"),
        "Binder internals": ("Binder", "BinderProxy", "BinderInternal", "Parcel"),
    }
    channel_hits = {}
    for mid, (cls, meth, sig) in methods.items():
        for ch, pats in channel_patterns.items():
            if any(p in cls for p in pats):
                channel_hits.setdefault(ch, set()).add(cls + "." + meth)
    if args.channels_out:
        with open(args.channels_out, "w") as f:
            f.write("channel\tmethod\n")
            for ch in sorted(channel_hits):
                for m in sorted(channel_hits[ch]):
                    f.write("%s\t%s\n" % (ch, m))
    return rows


if __name__ == "__main__":
    main()
