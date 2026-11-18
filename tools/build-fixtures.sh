#!/usr/bin/env bash
#
# Build the ACOS pure-Java DEX fixture corpus.
#
# For every fixtures/dexprogs/<name>/ program this script:
#   1. compiles it with the host JDK (javac --release 11),
#   2. runs the compiled classes on the *host JVM* to capture the reference
#      stdout / exit code (fixtures/expected/dexprogs/*), and
#   3. converts the classes to DEX with the Android SDK build-tools d8
#      (fixtures/build/dexprogs/<name>/classes.dex).
#
# The captured expectations are labelled "jvm-reference" (host OpenJDK 17),
# NOT Android/ART-verified. See fixtures/README.md.
#
# The script is idempotent: fixtures/build/ is recreated per program while
# fixtures/expected/ is regenerated deterministically. fixtures/build/ is
# gitignored; fixtures/expected/ is checked in.
set -euo pipefail

SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BUILD_TOOLS="$SDK/build-tools/$BUILD_TOOLS_VERSION"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
D8="$BUILD_TOOLS/d8"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Run the whole build under the host memory/time guard (ADR-0018).
if [ -z "${ACOS_GUARDED:-}" ]; then
    exec "$ROOT/tools/guarded.sh" --mem 6G --time 1800 -- \
        env ACOS_GUARDED=1 "$0" "$@"
fi

SRC_DIR="$ROOT/fixtures/dexprogs"
BUILD_DIR="$ROOT/fixtures/build/dexprogs"
EXPECTED_DIR="$ROOT/fixtures/expected/dexprogs"
# The reference expectations are **checked in**. They are regenerated in a
# staging directory and copied over only once every program compiled, ran and
# converted; a failure anywhere leaves the tracked files exactly as they were.
# (Before: a javac failure in the middle of the loop still deleted every
# expectation, which is how `28_sqlite_basic`'s checked-in reference kept
# disappearing from working trees.)
STAGE_DIR="$ROOT/fixtures/build/expected-staging"

die() { echo "build-fixtures: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH (need OpenJDK 17)"
command -v java  >/dev/null 2>&1 || die "java not found on PATH (need OpenJDK 17)"
[ -x "$D8" ]           || die "d8 not found or not executable: $D8 (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
[ -f "$ANDROID_JAR" ]  || die "android.jar not found: $ANDROID_JAR"
[ -d "$SRC_DIR" ]      || die "fixture source directory not found: $SRC_DIR"

echo "build-fixtures: SDK=$SDK build-tools=$BUILD_TOOLS_VERSION"
echo "build-fixtures: source=$SRC_DIR"

mkdir -p "$BUILD_DIR" "$EXPECTED_DIR"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

shopt -s nullglob
count=0
for dir in "$SRC_DIR"/*/; do
    name="$(basename "$dir")"
    sources=("$dir"*.java)
    [ "${#sources[@]}" -gt 0 ] || die "no .java sources in $dir"

    bdir="$BUILD_DIR/$name"
    rm -rf "$bdir"
    mkdir -p "$bdir/classes"

    # 1. Compile with the host JDK, targeting a DEX-friendly class file version.
    # `-cp android.jar` is what makes the framework-backed programs
    # (`28_sqlite_basic`: `android.database.sqlite.*`) compile at all; a plain
    # `--release 11` has no android classes on the platform path. This is the
    # same jar `tools/build-apps.sh` already resolves this way, and it only adds
    # the API surface - the pure-Java programs are unaffected. The program still
    # cannot *run* on the host JVM against those classes, which is why its
    # recorded reference is the JVM's own "Unable to initialize main class"
    # answer (exit 1); the real output is compared on ART
    # (`tools/oracle/run-framework-dexprog-art.sh`).
    javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$bdir/classes" \
        "${sources[@]}"

    # 2. Run on the host JVM and capture the reference behaviour.
    set +e
    java -cp "$bdir/classes" Main >"$bdir/run.stdout" 2>"$bdir/run.stderr"
    exit_code=$?
    set -e

    cp "$bdir/run.stdout" "$STAGE_DIR/$name.stdout"
    printf '%s\n' "$exit_code" >"$STAGE_DIR/$name.exit"
    # stderr first line is recorded only for programs that are meant to fail;
    # it is JVM-specific (see fixtures/README.md).
    if [ "$exit_code" -ne 0 ]; then
        head -n 1 "$bdir/run.stderr" >"$STAGE_DIR/$name.stderr" || true
    fi

    # 3. Convert to DEX. No --release flag: default d8 debug mode is fine.
    mapfile -t class_files < <(find "$bdir/classes" -name '*.class' | sort)
    [ "${#class_files[@]}" -gt 0 ] || die "no class files produced for $name"
    "$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$bdir" "${class_files[@]}"

    [ -f "$bdir/classes.dex" ] || die "d8 did not produce classes.dex for $name"

    lines="$(wc -l <"$bdir/run.stdout")"
    printf '  %-28s exit=%-3s stdout_lines=%-4s dex=%s bytes\n' \
        "$name" "$exit_code" "$lines" "$(stat -c '%s' "$bdir/classes.dex")"
    count=$((count + 1))
done

# Every program compiled, ran and produced its DEX: publish the references.
# Only now are the tracked files touched, and only by overwriting them.
cp -f "$STAGE_DIR"/* "$EXPECTED_DIR"/
orphans=()
for existing in "$EXPECTED_DIR"/*; do
    [ -e "$existing" ] || continue
    [ -e "$STAGE_DIR/$(basename "$existing")" ] || orphans+=("$(basename "$existing")")
done
if [ "${#orphans[@]}" -gt 0 ]; then
    echo "build-fixtures: warning: expectations with no fixture source (left untouched, remove by hand if obsolete): ${orphans[*]}" >&2
fi
rm -rf "$STAGE_DIR"

echo "build-fixtures: built $count programs"
echo "build-fixtures: dex   -> $BUILD_DIR/<name>/classes.dex"
echo "build-fixtures: ref   -> $EXPECTED_DIR/<name>.{stdout,exit[,stderr]}"
