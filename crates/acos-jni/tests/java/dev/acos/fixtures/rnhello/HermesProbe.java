// W84-A: harness Java entry for the React Native + Hermes native stack.
//
// This class is compiled to `crates/acos-jni/tests/data/rn-hermes-probe.dex`
// and executed by `crates/acos-jni/tests/rn_hermes.rs` through the real VM
// (`JniRuntime::run`). It drives the *same* Java/native path SoLoader uses:
//
//   1. `System.loadLibrary` for the fixture's x86_64 `.so`s, in dependency
//      order (each one's `JNI_OnLoad` registers its JNI natives), then
//   2. the `com.facebook.react.soloader.OpenSourceMergedSoMapping` merged
//      `lib*_so()` entry points, which run the original per-library
//      `JNI_OnLoad`s that were folded into `libreactnative`/`libhermestooling`
//      (`jni_lib_merge`), then
//   3. the Hermes executor / instance natives that those registrations unlock.
//
// Everything is printed as `ACOS_RN_PROBE {...}` lines so the first failing
// stage survives a crash and the Rust test can parse it.

package dev.acos.fixtures.rnhello;

import com.facebook.react.soloader.OpenSourceMergedSoMapping;

public final class HermesProbe {

  // Implemented by our NDK-built `libhermesprobe.so`: it initialises a Hermes
  // runtime, evaluates a trivial expression and attempts the bundled HBC.
  private static native int acosRnHermesProbe(String bundlePath);

  private static void emit(String json) {
    System.out.println("ACOS_RN_PROBE " + json);
    System.out.flush();
  }

  private static void stage(String phase, String detail) {
    emit("{\"phase\":\"" + phase + "\",\"detail\":\"" + detail + "\"}");
  }

  private static void load(String name) throws Throwable {
    System.loadLibrary(name);
    stage("load", name);
  }

  private static String escape(Throwable t) {
    String m = t.toString();
    return m.replace("\\", "\\\\").replace("\"", "'").replace("\n", " ");
  }

  public static void main(String[] args) {
    emit("{\"phase\":\"begin\"}");
    try {
      // Dependency order matches `libc++_shared` <- `libjsi` <- `libfbjni`
      // <- `libhermes` <- `libreactnative` <- `libhermestooling`.
      for (String n :
          new String[] {"c++_shared", "jsi", "fbjni", "hermes", "reactnative", "hermestooling"}) {
        load(n);
      }
      stage("load", "all");
    } catch (Throwable t) {
      emit("{\"phase\":\"load-error\",\"error\":\"" + escape(t) + "\"}");
      return;
    }

    // The merged per-library onloads. SoLoader calls these from
    // `OpenSourceMergedSoMapping.invokeJniOnload`, which is what turns each
    // folded `JNI_OnLoad` back on (and is idempotent inside the merge wrapper).
    // `invokeJniOnload` names, minus the merged *parent* names (`reactnative`,
    // `hermestooling`): the parent's own `JNI_OnLoad` is the top-level one that
    // `System.loadLibrary` already ran, so the merge section only carries the
    // folded sub-libraries.
    OpenSourceMergedSoMapping m = OpenSourceMergedSoMapping.INSTANCE;
    String[] merged = {
      "fabricjni",
      "jsinspector",
      "mapbufferjni",
      "react_devsupportjni",
      "react_featureflagsjni",
      "react_newarchdefaults",
      "reactnativeblob",
      "reactnativejni",
      "reactnativejni_common",
      "rninstance",
      "turbomodulejsijni",
      "uimanagerjni",
      "yoga",
      "hermes_executor",
      "hermesinstancejni",
      "jsijniprofiler",
    };
    for (String lib : merged) {
      try {
        int rc;
        switch (lib) {
          case "fabricjni":
            rc = m.libfabricjni_so();
            break;
          case "jsinspector":
            rc = m.libjsinspector_so();
            break;
          case "mapbufferjni":
            rc = m.libmapbufferjni_so();
            break;
          case "react_devsupportjni":
            rc = m.libreact_devsupportjni_so();
            break;
          case "react_featureflagsjni":
            rc = m.libreact_featureflagsjni_so();
            break;
          case "react_newarchdefaults":
            rc = m.libreact_newarchdefaults_so();
            break;
          case "reactnativeblob":
            rc = m.libreactnativeblob_so();
            break;
          case "reactnativejni":
            rc = m.libreactnativejni_so();
            break;
          case "reactnativejni_common":
            rc = m.libreactnativejni_common_so();
            break;
          case "rninstance":
            rc = m.librninstance_so();
            break;
          case "turbomodulejsijni":
            rc = m.libturbomodulejsijni_so();
            break;
          case "uimanagerjni":
            rc = m.libuimanagerjni_so();
            break;
          case "yoga":
            rc = m.libyoga_so();
            break;
          case "hermes_executor":
            rc = m.libhermes_executor_so();
            break;
          case "hermesinstancejni":
            rc = m.libhermesinstancejni_so();
            break;
          case "jsijniprofiler":
            rc = m.libjsijniprofiler_so();
            break;
          default:
            rc = -1;
        }
        stage("merged", lib + " rc=" + rc);
      } catch (Throwable t) {
        emit("{\"phase\":\"merged-error\",\"lib\":\"" + lib + "\",\"error\":\"" + escape(t) + "\"}");
      }
    }
    // Hermes runtime init + JS eval through our own confined fixture library.
    try {
      load("hermesprobe");
      String bundle = args.length > 0 ? args[0] : "";
      int rc = acosRnHermesProbe(bundle);
      stage("hermesprobe", "rc=" + rc);
    } catch (Throwable t) {
      emit("{\"phase\":\"hermesprobe-error\",\"error\":\"" + escape(t) + "\"}");
    }
    emit("{\"phase\":\"end\"}");
  }
}
