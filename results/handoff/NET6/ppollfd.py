#!/usr/bin/env python3
"""NET6: read a blocked guest thread's `ppoll` pollfd out of /proc and resolve
the descriptor to a socket inode (which `ss` then names).

    ppollfd.py <pid> <tid>

Prints the syscall line twice (2 s apart), the pollfd array decoded, the fd's
`/proc/<pid>/fd` target, and every socket fd of the process with its inode.
"""
import os
import struct
import sys
import time


def syscall(pid, tid):
    try:
        with open(f"/proc/{pid}/task/{tid}/syscall") as f:
            return f.read().strip()
    except OSError as exc:
        return f"<unreadable: {exc}>"


def read_mem(pid, addr, n):
    with open(f"/proc/{pid}/mem", "rb", buffering=0) as m:
        m.seek(addr)
        return m.read(n)


def fd_target(pid, fd):
    try:
        return os.readlink(f"/proc/{pid}/fd/{fd}")
    except OSError as exc:
        return f"<{exc}>"


def main():
    pid, tid = int(sys.argv[1]), int(sys.argv[2])
    first = syscall(pid, tid)
    print("syscall:", first)
    parts = first.split()
    if len(parts) >= 3 and parts[0] == "271":  # ppoll
        fds_ptr = int(parts[1], 16)
        nfds = int(parts[2], 16)
        tmo_ptr = int(parts[3], 16)
        print(f"ppoll nfds={nfds} fds=0x{fds_ptr:x} timeout=0x{tmo_ptr:x}")
        try:
            raw = read_mem(pid, fds_ptr, 8 * nfds)
            for i in range(nfds):
                fd, events, revents = struct.unpack_from("ihh", raw, 8 * i)
                tgt = fd_target(pid, fd)
                inode = tgt.split("[")[1].rstrip("]") if "[" in tgt else "?"
                print(
                    f"  pollfd[{i}] fd={fd} events=0x{events:x} revents=0x{revents:x}"
                    f" -> {tgt}"
                )
                print(f"RESULT fd={fd} inode={inode}")
        except OSError as exc:
            print(f"  <pollfd read failed: {exc}>")
        try:
            sec, nsec = struct.unpack("<qq", read_mem(pid, tmo_ptr, 16))
            print(f"  timeout = {sec}s {nsec}ns")
        except OSError as exc:
            print(f"  <timeout read failed: {exc}>")
    print("--- fds ---")
    for name in sorted(os.listdir(f"/proc/{pid}/fd"), key=int):
        tgt = fd_target(pid, name)
        if "socket" in tgt or "dnsproxyd" in tgt:
            print(f"  fd={name} {tgt}")
    time.sleep(2)
    print("syscall after 2s:", syscall(pid, tid))


if __name__ == "__main__":
    main()
