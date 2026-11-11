#!/usr/bin/env python3
"""Print the launch guest's host pid: the descendant of the acos-art launcher
whose argv[0] is the image's dalvikvm64."""
import os, sys

def read(path, mode='r'):
    try:
        with open(path, mode) as f:
            return f.read()
    except OSError:
        return '' if mode == 'r' else b''

def ppid_of(pid):
    for ln in read('/proc/%d/status' % pid).splitlines():
        if ln.startswith('PPid:'):
            return int(ln.split()[1])
    return 0

root = int(sys.argv[1])
kids = {}
for d in os.listdir('/proc'):
    if d.isdigit():
        p = int(d)
        kids.setdefault(ppid_of(p), []).append(p)
stack = [root]
while stack:
    cur = stack.pop()
    for k in kids.get(cur, []):
        argv0 = read('/proc/%d/cmdline' % k, 'rb').split(b'\0')[0].decode('latin1')
        if argv0 == '/system/apex/com.android.art/bin/dalvikvm64':
            print(k)
            sys.exit(0)
        stack.append(k)
sys.exit(1)
