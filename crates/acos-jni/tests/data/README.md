# Test DEX

`hellojni-driver.dex` is compiled from
`tests/java/dev/acos/fixtures/hellojni/MainActivity.java`. It uses the same
package/class name and native declarations as the fixture app so the real
`libhellojni.so` from `fixtures/build/apps/hello-jni.apk` resolves its
`Java_dev_acos_fixtures_hellojni_MainActivity_*` exports, while avoiding the
framework UI classes (`Activity`, `TextView`) that the full app needs.

Regenerate (build tools only; never part of the execution path):

```sh
mkdir -p target/tmp/jni-testdex/classes
javac --release 11 -encoding UTF-8 -d target/tmp/jni-testdex/classes \
    crates/acos-jni/tests/java/dev/acos/fixtures/hellojni/MainActivity.java
"$ANDROID_HOME"/build-tools/36.0.0/d8 --min-api 26 \
    --output target/tmp/jni-testdex \
    target/tmp/jni-testdex/classes/dev/acos/fixtures/hellojni/MainActivity.class
cp target/tmp/jni-testdex/classes.dex crates/acos-jni/tests/data/hellojni-driver.dex
```
