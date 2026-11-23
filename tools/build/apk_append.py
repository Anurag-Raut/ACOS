#!/usr/bin/env python3
"""Append files/directories to an existing zip/APK without adding entries
that would interfere with APK signing (no synthetic META-INF/MANIFEST.MF).

Usage:
    apk_append.py <apk> <src>[:<arcname>] [<src>[:<arcname>] ...]

For a directory <src>, every file below it is added with its path relative to
<src> (prefixed by <arcname> when given). For a file, <arcname> defaults to the
basename. ``:`` in an argument is treated as the separator; paths are otherwise
used verbatim.
"""
import os
import sys
import zipfile


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    apk = sys.argv[1]
    if not os.path.isfile(apk):
        print(f"apk_append: no such APK: {apk}", file=sys.stderr)
        return 2
    with zipfile.ZipFile(apk, "a", zipfile.ZIP_DEFLATED) as z:
        for arg in sys.argv[2:]:
            src, sep, arc = arg.partition(":")
            if not os.path.exists(src):
                print(f"apk_append: no such source: {src}", file=sys.stderr)
                return 2
            if os.path.isdir(src):
                for root, _dirs, files in os.walk(src):
                    for f in sorted(files):
                        full = os.path.join(root, f)
                        rel = os.path.relpath(full, src)
                        name = os.path.join(arc, rel) if sep and arc else rel
                        z.write(full, name.replace(os.sep, "/"))
            else:
                name = arc if sep and arc else os.path.basename(src)
                z.write(src, name.replace(os.sep, "/"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
