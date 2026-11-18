#!/usr/bin/env bash
#
# Build the React Native "hello" fixture (dev.acos.fixtures.rnhello) without
# Gradle.
#
# This is the RN counterpart of tools/build-apps.sh: it uses the same
# aapt2 + javac + d8 + zipalign + apksigner toolchain, but the classpath and the
# resources come from prebuilt Maven artifacts (react-android + hermes-android
# AARs and their AndroidX closure), and the JS bundle is produced by Metro and
# compiled to Hermes bytecode with the hermesc that ships in the react-native
# npm package.
#
# Nothing is fetched here unless `fetch` is passed. All downloads and caches
# live under .cache/rn-0.81.4/ (gitignored), in their own directories.
#
# Usage:
#   tools/build-rn-hello.sh            # build (assumes cache; warns otherwise)
#   tools/build-rn-hello.sh fetch      # download AARs + npm deps + bundle JS
#   tools/build-rn-hello.sh bundle     # (re)build just the JS/Hermes bundle
#   tools/build-rn-hello.sh debug      # the METRO1 debug variant (see below)
#
# Output:
#   fixtures/build/apps/rn-hello.apk           (x86_64, release, signed)
#   fixtures/build/apps/rn-hello-arm64.apk     (arm64-v8a, phone reference)
#   fixtures/build/apps/rn-debug.apk           (x86_64, debug, dev support on)
#
# # The debug variant (METRO1)
#
# `debug` is the same app with the two things a Metro dev loop needs and the
# store-shaped release build must not have:
#
#   1. **no `assets/index.android.bundle`** — the dev-support manager fetches
#      the bundle from the Metro dev server at launch instead of loading the
#      embedded one;
#   2. `getUseDeveloperSupport() == true` — RN's `DevSupportManager`, which is
#      what fetches `index.bundle?platform=android&dev=true`, keeps the `/message`
#      websocket open and shows the redbox.
#
# Both are selected by the generated `BuildVariant` class (the template's
# `BuildConfig` stand-in). The Metro address is RN's own `debug_http_host`
# preference, defaulting to `localhost:8081` (`ACOS_RN_METRO_HOST` overrides);
# see `MainApplication.onCreate` for why it is not the stock-emulator alias.
# Standard fix: keep the address in one place — the pref the app writes — so a
# scenario can point the same APK at another port without a rebuild.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -z "${ACOS_GUARDED:-}" ]; then
    exec "$ROOT/tools/guarded.sh" --mem 6G --time 1800 -- \
        env ACOS_GUARDED=1 "$0" "$@"
fi

SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BT="$SDK/build-tools/$BUILD_TOOLS_VERSION"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
AAPT2="$BT/aapt2"
D8="$BT/d8"
ZIPALIGN="$BT/zipalign"
APKSIGNER="$BT/apksigner"

CACHE="$ROOT/.cache/rn-0.81.4"
RN_VERSION="0.81.4"
APP_DIR="$ROOT/fixtures/apps/rn-hello"
OUT_DIR="$ROOT/fixtures/build/apps"
KEYSTORE="$OUT_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"
JSPROJ="$CACHE/jsproject"
REACT_AAR="$CACHE/aar/react-android-${RN_VERSION}-release.aar"
HERMES_AAR="$CACHE/aar/hermes-android-${RN_VERSION}-release.aar"

die() { echo "build-rn-hello: error: $*" >&2; exit 1; }
log() { echo "build-rn-hello: $*"; }

# ---------------------------------------------------------------------------
# fetch: download the pinned Maven AARs and the npm JS build environment
# ---------------------------------------------------------------------------
fetch() {
    mkdir -p "$CACHE/aar" "$CACHE/npm" "$CACHE/rnjs" "$CACHE/jsproject"
    for u in \
        "https://repo.reactnative.dev/maven2/com/facebook/react/react-android/${RN_VERSION}/react-android-${RN_VERSION}-release.aar" \
        "https://repo.reactnative.dev/maven2/com/facebook/react/hermes-android/${RN_VERSION}/hermes-android-${RN_VERSION}-release.aar" \
        "https://repo.reactnative.dev/maven2/com/facebook/react/react-android/${RN_VERSION}/react-android-${RN_VERSION}.pom" \
        "https://repo.reactnative.dev/maven2/com/facebook/react/hermes-android/${RN_VERSION}/hermes-android-${RN_VERSION}.pom" ; do
        f="$CACHE/aar/$(basename "$u")"
        [ -f "$f" ] && continue
        log "fetch $(basename "$u")"
        curl -fsSL --max-time 900 -o "$f.part" "$u"
        mv "$f.part" "$f"
    done
    if [ ! -d "$CACHE/rnjs/package" ]; then
        log "fetch react-native npm package"
        (cd "$CACHE/npm" && npm pack "react-native@${RN_VERSION}" --ignore-scripts --silent)
        tar xzf "$CACHE"/npm/react-native-*.tgz -C "$CACHE/rnjs"
    fi
    if [ ! -d "$JSPROJ/node_modules" ]; then
        log "npm install (ignore-scripts) for the JS bundler"
        cp "$APP_DIR"/package.json "$JSPROJ/" 2>/dev/null || true
        (cd "$JSPROJ" && npm install --ignore-scripts --no-audit --no-fund \
            --cache "$CACHE/npm-cache" >/dev/null 2>&1)
    fi
    bundle
}

# ---------------------------------------------------------------------------
# bundle: Metro bundle -> hermesc bytecode
# ---------------------------------------------------------------------------
bundle() {
    [ -x "$CACHE/rnjs/package/sdks/hermesc/linux64-bin/hermesc" ] \
        || die "hermesc missing; run '$0 fetch'"
    [ -f "$JSPROJ/node_modules/react-native/index.js" ] \
        || die "jsproject node_modules missing; run '$0 fetch'"
    cp "$APP_DIR/index.js" "$APP_DIR/app.json" "$APP_DIR/babel.config.js" \
       "$APP_DIR/metro.config.js" "$JSPROJ/"
    mkdir -p "$JSPROJ/out"
    log "Metro bundle"
    (cd "$JSPROJ" && node node_modules/react-native/cli.js bundle \
        --platform android --dev false --minify false \
        --entry-file index.js \
        --bundle-output out/index.android.bundle \
        --assets-dest out/assets >/dev/null 2>&1)
    log "hermesc -> Hermes bytecode"
    "$CACHE/rnjs/package/sdks/hermesc/linux64-bin/hermesc" -O -emit-binary \
        -out "$JSPROJ/out/index.android.bundle.hbc" "$JSPROJ/out/index.android.bundle" \
        2>/dev/null
    ls -l "$JSPROJ/out/index.android.bundle" "$JSPROJ/out/index.android.bundle.hbc"
}

VARIANT=${1:-build}
DEBUG=0
APK_NAME=rn-hello.apk
case "$VARIANT" in
    fetch) fetch ;;
    bundle) bundle ;;
    build) ;;
    debug)
        DEBUG=1
        APK_NAME=rn-debug.apk
        ;;
    *) die "unknown command: $1" ;;
esac

# The Metro address the debug variant asks the dev server for. It is written
# into the app's `debug_http_host` preference (RN's documented knob), so the
# scenario can change it without a rebuild; `localhost` is what ACOS's
# host-port opt-in (the `adb reverse` equivalent) makes reachable.
METRO_HOST="${ACOS_RN_METRO_HOST:-localhost:8081}"

# ---------------------------------------------------------------------------
# preconditions
# ---------------------------------------------------------------------------
command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$REACT_AAR" ] || die "missing $REACT_AAR (run '$0 fetch')"
[ -f "$HERMES_AAR" ] || die "missing $HERMES_AAR (run '$0 fetch')"
if [ "$DEBUG" = 0 ]; then
    [ -f "$JSPROJ/out/index.android.bundle.hbc" ] || bundle
fi

rm -rf "$OUT_DIR/work-rn"
WORK="$OUT_DIR/work-rn"
mkdir -p "$WORK"/{compiled,gen,classes,dex,lib/x86_64,assets,stage/libs,stage/res}
mkdir -p "$OUT_DIR"

# ---------------------------------------------------------------------------
# 1. resolve + stage the dependency classpath and resources
# ---------------------------------------------------------------------------
log "resolving dependency closure"
ACOS_RN_AAR_DIR="$CACHE/aar" python3 "$ROOT/tools/rn-hello-deps.py" > "$WORK/deps.txt" 2>"$WORK/deps.log"
cat "$WORK/deps.log"
[ -s "$WORK/deps.txt" ] || die "dependency resolution produced nothing"

stage_dep() {
    local p="$1" idx="$2"
    case "$p" in
        *.aar)
            local d="$WORK/stage/${idx}"
            mkdir -p "$d"
            (cd "$d" && unzip -o -q "$p" 'classes.jar' 'res/*' 'AndroidManifest.xml' 2>/dev/null || true)
            [ -f "$d/classes.jar" ] && cp "$d/classes.jar" "$WORK/stage/libs/${idx}.jar"
            if [ -d "$d/res" ]; then
                rm -rf "$WORK/stage/res/${idx}"
                cp -r "$d/res" "$WORK/stage/res/${idx}"
            fi
            local pkg=""
            [ -f "$d/AndroidManifest.xml" ] && pkg="$(grep -oE 'package="[^"]+"' "$d/AndroidManifest.xml" | head -1 | sed 's/package="//;s/"//')"
            [ -n "$pkg" ] && echo "$pkg" >> "$WORK/stage/packages.txt"
            ;;
        *.jar)
            cp "$p" "$WORK/stage/libs/${idx}.jar"
            ;;
    esac
}
idx=0
while IFS= read -r dep; do
    # deps.txt paths are relative to $ROOT for the fetched AARs
    [ -f "$dep" ] || dep="$ROOT/$dep"
    [ -f "$dep" ] || die "dependency not found: $dep"
    stage_dep "$dep" "$idx"
    idx=$((idx+1))
done < "$WORK/deps.txt"

log "compiling resources (app + $(ls -1 "$WORK/stage/res" 2>/dev/null | wc -l) dependency res dirs)"
"$AAPT2" compile --dir "$APP_DIR/res" -o "$WORK/compiled/app.zip"
for r in "$WORK/stage/res"/*; do
    [ -d "$r" ] || continue
    out="$WORK/compiled/$(basename "$r").zip"
    "$AAPT2" compile --dir "$r" -o "$out" 2>/dev/null || true
done

EXTRA_PKGS="$(sort -u "$WORK/stage/packages.txt" 2>/dev/null | grep -E '^[A-Za-z_][A-Za-z0-9_.]*$' | paste -sd: -)"
log "aapt2 link (extra R packages: ${EXTRA_PKGS:-none})"
LINK_ARGS=(-o "$WORK/base.apk" -I "$ANDROID_JAR" --manifest "$APP_DIR/AndroidManifest.xml"
           --java "$WORK/gen" --min-sdk-version 26 --target-sdk-version 34 --auto-add-overlay)
[ -n "$EXTRA_PKGS" ] && LINK_ARGS+=(--extra-packages "$EXTRA_PKGS")
for z in "$WORK"/compiled/*.zip; do LINK_ARGS+=(-R "$z"); done
"$AAPT2" link "${LINK_ARGS[@]}"

# ---------------------------------------------------------------------------
# 2. compile Java (app + generated R)
# ---------------------------------------------------------------------------
mkdir -p "$WORK/gen/dev/acos/fixtures/rnhello"
if [ "$DEBUG" = 1 ]; then DEBUG_TRUE=true; else DEBUG_TRUE=false; fi
cat >"$WORK/gen/dev/acos/fixtures/rnhello/BuildVariant.java" <<EOF
package dev.acos.fixtures.rnhello;

/**
 * The template's \`BuildConfig\` stand-in, generated by tools/build-rn-hello.sh.
 *
 * \`DEBUG\` is the variant switch: with it \`getUseDeveloperSupport()\` is true
 * (React Native's dev-support manager: the bundle is fetched from Metro, the
 * redbox and the reload/reload-broadcast path exist) and the APK ships no
 * \`assets/index.android.bundle\`. \`METRO_HOST\` is the \`debug_http_host\` value
 * the debug variant writes before the bridge exists.
 */
public final class BuildVariant {
  public static final boolean DEBUG = ${DEBUG_TRUE};
  public static final String METRO_HOST = "${METRO_HOST}";

  private BuildVariant() {}
}
EOF
CP="$ANDROID_JAR:$(find "$WORK/stage/libs" -name '*.jar' | paste -sd: -)"
log "javac ($( [ "$DEBUG" = 1 ] && echo debug || echo release ) variant, metro host ${METRO_HOST})"
# shellcheck disable=SC2046
javac --release 11 -encoding UTF-8 -cp "$CP" -d "$WORK/classes" \
    $(find "$APP_DIR/src" "$WORK/gen" -name '*.java')
# jar the app classes for d8
(cd "$WORK/classes" && jar cf "$WORK/app-classes.jar" .)

# ---------------------------------------------------------------------------
# 3. dex everything
# ---------------------------------------------------------------------------
log "d8 (app + $(find "$WORK/stage/libs" -name '*.jar' | wc -l) dependency jars)"
# Merge the dependency jars into one, dropping duplicate entries (e.g.
# androidx.annotation-1.3.0.jar vs annotation-jvm-1.9.1.jar) which d8 rejects.
python3 - "$WORK/stage/libs" "$WORK/deps-merged.jar" <<'PY'
import os, sys, zipfile
libdir, out = sys.argv[1], sys.argv[2]
seen = set()
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zo:
    for fn in sorted(os.listdir(libdir), key=lambda s: (len(s), s)):
        if not fn.endswith(".jar"):
            continue
        with zipfile.ZipFile(os.path.join(libdir, fn)) as zi:
            for n in zi.namelist():
                if n.endswith("/") or n.startswith("META-INF/") or n in seen:
                    continue
                seen.add(n)
                zo.writestr(n, zi.read(n))
PY
D8_INPUTS=("$WORK/app-classes.jar" "$WORK/deps-merged.jar")
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" "${D8_INPUTS[@]}" 2>"$WORK/d8.log" || {
    tail -40 "$WORK/d8.log" >&2; die "d8 failed"; }
grep -iE "warning: .*(missing|defined multiple)" "$WORK/d8.log" | head -5 || true
ls -la "$WORK/dex"

# ---------------------------------------------------------------------------
# 4. assemble the APK (native libs + Hermes bundle + dex)
# ---------------------------------------------------------------------------
log "staging native libs"
unzip -o -j -q "$REACT_AAR"  'jni/x86_64/*' -d "$WORK/lib/x86_64"
unzip -o -j -q "$HERMES_AAR" 'jni/x86_64/*' -d "$WORK/lib/x86_64"
cp "$JSPROJ/out/index.android.bundle.hbc" "$WORK/assets/index.android.bundle"
ls -l "$WORK/lib/x86_64"

build_variant() {
    local abi="$1" abi_dir="$2" out="$3"
    local v="$WORK/variant-$abi"
    rm -rf "$v"; mkdir -p "$v/lib/$abi_dir" "$v/dex"
    cp "$WORK/base.apk" "$v/base.apk"
    if [ "$abi" = "x86_64" ]; then
        cp "$WORK/lib/x86_64/"*.so "$v/lib/$abi_dir/"
    else
        unzip -o -j -q "$REACT_AAR"  "jni/$abi_dir/*" -d "$v/lib/$abi_dir"
        unzip -o -j -q "$HERMES_AAR" "jni/$abi_dir/*" -d "$v/lib/$abi_dir"
    fi
    cp "$WORK/dex/"*.dex "$v/dex/"
    if [ "$DEBUG" = 1 ]; then
        # A debug APK has no assets/index.android.bundle: the dev-support
        # manager must fetch the bundle from Metro, and an embedded one would
        # silently win.
        python3 "$ROOT/tools/apk_append.py" "$v/base.apk" "$v/dex:." "$v/lib:lib"
    else
        python3 "$ROOT/tools/apk_append.py" "$v/base.apk" \
            "$v/dex:." "$v/lib:lib" "$WORK/assets:assets"
    fi
    rm -f "$out" "$v/aligned.apk"
    "$ZIPALIGN" -f 4 "$v/base.apk" "$v/aligned.apk"
    "$APKSIGNER" sign --ks "$KEYSTORE" --ks-pass "pass:$KEY_PASS" \
        --key-pass "pass:$KEY_PASS" --ks-key-alias "$KEY_ALIAS" \
        --v1-signing-enabled true --v2-signing-enabled true --v3-signing-enabled true \
        --out "$out" "$v/aligned.apk"
    "$APKSIGNER" verify --verbose "$out" >/dev/null
    log "[$abi] -> $out ($(stat -c '%s' "$out") bytes)"
}

# throwaway debug keystore
if [ ! -f "$KEYSTORE" ]; then
    log "generating throwaway debug keystore"
    keytool -genkeypair -keystore "$KEYSTORE" -storepass "$KEY_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 -deststoretype pkcs12 >/dev/null 2>&1
fi

build_variant x86_64 x86_64 "$OUT_DIR/$APK_NAME"

# optional arm64 reference variant for the physical phone (ARM CPU)
if [ "$DEBUG" = 1 ]; then
    :   # the debug variant is the emulator loop only (ACOS is x86_64)
elif [ "${ACOS_RN_ARM64:-1}" = "1" ]; then
    build_variant arm64 arm64-v8a "$OUT_DIR/rn-hello-arm64.apk"
fi

# ---------------------------------------------------------------------------
# 5. verify
# ---------------------------------------------------------------------------
log "badging:"
"$AAPT2" dump badging "$OUT_DIR/$APK_NAME" | sed 's/^/    /' | head -n 12
log "native libs in rn-hello.apk:"
unzip -l "$OUT_DIR/$APK_NAME" | grep -E "lib/.*\.so|assets/.*bundle" | awk '{print "    "$1" "$4}'
if [ "$DEBUG" = 1 ] && unzip -l "$OUT_DIR/$APK_NAME" | grep -q "assets/index.android.bundle"; then
    die "debug APK must not embed a JS bundle (dev support would never fetch from Metro)"
fi
log "done -> $OUT_DIR/$APK_NAME"
