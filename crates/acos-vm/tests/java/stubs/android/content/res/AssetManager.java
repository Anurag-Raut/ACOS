package android.content.res;

/**
 * Compile-time stub for the hidden `AssetManager()` constructor and
 * `addAssetPath(String)` (both `@hide` in the public SDK). The stub is only a
 * `--lib` for `javac`/`d8`; the DEX under test references the real framework
 * class, which the VM loads from the boot classpath.
 */
public class AssetManager {
    public AssetManager() {}

    public int addAssetPath(String path) {
        return 0;
    }
}
