#!/usr/bin/env python3
"""Resolve the AndroidX closure needed by Flutter's Android embedding.

The prebuilt `flutter.jar` from the official engine artifacts references
AndroidX classes but does not bundle them. This helper reads the local Gradle
module cache (read-only), takes the newest artifact for each module, extracts
each AAR's classes.jar, and merges the class files into one `androidx.jar`
with unique entries. That jar is used both as a javac classpath and as a d8
program input.

Usage:
    flutter_androidx.py <output.jar> [--gradle-cache DIR]

Exit codes: 0 success, 2 missing cache/module.
"""
import argparse
import os
import sys
import zipfile

# Modules referenced by flutter.jar's embedding (jdeps on Flutter 3.47.9).
MODULES = [
    "androidx.activity/activity",
    "androidx.core/core",
    "androidx.core/core-viewtree",
    "androidx.exifinterface/exifinterface",
    "androidx.fragment/fragment",
    "androidx.lifecycle/lifecycle-runtime-android",
    "androidx.lifecycle/lifecycle-common-jvm",
    "androidx.lifecycle/lifecycle-viewmodel-android",
    "androidx.lifecycle/lifecycle-livedata",
    "androidx.lifecycle/lifecycle-livedata-core",
    "androidx.lifecycle/lifecycle-process",
    "androidx.lifecycle/lifecycle-service",
    "androidx.tracing/tracing",
    "androidx.window/window",
    "androidx.window/window-core-android",
    "androidx.annotation/annotation-jvm",
    "androidx.annotation/annotation-experimental",
    "androidx.collection/collection-jvm",
    "androidx.arch.core/core-runtime",
    "androidx.arch.core/core-common",
    "androidx.savedstate/savedstate-android",
    "androidx.savedstate/savedstate-android",
    "androidx.versionedparcelable/versionedparcelable",
    # Kotlin runtime needed by the Kotlin-authored AndroidX classes above
    # (androidx.lifecycle.LifecycleRegistry calls kotlin.jvm.internal.Intrinsics).
    "org.jetbrains.kotlin/kotlin-stdlib",
    "org.jetbrains.kotlinx/kotlinx-coroutines-core-jvm",
    "org.jetbrains.kotlinx/kotlinx-coroutines-android",
    # Native-library loader used by FlutterLoader (not in the local Gradle cache;
    # fetched separately into .cache/flutter-1/relinker-maven/).
    "com.getkeepsafe.relinker/relinker",
]


def version_key(version: str):
    # sort "2.10.0" above "2.9.3"; numeric components first, then text.
    parts = []
    for p in version.replace("-", ".").split("."):
        try:
            parts.append((0, int(p)))
        except ValueError:
            parts.append((1, p))
    return parts


def find_artifact(caches, module: str):
    group, name = module.split("/", 1)
    for cache in caches:
        base = os.path.join(cache, group, name)
        if not os.path.isdir(base):
            continue
        versions = [v for v in os.listdir(base) if os.path.isdir(os.path.join(base, v))]
        if not versions:
            continue
        versions.sort(key=version_key, reverse=True)
        for version in versions:
            vdir = os.path.join(base, version)
            candidates = []
            for root, _dirs, files in os.walk(vdir):
                for f in files:
                    if f.endswith((".aar", ".jar")):
                        candidates.append(os.path.join(root, f))
            if candidates:
                candidates.sort(key=lambda p: (p.endswith(".jar"), os.path.getsize(p)))
                return candidates[0], version, cache
    return None


def merge(output: str, caches):
    staged = []
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as zo:
        seen = set()
        for module in MODULES:
            found = find_artifact(caches, module)
            if not found:
                print(f"flutter_androidx: missing module in caches: {module}", file=sys.stderr)
                return 2
            path, version, cache = found
            staged.append((module, version, path))
            jars = []
            if path.endswith(".aar"):
                with zipfile.ZipFile(path) as za:
                    try:
                        data = za.read("classes.jar")
                    except KeyError:
                        # Newer AndroidX "stub" AARs (e.g. savedstate 1.4.0)
                        # carry no classes; the -android sibling does.
                        print(f"flutter_androidx: skipping AAR without classes.jar: {path}", file=sys.stderr)
                        continue
                tmp = output + ".classes.tmp"
                with open(tmp, "wb") as f:
                    f.write(data)
                jars.append(tmp)
            else:
                jars.append(path)
            for jar in jars:
                with zipfile.ZipFile(jar) as zi:
                    for n in zi.namelist():
                        if n.endswith("/") or n.startswith("META-INF/") or n in seen:
                            continue
                        seen.add(n)
                        zo.writestr(n, zi.read(n))
                if jar.endswith(".classes.tmp"):
                    os.unlink(jar)
    for module, version, path in staged:
        print(f"  {module} {version} <- {os.path.relpath(path, cache)}")
    print(f"flutter_androidx: wrote {output} ({os.path.getsize(output)} bytes, {len(seen)} entries)")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("output")
    ap.add_argument("--gradle-cache", default=os.path.expanduser("~/.gradle/caches/modules-2/files-2.1"))
    args = ap.parse_args()
    if not os.path.isdir(args.gradle_cache):
        print(f"flutter_androidx: no Gradle cache at {args.gradle_cache}", file=sys.stderr)
        return 2
    # Extra local cache for artifacts that are not in the Gradle cache
    # (currently ReLinker); sits next to the generated androidx.jar.
    extra = os.path.join(os.path.dirname(os.path.abspath(args.output)), "relinker-maven")
    caches = [args.gradle_cache]
    if os.path.isdir(extra):
        caches.append(extra)
    return merge(args.output, caches)


if __name__ == "__main__":
    raise SystemExit(main())
