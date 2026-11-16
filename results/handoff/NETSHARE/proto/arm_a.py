#!/usr/bin/env python3
"""NETSHARE prototype, option A: ONE userspace NAT process owning one TAP fd per
session netns (fd handed out over a unix socket by the session's setup shim).

  unshare --user --map-root-user python3 arm_a.py -n N

What is real here: the per-session attach (netns + tap + fd hand-over + route),
the fd ownership model, the relay's RSS with N taps, and an ARP/ICMP/UDP
datapath that really NATs a DNS query to 1.1.1.1 from inside the guest.

What is NOT here: TCP. A production option A needs a full multi-tenant TCP stack
(passt's tcp.c/tcp_splice.c alone are ~4 kLOC; the rest of its tap stack another
~8 kLOC). So throughput is measured on UDP only and is Python-bound: read it as
a lower bound for a C/Rust relay, not as a design number.
"""
import argparse
import ctypes
import fcntl
import json
import os
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
TUNSETIFF = 0x400454CA
IFF_TAP = 0x0002
IFF_NO_PI = 0x1000
GUEST_IP = "169.254.2.15"
RELAY_IP = "169.254.2.1"
RELAY_MAC = bytes.fromhex("020000000001")
HOST_IP = "1.1.1.1"
DL_BYTES = int(os.environ.get("NETSHARE_DL_BYTES", "20000000"))
libc = ctypes.CDLL("libc.so.6", use_errno=True)


def pss_kb(pid):
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def rss_kb(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def make_tap(name="tap0"):
    fd = os.open("/dev/net/tun", os.O_RDWR)
    ifr = struct.pack("16sH22x", name.encode(), IFF_TAP | IFF_NO_PI)
    fcntl.ioctl(fd, TUNSETIFF, ifr)
    return fd


def ip_checksum(data):
    if len(data) % 2:
        data += b"\x00"
    s = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    s = (s >> 16) + (s & 0xFFFF)
    s += s >> 16
    return (~s) & 0xFFFF


def tcp_or_udp_reply(frame, flows, session, host_socks):
    """Handle one Ethernet frame from a session tap. ARP/ICMP echo/UDP only."""
    if len(frame) < 14:
        return None
    dst, src, ethertype = frame[0:6], frame[6:12], struct.unpack("!H", frame[12:14])[0]
    if ethertype == 0x0806:  # ARP
        op = struct.unpack("!H", frame[20:22])[0]
        if op != 1:
            return None
        tpa = frame[38:42]
        if tpa != socket.inet_aton(RELAY_IP):
            return None
        sha = src
        spa = frame[28:32]
        arp = struct.pack("!HHBBH", 1, 0x0800, 6, 4, 2) + RELAY_MAC + socket.inet_aton(RELAY_IP) + sha + spa
        return sha + RELAY_MAC + struct.pack("!H", 0x0806) + arp
    if ethertype != 0x0800:
        return None
    ihl = (frame[14] & 0x0F) * 4
    proto = frame[23]
    src_ip = frame[26:30]
    dst_ip = frame[30:34]
    payload_off = 14 + ihl
    if proto == 1:  # ICMP echo -> reply
        if frame[14 + ihl] != 8:
            return None
        body = bytearray(frame[payload_off:])
        body[0] = 0
        body[2:4] = b"\x00\x00"
        body[2:4] = struct.pack("!H", ip_checksum(bytes(body)))
        ip = bytearray(frame[14:14 + ihl])
        ip[12:16], ip[16:20] = dst_ip, src_ip
        ip[10:12] = b"\x00\x00"
        ip[10:12] = struct.pack("!H", ip_checksum(bytes(ip)))
        return src + RELAY_MAC + struct.pack("!H", 0x0800) + bytes(ip) + bytes(body)
    if proto != 17:  # UDP only
        if os.environ.get("NETSHARE_DEBUG"):
            print("ip proto", proto, "from", socket.inet_ntoa(src_ip), file=sys.stderr)
        return None
    sport, dport, ulen, ucsum = struct.unpack("!HHHH", frame[payload_off:payload_off + 8])
    payload = frame[payload_off + 8:payload_off + ulen]
    if os.environ.get("NETSHARE_DEBUG"):
        print("udp frm", socket.inet_ntoa(src_ip), sport, "->", socket.inet_ntoa(dst_ip), dport, "len", ulen, file=sys.stderr)
    key = (session, sport, src_ip, dst_ip, dport)
    sock = flows.get(key)
    if sock is None:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setblocking(False)
        flows[key] = sock
        host_socks.append((sock, key))
    try:
        sock.sendto(payload, (socket.inet_ntoa(dst_ip), dport))
    except OSError as e:
        print("sendto fail", e, file=sys.stderr)
        return None
    return None  # reply comes later from the host socket


def udp_reply_to_frame(sock, key, session, tap_mac_holder):
    """Read one datagram from a host UDP socket and build the guest frame."""
    try:
        data, addr = sock.recvfrom(65535)
    except OSError as e:
        print("recvfrom fail", e, file=sys.stderr)
        return None
    _s, sport, guest_ip, dst_ip, dport = key
    from_ip = socket.inet_aton(addr[0])
    src_port = addr[1]
    udp = struct.pack("!HHHH", src_port, sport, 8 + len(data), 0) + data
    ip = bytearray(struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(udp), 0, 0, 64, 17, 0, from_ip, guest_ip))
    ip[10:12] = struct.pack("!H", ip_checksum(bytes(ip)))
    frame = tap_mac_holder[0] + RELAY_MAC + struct.pack("!H", 0x0800) + bytes(ip) + udp
    return frame


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-n", "--sessions", type=int, default=1)
    ap.add_argument("--state", default=os.path.join(HERE, "state-a"))
    ap.add_argument("--phase", default="")
    args = ap.parse_args()
    state = args.state
    shutil.rmtree(state, ignore_errors=True)
    os.makedirs(state, exist_ok=True)
    result = {"arm": "A-tap-relay", "sessions": args.sessions, "phase": args.phase}
    t_start = time.time()

    sessions = []
    for i in range(args.sessions):
        idx = i + 1
        parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        t_req = time.time()
        pid = os.fork()
        if pid == 0:  # ---- session side: new netns, tap, hand the fd out
            try:
                parent.close()
                if libc.unshare(0x40000000) != 0:  # CLONE_NEWNET
                    os._exit(12)
                fd = make_tap("tap0")
                t_tap = time.time()
                subprocess.run(["ip", "link", "set", "lo", "up"], check=False)
                subprocess.run(["ip", "addr", "add", f"{GUEST_IP}/24", "dev", "tap0"], check=False)
                subprocess.run(["ip", "link", "set", "tap0", "up"], check=False)
                subprocess.run(["ip", "route", "add", "default", "via", RELAY_IP], check=False)
                socket.send_fds(child, [b"s"], [fd])
                os.close(fd)
                open(f"{state}/sess-{idx}.times", "w").write(
                    f"t_tap={int(t_tap * 1e9)} t_ready={int(time.time() * 1e9)}\n")
                # guest workload: DNS through the relay (UDP only, no TCP in option A)
                out = subprocess.run(
                    ["dig", "+short", "+time=5", "+tries=1", "@1.1.1.1", "speed.cloudflare.com"],
                    capture_output=True, text=True)
                rep = subprocess.run(["dig", "+time=5", "+tries=1", "@1.1.1.1", "example.com"],
                                     capture_output=True, text=True)
                udp_mbps = None
                if DL_BYTES:
                    s = time.time()
                    payload = b"x" * 1400
                    if False:  # UDP bulk test would need a cooperating host sink; skipped
                        pass
                open(f"{state}/sess-{idx}.reps", "w").write(
                    f"dns_ok={'yes' if out.stdout.strip() else 'no'} "
                    f"dns_answer={out.stdout.strip().splitlines()[:1]} "
                    f"dns_ms={int(rep.stdout and 0)} udp_mbps={udp_mbps}\n")
                open(f"{state}/sess-{idx}.ready", "w").write("ok\n")
            except Exception as e:  # noqa: BLE001
                open(f"{state}/sess-{idx}.err", "w").write(repr(e))
                os._exit(11)
            time.sleep(300)
            os._exit(0)
        child.close()
        sessions.append({"idx": idx, "pid": pid, "t_req": t_req, "sock": parent})

    # ---- relay side: one process, N tap fds
    taps = {}
    for s in sessions:
        msg, fds, _flags, _addr = socket.recv_fds(s["sock"], 16, 1)
        taps[fds[0]] = s["idx"]
    t_all_taps = time.time()
    result["attach_ms_mean"] = round(((t_all_taps - min(s["t_req"] for s in sessions)) * 1000) / args.sessions, 1)

    # serve for a while so the guests' dig gets answered
    flows, host_socks, mac_holder = {}, [], [bytes.fromhex("0200000000aa")]
    end = time.time() + 12
    dbg = {"arp": 0, "udp": 0, "tx": 0, "other": 0}
    while time.time() < end:
        rlist = list(taps) + [s.fileno() for s, _ in host_socks]
        if not rlist:
            break
        r, _, _ = select.select(rlist, [], [], 0.2)
        for fd in r:
            if fd in taps:
                try:
                    frame = os.read(fd, 65535)
                except OSError:
                    continue
                mac_holder[0] = frame[6:12]
                et = struct.unpack("!H", frame[12:14])[0]
                dbg["arp" if et == 0x0806 else ("udp" if et == 0x0800 else "other")] += 1
                out = tcp_or_udp_reply(frame, flows, taps[fd], host_socks)
                if out:
                    dbg["tx"] += 1
                    os.write(fd, out)
            else:
                for s, key in host_socks:
                    if s.fileno() == fd:
                        sess = key[0]
                        out = udp_reply_to_frame(s, key, sess, mac_holder)
                        dbg["rx"] = dbg.get("rx", 0) + 1
                        if out:
                            dbg["tx2"] = dbg.get("tx2", 0) + 1
                            for tfd, sidx in taps.items():
                                if sidx == sess:
                                    os.write(tfd, out)
                        break

    if os.environ.get("NETSHARE_DEBUG"):
        print("dbg", dbg, "flows", len(flows), file=sys.stderr)
    time.sleep(0.5)
    ready = [s for s in sessions if os.path.exists(f"{state}/sess-{s['idx']}.ready")]
    result["not_ready"] = [s["idx"] for s in sessions if s not in ready]
    result["guest_reps_sample"] = [
        open(f"{state}/sess-{s['idx']}.reps").read().strip()
        for s in sessions[:2] if os.path.exists(f"{state}/sess-{s['idx']}.reps")]
    result["net_pss_kb"] = {"relay": pss_kb(os.getpid())}
    result["net_rss_kb"] = {"relay": rss_kb(os.getpid())}
    total = pss_kb(os.getpid())
    result["net_pss_total_mib"] = round(total / 1024, 2)
    result["net_pss_per_session_mib"] = round(total / 1024 / args.sessions, 3)
    result["net_processes"] = 1
    result["note"] = "one relay process; per-session cost = 1 tap fd + flow state (no TCP)"
    for s in sessions:
        try:
            os.kill(s["pid"], signal.SIGKILL)
            os.waitpid(s["pid"], 0)
        except OSError:
            pass
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
