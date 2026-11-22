#!/usr/bin/env bash
#
# Build fixtures/apps/camerax-probe into fixtures/build/apps/camerax-probe.apk
# without Gradle, using the SHA-256-verified Maven cache under
# `.cache/maven-camerax/` (fetched via `tools/fetch-camerax-deps.sh`).
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

APP_DIR="$ROOT/fixtures/apps/camerax-probe"
BUILD_DIR="$ROOT/fixtures/build/apps"
WORK="$BUILD_DIR/camerax-probe"
OUT="$BUILD_DIR/camerax-probe.apk"
CACHE_DIR="$ROOT/.cache/maven-camerax"
KEYSTORE="$BUILD_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"

die() { echo "build-camerax-probe: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
command -v unzip >/dev/null 2>&1 || die "unzip not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"

# Fail loudly with a helpful message if `.cache/maven-camerax` is missing or invalid.
"$ROOT/tools/fetch-camerax-deps.sh" --verify-only

mkdir -p "$BUILD_DIR"
if [ ! -f "$KEYSTORE" ]; then
    keytool -genkeypair \
        -keystore "$KEYSTORE" \
        -storepass "$KEY_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" \
        -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -deststoretype pkcs12 >/dev/null 2>&1 \
        || die "keytool failed to create $KEYSTORE"
fi

rm -rf "$WORK"
mkdir -p "$WORK"/{compiled,gen,classes,dex,lib/x86_64,stage/libs,stage/res}

echo "build-camerax-probe: staging Maven AARs and JARs"
idx=0
for dep in "$CACHE_DIR"/*; do
    case "$dep" in
        *.aar)
            d="$WORK/stage/${idx}"
            mkdir -p "$d"
            (cd "$d" && unzip -o -q "$dep" 'classes.jar' 'res/*' 'AndroidManifest.xml' 'jni/x86_64/*' 2>/dev/null || true)
            [ -f "$d/classes.jar" ] && cp "$d/classes.jar" "$WORK/stage/libs/${idx}.jar"
            if [ -d "$d/res" ]; then
                rm -rf "$WORK/stage/res/${idx}"
                cp -r "$d/res" "$WORK/stage/res/${idx}"
            fi
            if [ -d "$d/jni/x86_64" ]; then
                cp "$d/jni/x86_64/"*.so "$WORK/lib/x86_64/" 2>/dev/null || true
            fi
            if [ -f "$d/AndroidManifest.xml" ]; then
                pkg="$(grep -oE 'package="[^"]+"' "$d/AndroidManifest.xml" | head -1 | sed 's/package="//;s/"//')"
                [ -n "$pkg" ] && echo "$pkg" >> "$WORK/stage/packages.txt"
            fi
            ;;
        *.jar)
            cp "$dep" "$WORK/stage/libs/${idx}.jar"
            ;;
    esac
    idx=$((idx + 1))
done

echo "build-camerax-probe: compiling resources"
"$AAPT2" compile --dir "$APP_DIR/res" -o "$WORK/compiled/app.zip"
for r in "$WORK/stage/res"/*; do
    [ -d "$r" ] || continue
    "$AAPT2" compile --dir "$r" -o "$WORK/compiled/$(basename "$r").zip"
done

EXTRA_PKGS="$(sort -u "$WORK/stage/packages.txt" 2>/dev/null | grep -E '^[A-Za-z_][A-Za-z0-9_.]*$' | paste -sd: -)"
echo "build-camerax-probe: linking resources + manifest (extra R packages: ${EXTRA_PKGS:-none})"
LINK_ARGS=(
    -o "$WORK/base.apk"
    -I "$ANDROID_JAR"
    --manifest "$APP_DIR/AndroidManifest.xml"
    --java "$WORK/gen"
    --min-sdk-version 26
    --target-sdk-version 34
    --auto-add-overlay
)
[ -n "$EXTRA_PKGS" ] && LINK_ARGS+=(--extra-packages "$EXTRA_PKGS")
for z in "$WORK"/compiled/*.zip; do
    LINK_ARGS+=(-R "$z")
done
"$AAPT2" link "${LINK_ARGS[@]}"

CP="$ANDROID_JAR:$(find "$WORK/stage/libs" -name '*.jar' | sort | paste -sd: -)"
echo "build-camerax-probe: compiling Java"
# shellcheck disable=SC2046
javac --release 11 -encoding UTF-8 -cp "$CP" -d "$WORK/classes" \
    $(find "$APP_DIR/src" "$WORK/gen" -name '*.java')
(cd "$WORK/classes" && jar cf "$WORK/app-classes.jar" .)

echo "build-camerax-probe: merging dependency jars and dexing"
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

"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" \
    "$WORK/app-classes.jar" "$WORK/deps-merged.jar"

EXTRA_ARGS=("$WORK/dex:.")
if ls "$WORK/lib/x86_64/"*.so >/dev/null 2>&1; then
    EXTRA_ARGS+=("$WORK/lib:lib")
fi
python3 "$ROOT/tools/apk_append.py" "$WORK/base.apk" "${EXTRA_ARGS[@]}"

echo "build-camerax-probe: aligning & signing"
rm -f "$OUT" "$WORK/aligned.apk"
"$ZIPALIGN" -f 4 "$WORK/base.apk" "$WORK/aligned.apk"
"$APKSIGNER" sign \
    --ks "$KEYSTORE" \
    --ks-pass "pass:$KEY_PASS" \
    --key-pass "pass:$KEY_PASS" \
    --ks-key-alias "$KEY_ALIAS" \
    --v1-signing-enabled true \
    --v2-signing-enabled true \
    --v3-signing-enabled true \
    --out "$OUT" \
    "$WORK/aligned.apk"

"$APKSIGNER" verify --verbose "$OUT" >/dev/null
echo "build-camerax-probe: -> $OUT ($(stat -c '%s' "$OUT") bytes)"
