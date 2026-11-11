#!/usr/bin/env python3
"""Resolve the Android compile/runtime classpath for the RN hello fixture.

Reads the already-populated Gradle module cache (read-only) and walks the Maven
POM graph from the React Native host artifacts. For each `group:artifact` we
prefer the exact version named by the parent POM (Maven semantics); if that
version is not cached we fall back to the highest cached version. This is a
small, offline, best-effort resolver, not Gradle.

It prints one absolute path per line (AAR or JAR) on stdout and a short summary
on stderr.

Roots outside the Gradle cache (the react-android / hermes-android AARs fetched
by `tools/build-rn-hello.sh fetch`) are supplied via ACOS_RN_AAR_DIR.
"""
from __future__ import annotations

import glob
import os
import re
import sys
from typing import Dict, List, Optional, Tuple

GCC = os.environ.get(
    "ACOS_GRADLE_CACHE",
    os.path.expanduser("~/.gradle/caches/modules-2/files-2.1"),
)
EXTRA_DIRS = [d for d in [os.environ.get("ACOS_RN_AAR_DIR", "")] if d]

# (group, artifact, version) starting points. Versions are pinned to the RN
# 0.81.4 dependency set so we never drift onto a cached 0.85/0.86 AAR.
# `react-android`'s POM marks `com.facebook.fbjni:fbjni` `optional`, so a Maven
# walk that skips optional deps (correct for transitive resolution) never pulls
# the fbjni `classes.jar`. A real Gradle app declares fbjni as a runtime
# dependency, so it is listed here explicitly as a root; otherwise the APK's DEX
# omits `com.facebook.jni.*` and `libfbjni.so`'s `JNI_OnLoad` cannot run (see
# results/rn/FIRST_RUN.md).
#
# `javax.inject:javax.inject` is likewise `optional` in `react-android`'s POM,
# but RN's `MainReactPackage` desugared `invokedynamic` providers implement the
# `javax.inject.Provider` interface at runtime. Without it the DEX has only a
# type reference, and loading `MainReactPackage` fails with
# `NoClassDefFoundError: javax.inject.Provider` (W88-A).
ROOTS = [
    ("com.facebook.react", "react-android", "0.81.4"),
    ("com.facebook.react", "hermes-android", "0.81.4"),
    ("com.facebook.fbjni", "fbjni", "0.7.0"),
    ("javax.inject", "javax.inject", "1"),
    ("androidx.appcompat", "appcompat", "1.7.0"),
    ("androidx.swiperefreshlayout", "swiperefreshlayout", "1.1.0"),
    ("com.facebook.soloader", "soloader", "0.12.1"),
    # `com.facebook.infer.annotation` is `optional` in react-android's POM, so a
    # transitive walk skips it, but RN's bytecode calls its runtime helper
    # `Assertions.assertNotNull` (not just the annotations). Without it the DEX
    # keeps the reference and `CatalystInstanceImpl$Builder.build` aborts with
    # UNSUPPORTED_JAVA_API (W123-A). A real Gradle app pulls it in as a direct
    # dependency; list it here as an explicit root.
    ("com.facebook.infer.annotation", "infer-annotation", "0.18.0"),
    # Fresco is likewise `optional` in react-android's POM, but the RN bridge
    # and image view managers reference it at runtime (`FLog`, the Drawee /
    # ImagePipeline types). A real Gradle app adds these roots explicitly.
    ("com.facebook.fresco", "fresco", "3.6.0"),
    ("com.facebook.fresco", "imagepipeline-okhttp3", "3.6.0"),
    ("com.facebook.fresco", "middleware", "3.6.0"),
    # OkHttp is likewise `optional` in react-android's POM, but the Fresco module
    # that RN initializes on every native-modules queue uses it at runtime.
    # `okhttp3.JavaNetCookieJar` is NOT in the `okhttp` jar (4.9.2 or 4.12.0); it
    # lives in `okhttp-urlconnection`. Without that artifact the DEX keeps only
    # the type reference, `FrescoModule.initialize` throws
    # `NoClassDefFoundError: Failed resolution of: Lokhttp3/JavaNetCookieJar;` on
    # the mqt_native_modules thread, and every later UIManager/JS->native call is
    # posted to a dead queue (W23: ReactRootView stayed empty with JS running).
    # Both are pinned to the 4.9.2 the react-android 0.81.4 POM declares, so the
    # family and okio 2.9.0 stay mutually consistent (a cached 4.12.0 `okhttp`
    # core against okio 2.9.0 is not a combination RN ever resolves).
    ("com.squareup.okhttp3", "okhttp", "4.9.2"),
    ("com.squareup.okhttp3", "okhttp-urlconnection", "4.9.2"),
    # `androidx.activity:activity` declares `lifecycle-viewmodel-savedstate` as a
    # `compile` dependency, and `ComponentActivity.<init>` calls
    # `SavedStateHandleSupport.enableSavedStateHandles`. Without that artifact the
    # DEX has only a reference and the Activity cannot be constructed (W174-A).
    # The requested 2.6.1 and the highest cached 2.10.0 carry metadata only (the
    # 2.9+ modules are KMP `module`/`pom` with the binary split into the
    # `-android` variant), so pin the closest cached version whose main artifact
    # actually carries the classes. The general fallback below is also fixed to
    # never drop a subtree when the highest cached version has no binary.
    ("androidx.lifecycle", "lifecycle-viewmodel-savedstate", "2.6.2"),
]

DEP_RE = re.compile(r"<dependency>(.*?)</dependency>", re.S)


def _ver_key(v: str) -> Tuple:
    return tuple((0, int(p)) if p.isdigit() else (1, p) for p in re.split(r"[.\-+]", v))


def _cache_versions(g: str, a: str) -> List[str]:
    base = os.path.join(GCC, g, a)
    if not os.path.isdir(base):
        return []
    return [d for d in os.listdir(base) if os.path.isdir(os.path.join(base, d))]


def _extra_files(g: str, a: str, v: str) -> Tuple[Optional[str], Optional[str]]:
    for d in EXTRA_DIRS:
        if not os.path.isdir(d):
            continue
        poms = glob.glob(os.path.join(d, f"{a}-{v}.pom"))
        arts = []
        for ext in ("aar", "jar"):
            arts += glob.glob(os.path.join(d, f"{a}-{v}*.{ext}"))
        if arts:
            return (poms[0] if poms else None), arts[0]
    return None, None


def _cache_files(g: str, a: str, v: str) -> Tuple[Optional[str], Optional[str]]:
    base = os.path.join(GCC, g, a, v)
    if not os.path.isdir(base):
        return None, None
    poms = glob.glob(os.path.join(base, "*", "*.pom"))
    arts: List[str] = []
    for ext in ("aar", "jar"):
        arts += glob.glob(os.path.join(base, "*", "*." + ext))
    return (poms[0] if poms else None), (arts[0] if arts else None)


def files_for(g: str, a: str, v: str) -> Tuple[Optional[str], Optional[str]]:
    pom, art = _extra_files(g, a, v)
    if art:
        return pom, art
    return _cache_files(g, a, v)


def _parse_deps(pom: str):
    txt = open(pom, encoding="utf-8", errors="replace").read()
    txt = re.sub(r"<dependencyManagement>.*?</dependencyManagement>", "", txt, flags=re.S)
    for m in DEP_RE.finditer(txt):
        b = m.group(1)
        g = re.search(r"<groupId>(.*?)</groupId>", b)
        a = re.search(r"<artifactId>(.*?)</artifactId>", b)
        v = re.search(r"<version>(.*?)</version>", b)
        sc = re.search(r"<scope>(.*?)</scope>", b)
        opt = re.search(r"<optional>(.*?)</optional>", b)
        if not (g and a and v):
            continue
        scope = (sc.group(1).strip() if sc else "compile")
        if scope not in ("compile", "runtime"):
            continue
        if opt and opt.group(1).strip() == "true":
            continue
        ver = v.group(1).strip().strip("[]")
        yield g.group(1).strip(), a.group(1).strip(), ver


def resolve() -> List[Tuple[str, str, str]]:
    chosen: Dict[Tuple[str, str], str] = {}
    work = list(ROOTS)
    while work:
        g, a, v = work.pop(0)
        ga = (g, a)
        if ga in chosen:
            continue
        pom, art = files_for(g, a, v)
        if art is None:
            # Exact version has no cached binary. Fall back to the highest
            # cached version that actually carries an AAR/JAR. Picking the
            # mere highest version can silently drop the whole subtree when the
            # cache entry is metadata-only (e.g. AndroidX 2.9+ KMP modules,
            # whose base coordinate has no binary and whose `-android` variant
            # holds it).
            for cand in sorted(_cache_versions(g, a), key=_ver_key, reverse=True):
                pom, art = _cache_files(g, a, cand)
                if art:
                    v = cand
                    break
            else:
                continue
        chosen[ga] = v
        if pom:
            for d in _parse_deps(pom):
                if (d[0], d[1]) not in chosen:
                    work.append(d)
    return [(g, a, v) for (g, a), v in sorted(chosen.items())]


def main() -> int:
    out: List[str] = []
    missing: List[str] = []
    for g, a, v in resolve():
        pom, art = files_for(g, a, v)
        if art:
            out.append(art)
        else:
            missing.append(f"{g}:{a}:{v}")
    for line in out:
        print(line)
    print(
        f"rn-hello-deps: resolved {len(out)} artifacts; {len(missing)} without a file",
        file=sys.stderr,
    )
    for m in missing:
        print("  no file:", m, file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
