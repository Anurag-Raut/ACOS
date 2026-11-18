#!/usr/bin/env python3
"""GLSPIKE3: quiet delta sampler for the guest app process.

Only processes whose /proc/<pid>/maps mentions the app's own APK path are
tracked. For each, the first appearance of each GL/Vulkan library is printed
once (the guest's own linker view), and a final per-pid union is printed.
"""
import glob
import sys
import time

WANT = ("libEGL_angle.so", "libGLESv2_angle.so", "libGLESv1_CM_angle.so",
        "vulkan.pastel.so", "libvulkan.so", "libhardware.so", "libgraphicsenv.so",
        "libEGL.so", "libGLESv2.so", "libvndksupport.so", "libshim")

deadline = time.time() + float(sys.argv[1] if len(sys.argv) > 1 else 60)
start = time.time()
first = {}
union = {}
samples = 0
while time.time() < deadline:
    for maps in glob.glob("/proc/[0-9]*/maps"):
        pid = maps.split("/")[2]
        try:
            text = open(maps, "r", errors="replace").read()
            cmd = open("/proc/%s/cmdline" % pid, "rb").read().replace(b"\0", b" ").decode(
                "utf-8", "replace")
        except OSError:
            continue
        if "dalvikvm" not in cmd or "acos" in cmd.split(" ")[0]:
            continue
        if pid not in union:
            print("pid=%s cmdline=%s" % (pid, cmd[:160]), flush=True)
        samples += 1
        libs = set()
        for line in text.splitlines():
            if "lib" not in line and "vulkan" not in line:
                continue
            slash = line.find("/")
            if slash < 0:
                continue
            libs.add(line[slash:].strip())
        union.setdefault(pid, set()).update(libs)
        for want in WANT:
            if any(Path.endswith(want) or want in Path for Path in libs):
                key = (pid, want)
                if key not in first:
                    first[key] = time.time() - start
                    print("%7.3f pid=%s first %s" % (first[key], pid, want), flush=True)
    time.sleep(0.01)

print("sampler samples=%d pids=%s elapsed=%.3f" % (samples, sorted(union), time.time() - start))
for pid, libs in sorted(union.items()):
    gl = sorted(p for p in libs if any(w in p for w in WANT))
    print("pid=%s gl-libs=%d %s" % (pid, len(gl), " ".join(gl)))
