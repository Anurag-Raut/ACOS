#!/usr/bin/env python3
"""Fetch the K1 real-world APK corpus from F-Droid.

Reads the F-Droid `index-v2.json`, picks one APK per curated package, downloads
it, verifies the sha256 the index advertises, and writes `manifest.json`.

The corpus is deliberately *data*: it lives outside the repository
(`$ACOS_K1_CACHE/corpus/k1`), is never committed, and nothing from it is
executed except by ACOS itself inside its sandbox.

Selection rules (deterministic, recorded in the manifest):

  * prefer a version whose `targetSdkVersion` is <= the ACOS framework API (34),
    so a failure is not confounded by "app targets a newer framework";
  * among those prefer the newest versionCode;
  * require the APK to have no native code, or to ship an `x86_64` ABI;
  * if the package publishes per-ABI split APKs, take the `x86_64` split;
  * skip anything larger than `--max-mb` (default 200).

Usage:
  tools/compat/fetch-corpus.py                 # download into the default cache
  tools/compat/fetch-corpus.py --list          # print the selection, no download
  tools/compat/fetch-corpus.py --only org.fdroid.fdroid,org.kde.kdeconnect_tp
"""

import argparse
import concurrent.futures as futures
import hashlib
import json
import os
import sys
import time
import urllib.request

REPO = "https://f-droid.org/repo"
INDEX_URL = REPO + "/index-v2.json"
UA = "acos-compat-harness/1.0 (+https://github.com/acos-project/acos)"
FRAMEWORK_API = 34

# package -> (toolkit/category tag, why it is in the corpus)
CORPUS = {
    # --- plain framework/appcompat Views, no native code -------------------
    "org.fdroid.fdroid": ("views-appcompat", "client app; AppCompat + RecyclerView + downloads service"),
    "org.fdroid.basic": ("views-appcompat", "minimal fork of the same; smaller first-screen surface"),
    "com.beemdevelopment.aegis": ("views-appcompat", "Java, Material, TOTP; encrypted prefs"),
    "org.quantumbadger.redreader": ("views-appcompat", "Java, Reddit client, list/WebView"),
    "com.fsck.k9": ("views-appcompat", "Kotlin, Room/SQLite, mail UI, many fragments"),
    "de.danoeh.antennapod": ("views-appcompat", "Java, Media3/ExoPlayer, Room, lists"),
    "org.kde.kdeconnect_tp": ("views-appcompat", "Kotlin, Material, device pairing"),
    "net.gsantner.markor": ("views-appcompat", "Kotlin, editor + file browser"),
    "org.billthefarmer.editor": ("views-plain", "tiny pure-Views app, smallest possible surface"),
    "com.github.muellerma.coffee": ("views-appcompat", "small Kotlin app, keep-screen-on"),
    "com.github.gotify": ("views-appcompat", "small Kotlin app, notifications + Room"),
    "org.secuso.privacyfriendlynotes": ("views-appcompat", "Java, SQLite notes"),
    "it.niedermann.owncloud.notes": ("views-appcompat", "Java, SQLite + sync"),
    "com.money.manager.ex": ("views-appcompat", "Kotlin, Room, complex forms/lists"),
    "me.zhanghai.android.files": ("views-appcompat", "Kotlin, Material, file browser"),
    # --- Material 3 / Compose ---------------------------------------------
    "com.aurora.store": ("material", "Kotlin, Material Components, store UI"),
    "com.perflyst.twire": ("material-webview", "Kotlin, Material, ExoPlayer + WebView"),
    "org.breezyweather": ("compose", "Kotlin, Jetpack Compose Material3 (weather)"),
    "com.nononsenseapps.feeder": ("compose", "Kotlin, Compose UI + Room (RSS)"),
    "com.sadellie.unitto": ("compose", "small Compose Material3 unit converter"),
    "com.ismartcoding.plain": ("compose", "Compose + Ktor + Room, chat/files/tools"),
    "io.element.android.x": ("compose", "Compose (Element X) + native libs, x86_64 split"),
    "com.dot.gallery": ("compose", "Compose gallery + native libs, x86_64 split"),
    "net.mullvad.mullvadvpn": ("compose-native", "Compose UI + WireGuard/rust native libs"),
    # --- Kotlin coroutines / modern stack --------------------------------
    "com.keylesspalace.tusky": ("views-appcompat", "Kotlin coroutines + Retrofit + Room"),
    "de.westnordost.streetcomplete": ("views-appcompat", "Kotlin, ViewModel/coroutines, big resource set"),
    "com.nextcloud.client": ("views-appcompat", "Java/Kotlin, sync service + WorkManager"),
    "io.github.muntashirakon.AppManager": ("views-appcompat", "Kotlin/Java, package manager powers, shells out"),
    # --- WebView-heavy ----------------------------------------------------
    "org.wikipedia": ("webview", "WebView article rendering + search"),
    "com.ichi2.anki": ("webview-native", "WebView card renderer + native sqlite/anki backend, x86_64 split"),
    # --- React Native -----------------------------------------------------
    "org.jellyfin.mobile": ("react-native", "RN 0.7x client; exercises the RN bridge path"),
    "net.cozic.joplin": ("react-native", "RN + SQLite + WebView markdown"),
    "com.libreweatherrn": ("react-native", "small RN weather app, targetSdk 30"),
    "de.tutao.calendar": ("react-native", "Tuta calendar, RN + native crypto"),
    # --- Flutter ----------------------------------------------------------
    "chat.fluffy.fluffychat": ("flutter", "Flutter matrix client (record only: no Flutter engine)"),
    # --- native libs / GL / games ----------------------------------------
    "com.shatteredpixel.shatteredpixeldungeon": ("game-canvas", "libGDX-ish Canvas game, x86_64 libs"),
    "com.unciv.app": ("game-gles", "libGDX GLES game, x86_64 libs"),
    "io.anuke.mindustry": ("game-gles", "Arc/GLES game, x86_64 libs"),
    "org.supertuxkart.stk": ("game-gles", "native GLES2 engine (C++/SDL), x86_64 libs"),
    "org.videolan.vlc": ("native-media", "native media engine, x86_64 split"),
    "com.termux": ("native-terminal", "native terminal emulator + linker stress"),
    "com.tailscale.ipn": ("native-vpn", "Kotlin UI + Go userspace netstack"),
}


def cache_root():
    return os.environ.get(
        "ACOS_K1_CACHE", "/home/Qui/Projects/acos/.cache/k1"
    )


def corpus_dir():
    return os.environ.get(
        "ACOS_K1_CORPUS", "/home/Qui/Projects/acos/.cache/corpus/k1"
    )


def fetch(url, dest, retries=3):
    last = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=120) as r, open(dest, "wb") as f:
                while True:
                    chunk = r.read(1 << 20)
                    if not chunk:
                        break
                    f.write(chunk)
            return
        except Exception as e:  # noqa: BLE001 - retry any transport error
            last = e
            time.sleep(1 + attempt * 2)
    raise SystemExit(f"download failed: {url}: {last}")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def version_rows(pkg_entry):
    for ver in pkg_entry["versions"].values():
        m = ver.get("manifest") or {}
        sdk = m.get("usesSdk") or {}
        yield {
            "file": ver["file"],
            "version_code": int(m.get("versionCode") or 0),
            "version_name": m.get("versionName") or "",
            "nativecode": m.get("nativecode") or [],
            "target_sdk": sdk.get("targetSdkVersion"),
            "min_sdk": sdk.get("minSdkVersion"),
            "manifest": m,
        }


def pick(pkg, rows, max_bytes, prefer_api=FRAMEWORK_API):
    def abi_ok(r):
        nc = r["nativecode"]
        return (not nc) or ("x86_64" in nc)

    def size_ok(r):
        return int(r["file"].get("size") or 0) <= max_bytes

    eligible = [r for r in rows if abi_ok(r) and size_ok(r)]
    if not eligible:
        return None, "no x86_64/no-native APK under the size cap"

    def rank(r):
        # targetSdk <= framework API first, then newest versionCode, then the
        # narrower (x86_64-only) split over a fat APK.
        at_or_below = (r["target_sdk"] or 0) <= prefer_api
        split_only = r["nativecode"] == ["x86_64"]
        return (at_or_below, r["version_code"], split_only)

    best = max(eligible, key=rank)
    if not best["nativecode"]:
        why = "no native code"
    elif best["nativecode"] == ["x86_64"]:
        why = "x86_64 split"
    else:
        why = "fat APK with x86_64"
    return best, why


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--only", default=None, help="comma separated package filter")
    ap.add_argument("--max-mb", type=int, default=200)
    ap.add_argument("--jobs", type=int, default=4)
    args = ap.parse_args()

    root = corpus_dir()
    apks = os.path.join(root, "apks")
    os.makedirs(apks, exist_ok=True)
    index_path = os.path.join(root, "index-v2.json")
    if not os.path.isfile(index_path):
        print(f"fetching {INDEX_URL}", file=sys.stderr)
        fetch(INDEX_URL, index_path)
    with open(index_path) as f:
        index = json.load(f)["packages"]

    only = set(args.only.split(",")) if args.only else None
    selected, skipped = {}, {}
    for pkg, (tag, note) in CORPUS.items():
        if only and pkg not in only:
            continue
        entry = index.get(pkg)
        if not entry:
            skipped[pkg] = "absent from this F-Droid index"
            continue
        rows = list(version_rows(entry))
        best, why = pick(pkg, rows, args.max_mb * 1_000_000)
        if not best:
            skipped[pkg] = why
            continue
        selected[pkg] = (tag, note, best, why)

    for pkg, (tag, note, best, why) in sorted(selected.items()):
        name = best["file"]["name"].lstrip("/")
        mb = int(best["file"].get("size") or 0) / 1e6
        print(
            f"{pkg:42s} {best['version_name'][:18]:18s} vc={best['version_code']:<8d} "
            f"{mb:7.2f}MB tgt={best['target_sdk']} min={best['min_sdk']} "
            f"abis={best['nativecode'] or ['none']} [{why}] {tag}"
        )
    if args.list:
        return

    def download(item):
        pkg, (tag, note, best, why) = item
        remote = best["file"]["name"].lstrip("/")
        ver = best["version_name"] or str(best["version_code"])
        dest = os.path.join(apks, f"{pkg}_{best['version_code']}_{ver}.apk".replace("/", "-"))
        want = best["file"]["sha256"]
        if os.path.isfile(dest) and sha256(dest) == want:
            digest = want
        else:
            fetch(f"{REPO}/{remote}", dest)
            digest = sha256(dest)
            if digest != want:
                os.unlink(dest)
                raise SystemExit(f"sha256 mismatch for {pkg}: {digest} != {want}")
        return {
            "name": os.path.basename(dest),
            "package": pkg,
            "version": ver,
            "version_code": best["version_code"],
            "sha256": digest,
            "size_bytes": os.path.getsize(dest),
            "source_url": f"{REPO}/{remote}",
            "ui_toolkit": tag,
            "why": note,
            "has_native": bool(best["nativecode"]),
            "abis": best["nativecode"],
            "abi_fit": why,
            "target_sdk": best["target_sdk"],
            "min_sdk": best["min_sdk"],
            "src_tarball": (best["file"].get("src") or {}).get("name"),
        }

    started = time.time()
    rows, failures = [], []
    with futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futs = {pool.submit(download, it): it[0] for it in sorted(selected.items())}
        for fut in futures.as_completed(futs):
            pkg = futs[fut]
            try:
                row = fut.result()
                rows.append(row)
                print(f"ok   {pkg} ({row['size_bytes']/1e6:.1f} MB)", file=sys.stderr)
            except SystemExit as e:
                failures.append({"package": pkg, "error": str(e)})
                print(f"FAIL {pkg}: {e}", file=sys.stderr)

    manifest = {
        "corpus": "K1 real-world F-Droid compatibility corpus",
        "framework_api": FRAMEWORK_API,
        "index_url": INDEX_URL,
        "index_sha256": sha256(index_path),
        "fetched_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "download_seconds": round(time.time() - started, 1),
        "apps": sorted(rows, key=lambda r: r["package"]),
        "skipped": skipped,
        "failures": failures,
    }
    out = os.path.join(root, "manifest.json")
    with open(out, "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    total = sum(r["size_bytes"] for r in rows) / 1e9
    print(f"\n{len(rows)} APKs, {total:.2f} GB, manifest {out}")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
